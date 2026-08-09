use crate::brain::{circuit_broken, explore_exhausted, over_budget, stalled};
use crate::communication::{
    in_process_exchange, AgentEnvelope, AgentError, AgentHello, AgentMessage, AgentProtocolError,
    AgentResponse, AgentRole, AgentStatus, AgentTask,
};
use crate::context::context_rotted;
use crate::graph::{build_llm_agent, build_llm_agent_read_only};
use crate::knowledge::{provider_failure_label, Agents};
use crate::route::{RouteAudit, RouteRequest, RouteRole};
use crate::state::{AgentState, MAX_STEPS};
use langgraph::GraphError;
use provider::{CompletionRequest, LlmProvider, Message, Role};
use std::sync::Arc;

/// 规划器(M5 起步):让 provider 把一个目标拆成有序子任务(JSON 数组)。
/// 解析失败/模型出错 → **降级**为把整个目标当单个子任务(绝不返回空,循环有活干)。
///
/// 子任务本身可交给 [`build_llm_agent`] 逐个执行;彼此独立的还能靠引擎的 fan-out 并行跑。
async fn plan_attempt(provider: &dyn LlmProvider, task: &str) -> Result<Vec<String>, String> {
    let req = CompletionRequest {
        messages: vec![
            Message::new(
                Role::System,
                "Break the user's goal into 2-5 ordered, concrete subtasks. \
                 Reply ONLY a JSON array of strings, nothing else.",
            ),
            Message::new(Role::User, task.to_string()),
        ],
        tools: vec![],
    };
    let text = provider
        .complete(&req)
        .await
        .map_err(|error| provider_failure_label(error.as_ref()))?
        .text;
    Ok(parse_subtasks(&text).unwrap_or_else(|| vec![task.to_string()]))
}

pub async fn plan(provider: &dyn LlmProvider, task: &str) -> Vec<String> {
    plan_attempt(provider, task)
        .await
        .unwrap_or_else(|_| vec![task.to_string()])
}

struct TeammateOutcome {
    approved: bool,
    steps: usize,
    tokens: usize,
}

async fn run_teammate_via_protocol(
    provider: Arc<dyn LlmProvider>,
    task: &str,
    correlation_id: &str,
) -> Result<TeammateOutcome, GraphError> {
    let request = AgentEnvelope::task(
        format!("{correlation_id}:task"),
        "main",
        "teammate",
        correlation_id,
        AgentTask::new(
            task,
            true,
            vec!["read_file".to_string(), "search".to_string()],
            MAX_STEPS,
        ),
    );
    let response = in_process_exchange(
        AgentHello::guarded("main", AgentRole::Planner),
        AgentHello::read_only("teammate", AgentRole::Worker),
        request,
        |incoming| async move {
            let correlation_id = incoming.correlation_id.clone();
            let parent_id = incoming.message_id.clone();
            let from = incoming.to.clone();
            let to = incoming.from.clone();
            let AgentMessage::Task(payload) = incoming.message else {
                return Err(AgentProtocolError::Invalid(
                    "teammate expected Task".to_string(),
                ));
            };
            let app = build_llm_agent_read_only(provider)
                .map_err(|error| AgentProtocolError::Handler(error.to_string()))?;
            let outcome = app
                .invoke(AgentState::new(payload.task))
                .await
                .map_err(|error| AgentProtocolError::Handler(error.to_string()))?;
            Ok(AgentEnvelope::response(
                format!("{correlation_id}:response"),
                from,
                to,
                correlation_id,
                AgentResponse {
                    status: AgentStatus::Done,
                    approved: outcome.approved,
                    steps: outcome.steps,
                    tokens: outcome.total_tokens,
                    summary: outcome.messages.last().cloned().unwrap_or_default(),
                    modified_files: outcome.modified_files.into_iter().collect(),
                },
            )
            .with_parent(parent_id))
        },
    )
    .await
    .map_err(|error| GraphError::Join(error.to_string()))?;
    match response.message {
        AgentMessage::Response(result) => Ok(TeammateOutcome {
            approved: result.approved,
            steps: result.steps,
            tokens: result.tokens,
        }),
        AgentMessage::Error(AgentError { message, .. }) => Err(GraphError::Join(message)),
        _ => Err(GraphError::Join(
            "teammate returned unexpected message".to_string(),
        )),
    }
}

/// 一轮任务的**停机原因**(loop engineering:让「为什么停」成为机器可判的确定性信号,
/// 而非只知道停了)。`Approved`=确定性验证通过(成功);其余三种是护栏熔断(**响亮失败**):
/// `Budget` 超 token 预算、`Stall` 连续无进展、`StepCap` 到硬回合上限;`Unverified`=模型收尾但未获通过。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HaltReason {
    Approved,
    Budget,
    Stall,
    StepCap,
    /// **约束违反**(奖励黑客):试图删/清空受保护路径(测试)等 —— 被守卫硬拦。
    ConstraintBreach,
    /// **上下文腐烂**:压缩后上下文仍超硬上限(单条巨消息压不掉),继续只烧预算/降智。
    ContextRot,
    /// **熔断**:连续工具/provider 报错达 [`MAX_ERR_STREAK`],无人值守下提前停机防烧预算。
    CircuitBroken,
    Unverified,
}

impl HaltReason {
    pub fn as_str(self) -> &'static str {
        match self {
            HaltReason::Approved => "approved",
            HaltReason::Budget => "budget_exceeded",
            HaltReason::Stall => "no_progress",
            HaltReason::StepCap => "step_cap",
            HaltReason::ConstraintBreach => "constraint_breach",
            HaltReason::ContextRot => "context_rot",
            HaltReason::CircuitBroken => "circuit_broken",
            HaltReason::Unverified => "unverified",
        }
    }
    /// 成功(确定性验证通过)才 true;熔断/违约/未验证都是失败,供调用方给非零退出码。
    pub fn is_success(self) -> bool {
        matches!(self, HaltReason::Approved)
    }
}

/// 据终态判定停机原因。优先级(高→低):成功、超预算(经济护栏最该被看见)、**约束违反**(奖励黑客,
/// 安全须显)、**上下文腐烂**(结构性根因)、**熔断**(连错症状)、无进展(输出停滞)、回合上限(通用耗尽)、未验证。
/// 「更根因/更具体者优先」:同为失败终态时,给最有诊断价值的标签(喂 signal 复利)。
pub fn halt_reason(s: &AgentState) -> HaltReason {
    if s.approved {
        HaltReason::Approved
    } else if over_budget(s) {
        HaltReason::Budget
    } else if s
        .last_error
        .as_deref()
        .is_some_and(|e| e.contains("constraint"))
    {
        HaltReason::ConstraintBreach
    } else if context_rotted(s) {
        HaltReason::ContextRot
    } else if circuit_broken(s) {
        HaltReason::CircuitBroken
    } else if stalled(s) || explore_exhausted(s) {
        // 同标签 no_progress:输出重复 或 纯侦察耗尽(一直查不落盘),用户侧语义都是「没推进」
        HaltReason::Stall
    } else if s.steps >= MAX_STEPS {
        HaltReason::StepCap
    } else {
        HaltReason::Unverified
    }
}

/// 把一轮任务落成**标准存储库**的一条 run:`<run_dir>/manifest.json`(结构化结论:任务/是否通过/
/// 停机原因/步数/token)+ `trace.json`(完整审计轨迹)。相比旧的「cwd 平铺 trace.json 每轮覆盖」,
/// 每 run 独立目录 → 审计历史不再互相冲掉,是 loop engineering 里跨 run 复利的物理底座。
///
/// ponytail: 只落 manifest+trace 这两样**真正被产出**的东西。跨 loop 复利单元 signal 已落地(iter-16),
/// 但存**项目级** `.ridge/signals`(跨 run 共享 → 才复利),非 run 级子目录;溯源靠 signal 的 `source` 字段回指本 run。
pub fn write_run(out: &AgentState, run_dir: impl AsRef<std::path::Path>) -> std::io::Result<()> {
    let dir = run_dir.as_ref();
    let reason = halt_reason(out);
    // 可观测(iter-44):run 收尾一条 info 事件(停机原因/步数/token)。
    tracing::info!(
        reason = %reason.as_str(),
        steps = out.steps,
        tokens = out.total_tokens,
        approved = out.approved,
        "run complete"
    );
    std::fs::create_dir_all(dir)?;
    let manifest = serde_json::json!({
        "task": out.task,
        "approved": out.approved,
        "halt_reason": reason.as_str(),
        "steps": out.steps,
        "tokens": out.total_tokens,
    });
    let json = serde_json::to_string_pretty(&manifest).map_err(std::io::Error::other)?;
    std::fs::write(dir.join("manifest.json"), json)?;
    write_trace(out, dir.join("trace.json"))
}

/// 写一轮的审计轨迹到 `trace.json`(DoD⑥:客观证据,含工具输出/退出码 + 多轮 history)。密钥不入 trace。
pub fn write_trace(out: &AgentState, path: impl AsRef<std::path::Path>) -> std::io::Result<()> {
    let record = serde_json::json!({
        "task": out.task,
        "approved": out.approved,
        "steps": out.steps,
        "tokens": out.total_tokens,
        "trace": out.messages,   // 人读轨迹(含 act 的 exit code / 工具输出)
        "history": out.history,  // 模型面向多轮(含 role=tool 结果)
    });
    let json = serde_json::to_string_pretty(&record).map_err(std::io::Error::other)?;
    std::fs::write(path, json)
}

/// 从模型输出里抠出首个 `[` 到末个 `]` 的 JSON 数组(容忍模型包裹的解释文字)。
fn parse_subtasks(text: &str) -> Option<Vec<String>> {
    let start = text.find('[')?;
    let end = text.rfind(']')?;
    let arr: Vec<String> = serde_json::from_str(text.get(start..=end)?).ok()?;
    (!arr.is_empty()).then_some(arr)
}

/// 一个子任务的执行结果。
#[derive(Clone, Debug)]
pub struct SubtaskResult {
    pub task: String,
    pub approved: bool,
    pub steps: usize,
    pub tokens: usize,
    /// Structured provider/model choice; absent for the legacy fixed-provider API.
    pub route: Option<RouteAudit>,
}

/// 规划-执行的聚合报告。
#[derive(Clone, Debug)]
pub struct PlanReport {
    pub subtasks: Vec<SubtaskResult>,
    /// 全部子任务都通过才算整体通过。
    pub approved: bool,
    pub total_tokens: usize,
    pub total_steps: usize,
    /// Planner choice for the routed API; absent for the legacy fixed-provider API.
    pub planner_route: Option<RouteAudit>,
}

/// **规划 + 执行**(orchestrator-workers,M5 完整版):
/// `planner`(通常是强模型)把目标拆成子任务,`worker` 逐个执行,聚合结果。
/// 成本杠杆:强模型只管规划,弱模型扛执行量(planner ≠ worker)。
///
/// 目前**串行**执行(子任务常有依赖);彼此独立的子任务可改用 `tokio::spawn` + `join_all`
/// 并行(引擎/运行时已支持),这里先要正确性。
pub async fn run_planned(
    planner: Arc<dyn LlmProvider>,
    worker: Arc<dyn LlmProvider>,
    task: &str,
) -> Result<PlanReport, GraphError> {
    let subtasks = plan(planner.as_ref(), task).await;
    let mut results = Vec::with_capacity(subtasks.len());
    let mut total_tokens = 0;
    let mut total_steps = 0;
    let mut approved = true;

    for sub in subtasks {
        let app = build_llm_agent(worker.clone())?;
        let out = app.invoke(AgentState::new(sub.clone())).await?;
        approved &= out.approved;
        total_tokens += out.total_tokens;
        total_steps += out.steps;
        results.push(SubtaskResult {
            task: sub,
            approved: out.approved,
            steps: out.steps,
            tokens: out.total_tokens,
            route: None,
        });
    }

    Ok(PlanReport {
        subtasks: results,
        approved,
        total_tokens,
        total_steps,
        planner_route: None,
    })
}

/// Routed planner/teammate execution. Selection happens before each bounded
/// graph invocation; execution remains the existing serial, verified loop.
pub async fn run_planned_routed(
    agents: &Agents,
    main: Arc<dyn LlmProvider>,
    task: &str,
) -> Result<PlanReport, GraphError> {
    let planner_request = RouteRequest::from_task(task, RouteRole::Planner);
    let planner = agents.select_provider(&planner_request, main.clone());
    let mut planner_route = planner.decision.audit(RouteRole::Planner);
    let subtasks = match plan_attempt(planner.provider.as_ref(), task).await {
        Ok(subtasks) => subtasks,
        Err(first_failure) if planner_route.selected.is_some() => {
            planner_route.used_fallback = true;
            planner_route.reason = format!(
                "{}; selected provider failed ({first_failure}), deterministic main-provider fallback",
                planner_route.reason
            );
            match plan_attempt(main.as_ref(), task).await {
                Ok(subtasks) => subtasks,
                Err(fallback_failure) => {
                    planner_route.reason = format!(
                        "{}; main-provider fallback failed ({fallback_failure}), using original task",
                        planner_route.reason
                    );
                    vec![task.to_string()]
                }
            }
        }
        Err(_) => vec![task.to_string()],
    };
    let mut results = Vec::with_capacity(subtasks.len());
    let mut total_tokens = 0;
    let mut total_steps = 0;
    let mut approved = true;

    for sub in subtasks {
        let worker_request = RouteRequest::from_task(&sub, RouteRole::Teammate);
        let worker = agents.select_provider(&worker_request, main.clone());
        let mut worker_route = worker.decision.audit(RouteRole::Teammate);
        let out = match run_teammate_via_protocol(
            worker.provider.clone(),
            &sub,
            &format!("teammate:{index}", index = results.len()),
        )
        .await
        {
            Ok(out) => out,
            Err(first_failure) if worker_route.selected.is_some() => {
                worker_route.used_fallback = true;
                worker_route.reason = format!(
                    "{}; selected provider failed ({}), deterministic main-provider fallback",
                    worker_route.reason,
                    provider_failure_label(&first_failure)
                );
                run_teammate_via_protocol(
                    main.clone(),
                    &sub,
                    &format!("teammate:{}:fallback", results.len()),
                )
                .await?
            }
            Err(error) => return Err(error),
        };
        approved &= out.approved;
        total_tokens += out.tokens;
        total_steps += out.steps;
        results.push(SubtaskResult {
            task: sub,
            approved: out.approved,
            steps: out.steps,
            tokens: out.tokens,
            route: Some(worker_route),
        });
    }

    Ok(PlanReport {
        subtasks: results,
        approved,
        total_tokens,
        total_steps,
        planner_route: Some(planner_route),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::CONTEXT_ROT_TOKENS;
    use crate::*;
    use provider::ToolCall;

    struct AlwaysFailProvider;

    #[async_trait::async_trait]
    impl LlmProvider for AlwaysFailProvider {
        async fn complete(
            &self,
            _req: &CompletionRequest,
        ) -> Result<provider::Completion, provider::ProviderError> {
            Err("http 503: unavailable-body".into())
        }
    }

    // 一个「永不收工、每轮都调同一个失败命令」的 provider 步骤,带可配的 token 用量。
    fn stuck_step(tokens: u32) -> provider::Completion {
        provider::Completion {
            tool_calls: vec![ToolCall {
                id: "1".to_string(),
                name: "run_shell".to_string(),
                arguments: serde_json::json!({"cmd": "exit 1"}),
            }],
            usage: provider::Usage {
                prompt_tokens: tokens,
                completion_tokens: 0,
            },
            ..Default::default()
        }
    }

    /// 成本护栏:每轮烧 token,预算耗尽即熔断,不跑到回合上限。
    #[tokio::test]
    async fn budget_breaker_stops_before_cap() {
        use provider::ScriptedProvider;
        let provider = ScriptedProvider::new((0..8).map(|_| stuck_step(100)).collect::<Vec<_>>());
        let app = build_llm_agent(Arc::new(provider)).unwrap();
        let out = app
            .invoke(AgentState::new("loop").with_budget(250))
            .await
            .unwrap();

        assert!(!out.approved, "must not fake success");
        assert!(out.total_tokens >= 250, "hit budget: {}", out.total_tokens);
        assert!(
            out.steps < MAX_STEPS,
            "budget熔断应早于回合上限: steps={}",
            out.steps
        );
    }

    /// 无进展检测:工具输出连续 MAX_STALL 轮不变即熔断,不跑到回合上限。
    #[tokio::test]
    async fn no_progress_detection_stops_before_cap() {
        use provider::ScriptedProvider;
        let provider = ScriptedProvider::new((0..8).map(|_| stuck_step(0)).collect::<Vec<_>>());
        let app = build_llm_agent(Arc::new(provider)).unwrap();
        let out = app.invoke(AgentState::new("stuck")).await.unwrap();

        assert!(!out.approved);
        assert!(out.stall >= MAX_STALL, "stall={}", out.stall);
        assert!(
            out.steps < MAX_STEPS,
            "no-progress熔断应早于回合上限: steps={}",
            out.steps
        );
    }

    /// 纯侦察耗尽:每轮 read 不同文件 → stall 不触发,但 explore_streak 触顶后 soft-stop(no_progress),
    /// 不得烧到 MAX_STEPS 后再「重新触发一轮全库侦察」。
    #[tokio::test]
    async fn explore_thrash_stops_before_step_cap() {
        use provider::{Completion, ScriptedProvider, ToolCall};
        let dir = std::env::temp_dir().join(format!("ridge_explore_thrash_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // MAX_EXPLORE 次不同读 + wrapup 补全;多备几条防边界
        let n = MAX_EXPLORE + 4;
        let mut script = Vec::with_capacity(n + 1);
        for i in 0..n {
            let p = dir.join(format!("f{i}.txt"));
            std::fs::write(&p, format!("content-{i}")).unwrap();
            script.push(Completion {
                tool_calls: vec![ToolCall {
                    id: format!("r{i}"),
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": p.to_str().unwrap()}),
                }],
                ..Default::default()
            });
        }
        script.push(Completion {
            text: "已定位,建议下一步写改".into(),
            ..Default::default()
        });
        let app = build_llm_agent(Arc::new(ScriptedProvider::new(script))).unwrap();
        let out = app
            .invoke(AgentState::new("fix the bug then edit"))
            .await
            .unwrap();
        assert!(!out.approved, "只读侦察不得伪造成功");
        assert!(
            out.explore_streak >= MAX_EXPLORE,
            "explore_streak={}",
            out.explore_streak
        );
        assert_eq!(halt_reason(&out), HaltReason::Stall);
        assert!(
            out.steps < MAX_STEPS,
            "侦察熔断应远早于 step_cap: steps={}",
            out.steps
        );
        assert!(
            out.steps <= MAX_EXPLORE + 2,
            "应在触顶后很快 wrapup, steps={}",
            out.steps
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 停机原因分类:据终态确定性判定,优先级 成功 > 预算 > 无进展 > 回合上限 > 未验证。
    #[test]
    fn halt_reason_classifies_each_outcome() {
        let approved = AgentState {
            approved: true,
            ..Default::default()
        };
        assert_eq!(halt_reason(&approved), HaltReason::Approved);
        assert!(halt_reason(&approved).is_success());

        let budget = AgentState {
            budget_tokens: 100,
            total_tokens: 100,
            ..Default::default()
        };
        assert_eq!(halt_reason(&budget), HaltReason::Budget);

        let stall = AgentState {
            stall: MAX_STALL,
            ..Default::default()
        };
        assert_eq!(halt_reason(&stall), HaltReason::Stall);

        // 纯侦察耗尽:输出每轮不同 stall 不触发,但 explore_streak 触顶 → 同 no_progress
        let explore = AgentState {
            explore_streak: MAX_EXPLORE,
            ..Default::default()
        };
        assert_eq!(halt_reason(&explore), HaltReason::Stall);

        let cap = AgentState {
            steps: MAX_STEPS,
            ..Default::default()
        };
        assert_eq!(halt_reason(&cap), HaltReason::StepCap);

        // 约束违反(奖励黑客)由 last_error 分类,优先于回合上限。
        let breach = AgentState {
            steps: MAX_STEPS,
            last_error: Some("BLOCKED (constraint): 拒绝清空受保护路径".into()),
            ..Default::default()
        };
        assert_eq!(halt_reason(&breach), HaltReason::ConstraintBreach);

        // 熔断:连错达阈值 → circuit_broken,优先于回合上限(错误内容每轮不同、stall 不触发时兜底)。
        let circuit = AgentState {
            steps: MAX_STEPS,
            err_streak: MAX_ERR_STREAK,
            ..Default::default()
        };
        assert_eq!(halt_reason(&circuit), HaltReason::CircuitBroken);

        // 上下文腐烂:压缩后单条巨消息仍超硬上限 → context_rot,优先于熔断(结构性根因)。
        let big = "字".repeat(CONTEXT_ROT_TOKENS + 1); // 每 CJK 字≈1tok,单条即超硬上限,压不掉
        let rot = AgentState {
            steps: MAX_STEPS,
            err_streak: MAX_ERR_STREAK,
            history: vec![Message::user(big)],
            ..Default::default()
        };
        assert_eq!(halt_reason(&rot), HaltReason::ContextRot);

        // 熔断/违约都非成功 → 调用方据此给非零退出码。
        for r in [
            HaltReason::Budget,
            HaltReason::Stall,
            HaltReason::StepCap,
            HaltReason::ConstraintBreach,
            HaltReason::ContextRot,
            HaltReason::CircuitBroken,
            HaltReason::Unverified,
        ] {
            assert!(!r.is_success(), "{} 不应算成功", r.as_str());
        }
    }

    /// 熔断早停:连续报错累计到 MAX_ERR_STREAK 即命中 must_stop(早于回合上限)。
    #[test]
    fn circuit_breaks_before_step_cap() {
        let broken = AgentState {
            err_streak: MAX_ERR_STREAK,
            steps: 1, // 远未到回合上限
            ..Default::default()
        };
        assert!(circuit_broken(&broken));
        assert!(must_stop(&broken), "连错达阈值应触发停机,不必跑到回合上限");

        let ok = AgentState {
            err_streak: MAX_ERR_STREAK - 1,
            steps: 1,
            ..Default::default()
        };
        assert!(!must_stop(&ok), "未达连错阈值不应停机");
    }

    /// 标准存储库:一轮任务落成独立 run 目录,含 manifest.json(结构化结论)+ trace.json(完整轨迹)。
    #[test]
    fn write_run_creates_per_run_dir_with_manifest_and_trace() {
        let dir = std::env::temp_dir().join("ridge_write_run_test_1");
        let _ = std::fs::remove_dir_all(&dir); // 清上一次残留,保证干净
        let state = AgentState {
            task: "查天气".into(),
            steps: MAX_STEPS, // 未通过 + 到回合上限 → halt_reason=step_cap
            total_tokens: 42,
            ..Default::default()
        };
        write_run(&state, &dir).unwrap();

        let manifest = dir.join("manifest.json");
        let trace = dir.join("trace.json");
        assert!(manifest.exists(), "manifest.json 应物理生成");
        assert!(trace.exists(), "trace.json 应物理生成");

        let m: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
        assert_eq!(m["task"], "查天气");
        assert_eq!(m["approved"], false);
        assert_eq!(m["halt_reason"], "step_cap");
        assert_eq!(m["steps"], MAX_STEPS);
        assert_eq!(m["tokens"], 42);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// M5:规划器把目标拆成子任务(容忍模型包裹的解释文字)。
    #[tokio::test]
    async fn planner_decomposes_goal_into_subtasks() {
        use provider::{Completion, ScriptedProvider};
        let provider = ScriptedProvider::new(vec![Completion {
            text: r#"Sure! ["add fn", "write test", "run cargo test"]"#.to_string(),
            ..Default::default()
        }]);
        let subs = plan(&provider, "implement add").await;
        assert_eq!(subs, vec!["add fn", "write test", "run cargo test"]);
    }

    /// M5:模型没给出可解析的数组 → 降级为单个子任务(绝不返回空)。
    #[tokio::test]
    async fn planner_falls_back_when_unparseable() {
        use provider::{Completion, ScriptedProvider};
        let provider = ScriptedProvider::new(vec![Completion {
            text: "I'm not sure how to break this down".to_string(),
            ..Default::default()
        }]);
        let subs = plan(&provider, "do the thing").await;
        assert_eq!(subs, vec!["do the thing"]);
    }

    /// M5 完整:planner 拆 2 个子任务 → worker 逐个执行到 approved → 聚合整体通过。
    #[tokio::test]
    async fn orchestrator_plans_and_runs_subtasks() {
        use provider::{Completion, ScriptedProvider};

        let planner = ScriptedProvider::new(vec![Completion {
            text: r#"["impl add", "test add"]"#.to_string(),
            ..Default::default()
        }]);
        // worker 被两个子任务共享(串行):每个子任务耗 [跑 exit 0, 收尾] 两个补全。
        let step_pass = || Completion {
            tool_calls: vec![ToolCall {
                id: "1".to_string(),
                name: "run_shell".to_string(),
                arguments: serde_json::json!({"cmd": "exit 0"}),
            }],
            ..Default::default()
        };
        let step_done = || Completion {
            text: "done".to_string(),
            ..Default::default()
        };
        let worker =
            ScriptedProvider::new(vec![step_pass(), step_done(), step_pass(), step_done()]);

        let report = run_planned(
            Arc::new(planner),
            Arc::new(worker),
            "implement add with test",
        )
        .await
        .unwrap();

        assert_eq!(report.subtasks.len(), 2);
        assert!(report.approved, "两个子任务都应通过");
        assert!(report.subtasks.iter().all(|s| s.approved));
        assert_eq!(
            report
                .subtasks
                .iter()
                .map(|s| s.task.as_str())
                .collect::<Vec<_>>(),
            vec!["impl add", "test add"]
        );
    }

    #[tokio::test]
    async fn routed_orchestrator_audits_planner_and_teammate_choices() {
        use provider::{Completion, ScriptedProvider};

        let planner: Arc<dyn LlmProvider> = Arc::new(ScriptedProvider::new(vec![Completion {
            text: r#"["inspect"]"#.into(),
            ..Default::default()
        }]));
        let worker: Arc<dyn LlmProvider> = Arc::new(ScriptedProvider::new(vec![Completion {
            text: "done".into(),
            ..Default::default()
        }]));
        let planner_profile = ModelProfile {
            provider: "deep".into(),
            model: "planner".into(),
            kind: "openai".into(),
            context_window: Some(64_000),
            cost_tier: Some(3),
            latency_tier: Some(3),
            supports_tools: Some(false),
            supports_reasoning: Some(true),
            tags: vec!["planning".into()],
        };
        let worker_profile = ModelProfile {
            provider: "fast".into(),
            model: "worker".into(),
            kind: "openai".into(),
            context_window: Some(64_000),
            cost_tier: Some(1),
            latency_tier: Some(1),
            supports_tools: Some(true),
            supports_reasoning: Some(false),
            tags: vec!["execution".into()],
        };
        let agents = Agents {
            defs: Vec::new(),
            providers: std::collections::HashMap::new(),
            route_candidates: vec![
                AgentProvider {
                    profile: planner_profile,
                    provider: planner.clone(),
                },
                AgentProvider {
                    profile: worker_profile,
                    provider: worker.clone(),
                },
            ],
        };

        let report = run_planned_routed(&agents, planner, "design a complex architecture")
            .await
            .unwrap();
        assert_eq!(
            report
                .planner_route
                .as_ref()
                .and_then(|route| route.selected.as_deref()),
            Some("deep::planner")
        );
        assert_eq!(report.subtasks.len(), 1);
        assert_eq!(
            report.subtasks[0]
                .route
                .as_ref()
                .and_then(|route| route.selected.as_deref()),
            Some("fast::worker")
        );
        assert!(report.approved);
        assert!(report.subtasks[0]
            .route
            .as_ref()
            .is_some_and(|route| route.reason.contains("role=teammate")));
    }

    #[tokio::test]
    async fn routed_orchestrator_falls_back_after_planner_and_teammate_failures() {
        use provider::{Completion, ScriptedProvider};

        let main: Arc<dyn LlmProvider> = Arc::new(ScriptedProvider::new(vec![
            Completion {
                text: r#"["inspect"]"#.into(),
                ..Default::default()
            },
            Completion {
                text: "done".into(),
                ..Default::default()
            },
        ]));
        let failing: Arc<dyn LlmProvider> = Arc::new(AlwaysFailProvider);
        let profile = ModelProfile {
            provider: "limited".into(),
            model: "unavailable".into(),
            kind: "openai".into(),
            context_window: Some(64_000),
            cost_tier: Some(1),
            latency_tier: Some(1),
            supports_tools: Some(true),
            supports_reasoning: Some(true),
            tags: vec![],
        };
        let agents = Agents {
            defs: Vec::new(),
            providers: std::collections::HashMap::new(),
            route_candidates: vec![AgentProvider {
                profile,
                provider: failing,
            }],
        };

        let report = run_planned_routed(&agents, main.clone(), "design a complex architecture")
            .await
            .unwrap();
        assert!(report.approved);
        assert!(report
            .planner_route
            .as_ref()
            .is_some_and(|route| route.used_fallback
                && route.reason.contains("http 503")
                && route.reason.contains("main-provider fallback")));
        assert!(report.subtasks[0].route.as_ref().is_some_and(
            |route| route.used_fallback && route.reason.contains("main-provider fallback")
        ));
    }
}
