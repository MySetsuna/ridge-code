//! rc-core — 编排大脑(M2)。详见 PLAN.md §2。
//!
//! 流水线:Planner(强)分解为子任务 → Router 按难度分流强/弱 → Worker 执行
//! → 集成验证 + 失败修复(强) → Reviewer(强)评审 → 产出结果与成本账单。
//!
//! M2 简化(后续里程碑补全):
//! - 子任务按规划「顺序串行」执行(deps 已记录,留待并行 + git worktree 隔离);
//! - 编辑工具仍是整文件覆盖(留待结构化 patch 工具)。

use anyhow::{anyhow, bail, Result};
use rc_mcp::McpHub;
use rc_providers::LlmProvider;
use rc_tools::{dispatch, tool_specs};
use rc_types::{
    Cost, Diagnostic, Difficulty, Message, ModelTier, ReviewResult, Subtask, ToolCall, ToolSpec,
    Verdict,
};
use rc_verify::{resolve_plan, verify, VerifyPlan};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{debug, info, warn};

/// 编排参数。
pub struct OrchestratorConfig {
    /// 单次 agent 运行内的工具循环上限。
    pub max_steps: usize,
    /// 验证失败的最多修复轮数。
    pub max_repairs: usize,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            max_steps: 12,
            max_repairs: 3,
        }
    }
}

/// 一次编排的产出。
pub struct Outcome {
    pub subtasks: usize,
    pub repairs: usize,
    pub reviewed: bool,
    pub approved: bool,
    pub cost: Cost,
}

/// 编排器:持有强/弱两个 provider,以及工具、验证计划、工作目录。
pub struct Orchestrator {
    strong: Box<dyn LlmProvider>,
    weak: Box<dyn LlmProvider>,
    tools: Vec<ToolSpec>,
    read_tools: Vec<ToolSpec>,
    /// 可选的外部 MCP 工具(M4);None 时行为与 M3 完全一致。
    mcp: Option<McpHub>,
    /// 可选:按难度覆盖 worker 的 provider(命名注册表 + N 模型路由)。
    /// 命中则用它,否则回落 strong/weak;成本档位仍按难度记(见 `work`)。
    worker_models: HashMap<Difficulty, Box<dyn LlmProvider>>,
    verify_plan: VerifyPlan,
    project_dir: PathBuf,
    cfg: OrchestratorConfig,
}

impl Orchestrator {
    pub fn new(
        strong: Box<dyn LlmProvider>,
        weak: Box<dyn LlmProvider>,
        project_dir: PathBuf,
        cfg: OrchestratorConfig,
    ) -> Self {
        let tools = tool_specs();
        let read_tools = tools
            .iter()
            .filter(|t| matches!(t.name.as_str(), "read_file" | "list_dir"))
            .cloned()
            .collect();
        let verify_plan = resolve_plan(&project_dir);
        Self {
            strong,
            weak,
            tools,
            read_tools,
            mcp: None,
            worker_models: HashMap::new(),
            verify_plan,
            project_dir,
            cfg,
        }
    }

    /// 注入按难度覆盖的 worker provider(命名注册表 + N 模型路由)。
    /// 对应难度的子任务会用覆盖模型执行;未覆盖的难度回落 strong/weak。返回 self 以链式调用。
    pub fn with_worker_models(mut self, models: HashMap<Difficulty, Box<dyn LlmProvider>>) -> Self {
        self.worker_models = models;
        self
    }

    /// 注入外部 MCP 工具:把它们并入 Worker/修复/基线的工具集(不进 Reviewer 的只读工具集,
    /// 避免评审触发副作用工具)。返回 self 以链式调用。
    pub fn with_mcp(mut self, hub: McpHub) -> Self {
        self.tools.extend(hub.tool_specs().iter().cloned());
        self.mcp = Some(hub);
        self
    }

    /// 优雅关闭:若持有 MCP,关闭其全部子进程会话。运行结束后调用。
    pub async fn shutdown(self) {
        if let Some(hub) = self.mcp {
            hub.shutdown().await;
        }
    }

    pub async fn run(&self, task: &str) -> Result<Outcome> {
        let mut cost = Cost::default();

        info!("① 规划(强模型分解任务)");
        let subtasks = self.plan(task, &mut cost).await?;
        for st in &subtasks {
            info!(id = %st.id, difficulty = ?st.difficulty, "  子任务: {}", st.description);
        }

        info!("② 执行子任务(按难度路由强/弱)");
        for st in &subtasks {
            let tier = route_tier(st.difficulty);
            info!(id = %st.id, tier = ?tier, "  执行子任务");
            self.work(st, tier, &mut cost).await?;
        }

        info!("③ 集成验证 + 失败修复(强)");
        let repairs = self.verify_and_repair(&mut cost).await?;

        info!("④ 评审(强)");
        let mut reviewed = false;
        let mut approved = true;
        match self.review(task, &mut cost).await {
            Some(r) => {
                reviewed = true;
                approved = r.approved;
                if !r.approved && !r.issues.is_empty() {
                    warn!(issues = ?r.issues, "  评审未通过,修复一轮");
                    self.fix_from_review(&r, &mut cost).await?;
                    let _ = self.verify_and_repair(&mut cost).await?;
                    approved = true; // 已据评审修复
                } else {
                    info!("  ✅ 评审通过");
                }
            }
            None => warn!("  评审结果无法解析,跳过(按通过处理)"),
        }

        Ok(Outcome {
            subtasks: subtasks.len(),
            repairs,
            reviewed,
            approved,
            cost,
        })
    }

    /// 基线:全程强模型单 agent —— 不分解/不路由/不评审,直接工具循环 + 验证修复。
    pub async fn run_single(&self, task: &str) -> Result<Outcome> {
        let mut cost = Cost::default();
        info!("基线:全程强模型单 agent 直跑");
        let user =
            format!("请完成以下编码任务,写的代码应能通过编译。完成后用一句中文总结并停止:\n{task}");
        let mut msgs = vec![Message::system(WORKER_SYSTEM), Message::user(user)];
        run_agent(
            self.strong.as_ref(),
            ModelTier::Strong,
            &mut msgs,
            &self.tools,
            self.mcp.as_ref(),
            self.cfg.max_steps,
            &mut cost,
        )
        .await?;
        let repairs = self.verify_and_repair(&mut cost).await?;
        Ok(Outcome {
            subtasks: 1,
            repairs,
            reviewed: false,
            approved: true,
            cost,
        })
    }

    /// Planner:强模型把任务分解成有序子任务(JSON);解析失败则降级为单个 hard 子任务。
    async fn plan(&self, task: &str, cost: &mut Cost) -> Result<Vec<Subtask>> {
        let sys = "你是任务规划器。把用户的编码任务分解成 2 到 5 个有序子任务。\
只输出一个 JSON 数组,不要任何解释或 markdown 代码块。\
每个元素形如 {\"id\":\"s1\",\"description\":\"具体要做什么\",\"deps\":[],\"difficulty\":\"trivial|moderate|hard\"}。\
difficulty 用小写,表示该子任务难度。";
        let msgs = vec![Message::system(sys), Message::user(task)];
        for attempt in 1..=2 {
            let c = self.strong.complete(&msgs, &[]).await?;
            cost.add(
                ModelTier::Strong,
                c.usage.input_tokens,
                c.usage.output_tokens,
            );
            if let Some(subs) = parse_plan(&c.message.content) {
                return Ok(subs);
            }
            warn!(attempt, "规划输出无法解析为 JSON,降级处理");
        }
        Ok(vec![Subtask {
            id: "s1".into(),
            description: task.to_string(),
            deps: vec![],
            difficulty: Difficulty::Hard,
        }])
    }

    /// Worker:用路由到的 provider 执行一个子任务(共享文件系统)。
    /// provider 选取:按难度覆盖(worker_models)命中优先,否则回落 tier(strong/weak)。
    /// **成本档位仍是 tier**(= route_tier(difficulty)),与用哪个命名模型无关,保 eval 语义。
    async fn work(&self, st: &Subtask, tier: ModelTier, cost: &mut Cost) -> Result<()> {
        let user = format!(
            "这是整体编码任务的一个子任务(id={})。\n子任务:{}\n\
请用工具读写文件 / 执行命令来完成它;写的代码应能通过编译。完成后用一句中文总结并停止。",
            st.id, st.description
        );
        let provider = match self.worker_models.get(&st.difficulty) {
            Some(b) => b.as_ref(),
            None => self.provider_for(tier),
        };
        let mut msgs = vec![Message::system(WORKER_SYSTEM), Message::user(user)];
        run_agent(
            provider,
            tier,
            &mut msgs,
            &self.tools,
            self.mcp.as_ref(),
            self.cfg.max_steps,
            cost,
        )
        .await?;
        Ok(())
    }

    /// 验证 + 失败修复循环(修复一律用强模型)。
    async fn verify_and_repair(&self, cost: &mut Cost) -> Result<usize> {
        let mut repairs = 0usize;
        loop {
            match verify(&self.verify_plan, &self.project_dir).await? {
                Verdict::Pass => {
                    info!("  ✅ 验证通过");
                    return Ok(repairs);
                }
                Verdict::Uncertain { note } => {
                    warn!(%note, "  ⚠️ 无法客观验证,按完成处理");
                    return Ok(repairs);
                }
                Verdict::Fail { reasons } => {
                    if repairs >= self.cfg.max_repairs {
                        bail!(
                            "修复 {repairs} 轮后仍未通过验证。最后失败:\n{}",
                            render_reasons(&reasons)
                        );
                    }
                    repairs += 1;
                    warn!(round = repairs, "  ❌ 验证失败,强模型修复");
                    let feedback = format!(
                        "代码未通过验证,请据以下输出直接修改代码修复,然后停止:\n\n{}",
                        render_reasons(&reasons)
                    );
                    let mut msgs = vec![Message::system(WORKER_SYSTEM), Message::user(feedback)];
                    run_agent(
                        self.strong.as_ref(),
                        ModelTier::Strong,
                        &mut msgs,
                        &self.tools,
                        self.mcp.as_ref(),
                        self.cfg.max_steps,
                        cost,
                    )
                    .await?;
                }
            }
        }
    }

    /// Reviewer:强模型(只读工具)评审实现是否满足任务,返回结构化结论。
    async fn review(&self, task: &str, cost: &mut Cost) -> Option<ReviewResult> {
        let sys = "你是代码评审器。用 read_file / list_dir 查看当前实现,判断它是否正确完成了用户任务。\
最后只输出一个 JSON 对象:{\"approved\":true 或 false,\"issues\":[\"问题\"]}。approved 为 true 时 issues 可为空。";
        let user = format!("原始任务:{task}\n请评审当前代码实现是否满足该任务。");
        let mut msgs = vec![Message::system(sys), Message::user(user)];
        // 评审只用内置只读工具(read_file/list_dir),不接 MCP —— 传 None。
        match run_agent(
            self.strong.as_ref(),
            ModelTier::Strong,
            &mut msgs,
            &self.read_tools,
            None,
            self.cfg.max_steps,
            cost,
        )
        .await
        {
            Ok(content) => parse_review(&content),
            Err(e) => {
                warn!(error = %e, "评审执行失败");
                None
            }
        }
    }

    async fn fix_from_review(&self, review: &ReviewResult, cost: &mut Cost) -> Result<()> {
        let issues = review.issues.join("\n- ");
        let feedback = format!("评审发现以下问题,请直接修改代码修复后停止:\n- {issues}");
        let mut msgs = vec![Message::system(WORKER_SYSTEM), Message::user(feedback)];
        run_agent(
            self.strong.as_ref(),
            ModelTier::Strong,
            &mut msgs,
            &self.tools,
            self.mcp.as_ref(),
            self.cfg.max_steps,
            cost,
        )
        .await?;
        Ok(())
    }

    fn provider_for(&self, tier: ModelTier) -> &dyn LlmProvider {
        match tier {
            ModelTier::Strong => self.strong.as_ref(),
            ModelTier::Weak => self.weak.as_ref(),
        }
    }
}

fn route_tier(d: Difficulty) -> ModelTier {
    match d {
        Difficulty::Hard => ModelTier::Strong,
        _ => ModelTier::Weak,
    }
}

const WORKER_SYSTEM: &str =
    "你是 ridge-code 的执行器。你能调用工具读写文件、列目录、执行 shell 来完成编码任务。\
请先用 list_dir / read_file 了解上下文,再用 write_file / run_shell 实施改动;\
注意 write_file 是整文件覆盖,务必先读出并保留需要保留的已有内容。\
你写的代码应能通过编译。完成后用一句中文总结并停止调用工具。";

/// 跑一轮 agent:工具循环直到模型不再调用工具,返回最终文本;沿途按档位累计成本。
/// `mcp` 存在时,工具调用先按名路由到 MCP,否则落内置工具(见 `dispatch_tool`)。
async fn run_agent(
    provider: &dyn LlmProvider,
    tier: ModelTier,
    messages: &mut Vec<Message>,
    tools: &[ToolSpec],
    mcp: Option<&McpHub>,
    max_steps: usize,
    cost: &mut Cost,
) -> Result<String> {
    for step in 1..=max_steps {
        let completion = provider.complete(messages.as_slice(), tools).await?;
        cost.add(
            tier,
            completion.usage.input_tokens,
            completion.usage.output_tokens,
        );
        debug!(step, tier = ?tier, in_tok = completion.usage.input_tokens, out_tok = completion.usage.output_tokens, "模型回复");
        let msg = completion.message;
        if msg.tool_calls.is_empty() {
            return Ok(msg.content);
        }
        messages.push(msg.clone());
        for call in &msg.tool_calls {
            info!(step, tool = %call.name, "    工具");
            let result = dispatch_tool(mcp, call).await;
            messages.push(Message::tool_result(call.id.clone(), result));
        }
    }
    Err(anyhow!("达到最大轮数 {max_steps} 仍未给出最终答复"))
}

/// 工具分派:名字命中已连接的 MCP 工具则走 MCP,否则落内置工具。
/// 与内置 `dispatch` 一致,任何错误都转成给模型看的文本(让它自我纠正),不向上抛。
async fn dispatch_tool(mcp: Option<&McpHub>, call: &ToolCall) -> String {
    if let Some(hub) = mcp {
        if hub.has_tool(&call.name) {
            return match hub.call(call).await {
                Ok(out) => out,
                Err(e) => format!("ERROR: {e:#}"),
            };
        }
    }
    dispatch(call).await
}

fn render_reasons(reasons: &[Diagnostic]) -> String {
    reasons
        .iter()
        .map(|d| format!("## [{}] 失败\n{}", d.source, d.detail))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// 取首个 open 到末个 close 之间的子串(用于从模型回复里抠出 JSON)。
fn extract_between(s: &str, open: char, close: char) -> Option<&str> {
    let start = s.find(open)?;
    let end = s.rfind(close)?;
    if end > start {
        Some(&s[start..=end])
    } else {
        None
    }
}

fn parse_plan(content: &str) -> Option<Vec<Subtask>> {
    let json = extract_between(content, '[', ']')?;
    serde_json::from_str::<Vec<Subtask>>(json)
        .ok()
        .filter(|v| !v.is_empty())
}

fn parse_review(content: &str) -> Option<ReviewResult> {
    let json = extract_between(content, '{', '}')?;
    serde_json::from_str::<ReviewResult>(json).ok()
}

#[cfg(test)]
mod run_single_tests {
    use super::*;
    use rc_providers::{LlmProvider, StubProvider};
    use rc_types::{Completion, Message, Role, Usage};

    #[tokio::test]
    async fn run_single_is_single_subtask_no_review() {
        let done = Completion {
            message: Message {
                role: Role::Assistant,
                content: "done".into(),
                tool_calls: vec![],
                tool_call_id: None,
            },
            usage: Usage {
                input_tokens: 5,
                output_tokens: 5,
            },
        };
        let strong: Box<dyn LlmProvider> = Box::new(StubProvider::new("s", vec![done]));
        let weak: Box<dyn LlmProvider> = Box::new(StubProvider::new("w", vec![]));
        // 空临时目录:无 Cargo.toml/ridge.toml → 验证 Uncertain → 视为通过。
        let tmp = std::env::temp_dir().join("rc-core-run-single-test");
        std::fs::create_dir_all(&tmp).unwrap();

        let orch = Orchestrator::new(strong, weak, tmp, OrchestratorConfig::default());
        let out = orch.run_single("加一个函数").await.unwrap();

        assert_eq!(out.subtasks, 1);
        assert!(!out.reviewed);
        assert!(out.approved);
        assert!(out.cost.strong_tokens() > 0);
    }

    fn scripted(in_tok: u32) -> Completion {
        Completion {
            message: Message {
                role: Role::Assistant,
                content: "done".into(),
                tool_calls: vec![],
                tool_call_id: None,
            },
            usage: Usage {
                input_tokens: in_tok,
                output_tokens: 0,
            },
        }
    }

    #[tokio::test]
    async fn with_worker_models_overrides_provider_but_keeps_tier_cost() {
        use std::collections::HashMap;
        // 覆盖模型 usage=100、strong=7:用哪个 provider 由消耗到的 usage 区分。
        let strong: Box<dyn LlmProvider> = Box::new(StubProvider::new("s", vec![scripted(7)]));
        let weak: Box<dyn LlmProvider> = Box::new(StubProvider::new("w", vec![]));
        let hard_model: Box<dyn LlmProvider> =
            Box::new(StubProvider::new("hard", vec![scripted(100)]));

        let tmp = std::env::temp_dir().join("rc-core-worker-models-test");
        std::fs::create_dir_all(&tmp).unwrap();

        let mut models: HashMap<Difficulty, Box<dyn LlmProvider>> = HashMap::new();
        models.insert(Difficulty::Hard, hard_model);
        let orch = Orchestrator::new(strong, weak, tmp, OrchestratorConfig::default())
            .with_worker_models(models);

        let st = Subtask {
            id: "s1".into(),
            description: "x".into(),
            deps: vec![],
            difficulty: Difficulty::Hard,
        };
        let mut cost = Cost::default();
        orch.work(&st, ModelTier::Strong, &mut cost).await.unwrap();

        // 用的是覆盖模型(usage 100,而非 strong 的 7),且成本仍记在 strong tier。
        assert_eq!(cost.strong_in, 100);
        assert_eq!(cost.weak_tokens(), 0);
    }
}
