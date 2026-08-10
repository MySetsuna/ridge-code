use crate::knowledge::Skill;
use crate::state::{AgentState, Patch, MAX_ERR_STREAK, MAX_EXPLORE, MAX_STALL, MAX_STEPS};
use langgraph::{CompiledGraph, GraphError, StateGraph, END};
use std::convert::Infallible;
use std::sync::Arc;

/// 通用 agent 的基础 system prompt(不再只面向编码)。
pub(crate) const BASE_SYSTEM: &str =
    "You are a capable agent. Use the provided tools to accomplish the \
     user's task. Work in two phases: (1) locate — minimal search/ranged read_file (or codegraph if \
     available) until the root cause or exact edit target is known; (2) act — immediately call \
     edit_file/write_file/apply_edits (or run the verifying command). Do not re-list the repo or \
     re-read whole files once the target is known. Prefer edit_file (surgical, unique-match) over \
     rewriting with write_file. If edit_file fails on unique-match, copy the exact anchor from the \
     last successful read of that path and retry once — do not restart full-repo reconnaissance. \
     Prefer read_file/search over run_shell for reading source. For external/real-time info, \
     web_search to find links then fetch_url to read the actual page — trust the page text, not just \
     the snippet. When there is an objective way to verify (compiler exit code, tests), rely on it \
     and don't trust your own claim. \
     Harness contract: large tool outputs are truncated to a head+tail preview — for detail from a \
     big file use ranged read_file or search, never rely on one giant read. Never delete or empty \
     tests to make a check pass: it is blocked and counts as failure. Record a reusable finding, \
     pitfall or todo with signal_write so the next session inherits it. \
     Boundary contract: before each search/read, identify the one unknown it resolves and prefer the \
     smallest evidence-bearing call. Once enough evidence identifies the target or supports an \
     answer, stop searching: if a change is permitted, take the smallest safe action and verify it; \
     otherwise report the supported answer. If a search/read adds no new fact, do not repeat it; \
     switch to the smallest next action or report the concrete blocker. Do not use a no-op tool call \
     merely to reset exploration counters. \
     Reply concisely: no filler or restating the task; when changing code, emit only the minimal \
     edit (unique-match replace / diff), not a full-file rewrite. When done, stop.";

/// 宿主环境事实块(iter-51):把 OS + **实际可用** shell + 默认 shell 告知模型,令其**自主择**
/// run_shell 的 shell、用对路径/命令语法(修根因:此前 Windows 硬走 cmd,模型发 bash 语法条条失败)。
/// 静态(每机固定)→ 进冻结的 system prompt 首部,利 Claude 缓存。
pub(crate) fn host_env_block() -> String {
    let os = std::env::consts::OS;
    let shells = tools::available_shells().join(", ");
    let default = tools::default_shell();
    let hint = if cfg!(windows) {
        "默认 shell 是 PowerShell(非 bash):`ls -la`/`grep`/`cat`/`head`/`tail`/`&&`/`~/` 等 bash 语法会失败。\
         要么用 PowerShell 写法(`ls`、`Select-String`、`Get-Content`;多命令用 `;` 串联),\
         要么给 run_shell 传 shell:\"bash\"(若上面 available 列了 bash)。路径用 C:\\ 原生形式,勿用 /c/ 式 MSYS 路径。"
    } else {
        "用 POSIX sh 语法;要 bash 特性显式传 shell:\"bash\"。"
    };
    format!(
        "\n\n<host_env>\nos: {os}\nrun_shell 默认 shell: {default};可用: {shells}。\n\
         可给 run_shell 传 shell 字段(cmd|powershell|pwsh|bash|sh)显式选择。\n{hint}\n</host_env>"
    )
}

/// 把宿主环境 + 技能注入 system prompt(知识层 → 大脑偏好)。host_env 恒注入(令模型自主择 shell);
/// 技能有则续附。
#[allow(dead_code)]
pub(crate) fn build_system_prompt(skills: &[Skill]) -> String {
    build_system_prompt_with_mode(skills, false)
}

/// Build the prompt with the runtime tool boundary made explicit.
/// Read-only mode must not inherit the write-oriented locate→act instruction:
/// an inspection task is complete when its evidence-backed answer is ready.
pub(crate) fn build_system_prompt_with_mode(skills: &[Skill], read_only: bool) -> String {
    let mut s = String::from(BASE_SYSTEM);
    s.push_str(&host_env_block());
    if read_only {
        s.push_str(
            "\n\n<runtime_mode>\nread_only: true\nUse only the available read/search tools. \
             This is an inspection task: after enough evidence answers the user's request, \
             stop with concise findings. Hard inspection budget: at most 4 read/search calls \
             total; on the fourth call, stop invoking tools and answer with the strongest \
             supported findings, even if incomplete. State uncertainty instead of seeking more \
             evidence. Do not search for a write/edit target, invent a mutation step, or keep \
             exploring after the answer is supported.\n</runtime_mode>",
        );
    }
    if !skills.is_empty() {
        s.push_str("\n\n# Skills — domain knowledge to apply\n");
        for sk in skills {
            s.push_str(&format!(
                "\n## {} — {}\n{}\n",
                sk.name, sk.description, sk.body
            ));
        }
    }
    s
}

/// 超预算?(0 预算 = 不限)
pub(crate) fn over_budget(s: &AgentState) -> bool {
    s.budget_tokens > 0 && s.total_tokens >= s.budget_tokens
}

/// 陷入僵局?(连续 MAX_STALL 轮工具输出没变)
pub(crate) fn stalled(s: &AgentState) -> bool {
    s.stall >= MAX_STALL
}

/// 熔断?(连续 MAX_ERR_STREAK 轮工具/provider 报错 —— 即便报错内容每轮不同 stall 不触发)
pub(crate) fn circuit_broken(s: &AgentState) -> bool {
    s.err_streak >= MAX_ERR_STREAK
}

/// 纯侦察耗尽?(连续 [`MAX_EXPLORE`] 轮只读/搜索且未落盘改动)。
/// 与 stall 正交:stall 要「输出相同」,本条认「一直在查、从不 edit/write」。
pub(crate) fn explore_exhausted(s: &AgentState) -> bool {
    s.explore_streak >= MAX_EXPLORE
}

/// 纯侦察类内置/MCP 入口(不含 run_shell:测构建/跑命令算干活)。
pub(crate) fn is_explore_tool(name: &str) -> bool {
    matches!(
        name,
        "read_file" | "search" | "web_search" | "fetch_url" | "dispatch_agent"
    ) || name.starts_with("codegraph__")
}

/// 成功后清零 explore_streak 的落盘改写工具。
pub(crate) fn is_land_edit_tool(name: &str) -> bool {
    matches!(name, "write_file" | "edit_file" | "apply_edits")
}

/// 确定性**成功**信号(编码任务:shell `exit 0` 或测试 `passed`)。
/// shell 成功恒以 harness 产出的前缀 `"exit 0:"` 打头 —— 用 `starts_with` 而非 `contains`:
/// ①修正确性 bug(失败命令 `exit 7: ...` 正文若含 "exit 0" 文本会被 `contains` 误判成功);
/// ②堵奖励黑客(模型无法伪造位于**行首**的退出码前缀)。
pub(crate) fn tool_output_ok(o: &str) -> bool {
    o.starts_with("exit 0:") || (o.contains("passed") && !o.contains("failed"))
}

/// 确定性**失败**信号 = 与 [`is_error_observation`](exec) **同一结构化判据**(单一真相,免判据分叉):
/// 非零 `exit N` / 工具操作错误前缀 ` error:`(read/write/edit/shell/mcp/tool error:)/ `BLOCKED` /
/// `permission denied`。verify 的 finish 否决、熔断计数(`err_streak`)、TUI 显红共用之。
///
/// **刻意不认**裸 `error`/`failed` 内容子串:那会把 grep 命中含 "error" 的行、日志正文、`0 failed`/
/// `0 errors` 等**正常输出**误判失败,把模型「已完成的收尾」踢进「否决 → 回 reason → 再收尾」的无尽环
/// (act 不跑 → stall/err_streak 冻结,唯一出口 step_cap,白烧 token)。语义级「测试确实没过」交独立
/// reviewer 判(maker≠checker);内核只认**不可伪造**的结构信号(退出码由 harness 产、位于行首)。
pub fn tool_output_failed(o: &str) -> bool {
    crate::exec::is_error_observation(o)
}

/// verify 判据(通用 agent):
/// - 有确定性成功信号(编码任务)→ 通过;
/// - **模型 finish 且没有失败信号**(开放式/信息类任务,如调 MCP 查数据)→ 接受完成,不空转到回合上限。
///
/// 编码任务仍严格卡 `exit 0`;只对「模型自己收尾且无客观失败」放行,兼顾通用性与 maker≠checker。
pub(crate) fn verify_ok(s: &AgentState) -> bool {
    let out = s.tool_output.as_deref();
    out.is_some_and(tool_output_ok)
        || (s.last_action.as_deref() == Some("finish") && !out.is_some_and(tool_output_failed))
}

/// Deterministic reason shown when the checker rejects a turn. Keep this
/// derived from the same signals as [`verify_ok`] instead of trusting model
/// prose, so users can distinguish a tool failure from an unverified finish.
pub(crate) fn verify_failure_reason(s: &AgentState) -> &'static str {
    if s.tool_output.as_deref().is_some_and(tool_output_failed) {
        "tool output failed"
    } else if s.last_action.as_deref() == Some("finish") {
        "finish lacked deterministic success signal"
    } else {
        "no deterministic success signal"
    }
}

/// 多层独立退出:到回合上限 / 超预算 / 无进展 / 侦察耗尽 / 熔断任一命中,循环都该停(loop engineering:停机是设计的一半)。
/// 全是 O(1) 字段判定;上下文腐烂(需算压缩)不进此热路径,只在终态 [`halt_reason`] 里作诊断重标签。
pub(crate) fn must_stop(s: &AgentState) -> bool {
    s.steps >= MAX_STEPS
        || over_budget(s)
        || stalled(s)
        || explore_exhausted(s)
        || circuit_broken(s)
}

/// reason 之后的路由(scripted / llm 两条路径共用):finish 或需停机 → verify,否则 → act。
pub(crate) fn reason_route(s: &AgentState) -> Vec<String> {
    if must_stop(s) {
        return vec!["verify".to_string()];
    }
    match s.last_action.as_deref() {
        Some("finish") => vec!["verify".to_string()],
        _ => vec!["act".to_string()],
    }
}

/// verify 之后的路由(**scripted 路径**):通过或需停机 → END,否则回 reason。
/// (scripted 图无 `wrapup` 节点、大脑也不会写自然语言总结,故直接 END。)
pub(crate) fn verify_route(s: &AgentState) -> Vec<String> {
    if s.approved || must_stop(s) {
        vec![END.to_string()]
    } else {
        vec!["reason".to_string()]
    }
}

/// verify 之后的路由(**LLM 路径**):通过 → END;否则一律 → `wrapup`(产一段**面向用户的收束陈述**:
/// 为何停 / 已成 / 待办 / 阻塞)再 END。两类未过都收 wrapup:①护栏熔断(超预算/回合上限/无进展/连错);
/// ②模型已自判 `finish` 却未通过验证。
///
/// ②必须收 wrapup 而非回 reason:verify 只在 `finish` 或 must_stop 后到达(见 `reason_route`),
/// 故「未过且非 must_stop」恒是「模型已 finish 却被否」。回 reason 只会让模型见几乎不变的状态**原地再收尾**,
/// 而此环中 `act` 从不跑 → `stall`/`err_streak` 皆冻结、两个熔断器都失灵,唯一出口是 `steps` 撞 `MAX_STEPS`(2000),
/// 白烧 token 空转收尾。收 wrapup:出一段诚实交接后隐式 END,不成环、不伪装成功。
/// (下面 `reason` 分支在真实图中不可达 —— verify 前必为 finish/must_stop —— 保留仅为函数完备与防御。)
pub(crate) fn verify_route_llm(s: &AgentState) -> Vec<String> {
    if s.approved {
        vec![END.to_string()]
    } else if must_stop(s) || s.last_action.as_deref() == Some("finish") {
        vec!["wrapup".to_string()]
    } else {
        vec!["reason".to_string()]
    }
}

/// 决策大脑(maker 的一半):看状态,给出下一步动作名或 `"finish"`。
/// 这是接真实 LLM provider 的接缝 —— 换实现即可,图不用动。
pub trait Brain: Send + Sync + 'static {
    fn decide(&self, state: &AgentState) -> String;
}

/// 离线脚本大脑:按工具反馈推进(写代码 → 修复 → 完成),用于零联网跑通闭环 / 测试。
pub struct ScriptedBrain;

impl Brain for ScriptedBrain {
    fn decide(&self, s: &AgentState) -> String {
        match s.tool_output.as_deref() {
            None => "write_code".to_string(),
            Some(o) if o.contains("failed") => "fix".to_string(),
            Some(o) if o.contains("passed") => "finish".to_string(),
            _ => "finish".to_string(),
        }
    }
}

/// 工具执行器(act 节点用)。真实场景后面换成 MCP / shell / 编译器。
pub type Tool = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// 便捷构造:脚本大脑。
pub fn scripted() -> Arc<dyn Brain> {
    Arc::new(ScriptedBrain)
}

/// 便捷构造:默认离线工具 —— 模拟「写代码先挂、修复后通过」的客观验证信号。
pub fn default_tool() -> Tool {
    Arc::new(|action: &str| match action {
        "write_code" => "tests: 1 failed".to_string(),
        "fix" => "tests: passed".to_string(),
        other => format!("unknown tool `{other}`"),
    })
}

/// 便捷构造:**真实** shell 工具(M1 物理闭环)—— 把 action 当命令跑,返回退出码 + 输出。
/// 这是 act 节点触碰真实世界的接缝。⚠ 无沙箱,只喂受控命令。
pub fn shell_tool() -> Tool {
    Arc::new(|action: &str| match tools::run_shell(action) {
        Ok(r) => format!("exit {}: {}{}", r.code, r.stdout.trim(), r.stderr.trim()),
        Err(e) => format!("shell error: {e}"),
    })
}

/// 把 agent 装配成一张编译好的 langgraph 图。
pub fn build_agent(
    brain: Arc<dyn Brain>,
    tool: Tool,
) -> Result<CompiledGraph<AgentState>, GraphError> {
    let mut g = StateGraph::<AgentState>::new();

    // reason:推进一个回合,问大脑要下一步动作。
    let brain_c = brain.clone();
    g.add_node("reason", move |s: AgentState| {
        let brain = brain_c.clone();
        async move {
            let action = brain.decide(&s);
            let msg = format!("reason#{}: -> {action}", s.steps + 1);
            Ok::<_, Infallible>(Patch::Batch(vec![
                Patch::BumpStep,
                Patch::Message(msg),
                Patch::Action(Some(action)),
            ]))
        }
    });

    // act:执行工具,把客观输出写回状态。
    let tool_c = tool.clone();
    g.add_node("act", move |s: AgentState| {
        let tool = tool_c.clone();
        async move {
            let action = s.last_action.clone().unwrap_or_default();
            let out = (tool.as_ref())(&action);
            Ok::<_, Infallible>(Patch::Batch(vec![
                Patch::Message(format!("act: {action} -> {out}")),
                Patch::ToolOutput(Some(out)),
            ]))
        }
    });

    g.add_node("verify", verify_node);

    g.set_entry("reason");
    g.add_conditional_edge("reason", reason_route);
    g.add_edge("act", "reason"); // 反思环:工具跑完回 reason 复盘
    g.add_conditional_edge("verify", verify_route);

    g.compile()
}

/// verify 节点(scripted / llm 两条路径共用):独立 checker,按 [`verify_ok`] 判定。
pub(crate) async fn verify_node(s: AgentState) -> Result<Patch, Infallible> {
    let ok = verify_ok(&s);
    let patch = if ok {
        Patch::Batch(vec![
            Patch::Approved(true),
            Patch::Message("verify: PASS (deterministic gate)".to_string()),
        ])
    } else {
        let reason = verify_failure_reason(&s);
        Patch::Batch(vec![
            Patch::Approved(false),
            Patch::Issues(vec![reason.to_string()]),
            Patch::Message(format!("verify: FAIL ({reason}) -> back to reason")),
        ])
    };
    Ok(patch)
}

#[cfg(test)]
mod tests {
    use super::{
        build_system_prompt, build_system_prompt_with_mode, explore_exhausted, is_explore_tool,
        is_land_edit_tool, must_stop, tool_output_failed, verify_failure_reason, verify_node,
        AgentState, BASE_SYSTEM,
    };
    use crate::state::MAX_EXPLORE;

    /// 输出端省钱:BASE_SYSTEM 含 Lean-output 约束(简洁作答 + 只出最小编辑)。
    #[test]
    fn base_system_has_lean_output_directive() {
        assert!(BASE_SYSTEM.contains("concisely"));
        assert!(BASE_SYSTEM.contains("minimal edit"));
        // 无技能时系统提示词 = BASE_SYSTEM + host_env 事实块(令模型自主择 shell),不含技能噪声。
        let sys = build_system_prompt(&[]);
        assert!(
            sys.starts_with(BASE_SYSTEM),
            "首部须冻结 BASE_SYSTEM 利缓存"
        );
        assert!(!sys.contains("# Skills"), "无技能不该有技能段");
    }

    #[test]
    fn read_only_prompt_closes_inspection_without_write_target() {
        let sys = build_system_prompt_with_mode(&[], true);
        assert!(sys.contains("read_only: true"));
        assert!(sys.contains("after enough evidence answers"));
        assert!(sys.contains("at most 4 read/search calls"));
        assert!(sys.contains("on the fourth call, stop invoking tools"));
        assert!(sys.contains("Do not search for a write/edit target"));
        assert!(build_system_prompt(&[]).starts_with(BASE_SYSTEM));
        assert!(!build_system_prompt(&[]).contains("read_only: true"));
    }

    /// 模型自主择 shell:host_env 事实块恒注入(OS + 可用/默认 shell),给模型自选的依据。
    #[test]
    fn system_prompt_injects_host_env() {
        let sys = build_system_prompt(&[]);
        assert!(sys.contains("<host_env>"), "应注入 host_env 块");
        assert!(sys.contains(std::env::consts::OS), "应告知 OS");
        assert!(sys.contains("run_shell 默认 shell"), "应告知默认 shell");
        assert!(
            sys.contains(tools::default_shell()),
            "应含宿主默认 shell 名"
        );
    }

    /// 失败判据**结构化**(单一真相,不认裸子串):正常输出含 "error"/"failed" 字样(grep 命中、
    /// 日志、`0 failed`)**不**判失败 —— 免把已完成的收尾误踢进无尽环;结构信号(非零 exit / ` error:` /
    /// BLOCKED / permission)仍判失败,与 [`is_error_observation`] 逐例同源。
    #[test]
    fn tool_output_failed_is_structural_not_substring() {
        // 正常内容含吓人字样 → 不判失败(旧松散判据的误报源、无尽收尾环的触发器)。
        assert!(!tool_output_failed("grep 命中: src/x.rs 处理 error 分支"));
        assert!(!tool_output_failed(
            "build log: 0 errors, 0 failed — all good"
        ));
        assert!(!tool_output_failed("exit 0: ok"));
        // 结构信号仍判失败(不可伪造)。
        assert!(tool_output_failed("exit 1: boom"));
        assert!(tool_output_failed("read error: no such file"));
        assert!(tool_output_failed("BLOCKED (dangerous: rm -rf)"));
        assert!(tool_output_failed("permission denied by user: run_shell"));
        // 与结构化判据逐例同源(免判据分叉)。
        for o in [
            "grep: error 分支",
            "0 errors, 0 failed",
            "exit 0: ok",
            "exit 1: boom",
            "read error: x",
            "BLOCKED xxx",
            "permission denied yyy",
        ] {
            assert_eq!(tool_output_failed(o), crate::exec::is_error_observation(o));
        }
    }

    #[test]
    fn verify_failure_reason_explains_deterministic_boundary() {
        let tool_failed = AgentState {
            last_action: Some("retry".into()),
            tool_output: Some("exit 1: boom".into()),
            ..Default::default()
        };
        assert_eq!(verify_failure_reason(&tool_failed), "tool output failed");

        let unverified_finish = AgentState {
            last_action: Some("finish".into()),
            tool_output: Some("read output".into()),
            ..Default::default()
        };
        assert_eq!(
            verify_failure_reason(&unverified_finish),
            "finish lacked deterministic success signal"
        );

        assert_eq!(
            verify_failure_reason(&AgentState::default()),
            "no deterministic success signal"
        );
    }

    #[tokio::test]
    async fn verify_failure_message_exposes_reason() {
        use langgraph::GraphState;

        let state = AgentState {
            last_action: Some("retry".into()),
            tool_output: Some("read error: missing".into()),
            ..Default::default()
        };
        let mut out = AgentState::default();
        out.apply(verify_node(state).await.unwrap());
        assert_eq!(out.issues, vec!["tool output failed"]);
        assert!(out
            .messages
            .iter()
            .any(|m| m == "verify: FAIL (tool output failed) -> back to reason"));
    }

    /// harness-aware 系统提示词:把 iter-17/19/20 后新成的**物理契约**讲给模型 ——
    /// 输出截断(用 ranged read)、勿删测试(被拦=失败)、signal_write 沉淀复利。
    #[test]
    fn base_system_states_harness_contract() {
        assert!(BASE_SYSTEM.contains("truncated"), "应告知输出被截断");
        assert!(BASE_SYSTEM.contains("ranged read_file"), "应导向分段读");
        assert!(BASE_SYSTEM.contains("delete or empty"), "应禁删/清空测试");
        assert!(BASE_SYSTEM.contains("signal_write"), "应鼓励沉淀复利信号");
        assert!(
            BASE_SYSTEM.contains("two phases") && BASE_SYSTEM.contains("locate"),
            "应要求先定位再动手"
        );
        assert!(
            BASE_SYSTEM.contains("do not restart full-repo"),
            "edit 失败后禁止重启全库侦察"
        );
        assert!(BASE_SYSTEM.contains("Boundary contract"));
        assert!(BASE_SYSTEM.contains("adds no new fact"));
        assert!(BASE_SYSTEM.contains("smallest safe action"));
    }

    #[test]
    fn explore_exhausted_and_must_stop() {
        let ok = AgentState {
            explore_streak: MAX_EXPLORE - 1,
            ..Default::default()
        };
        assert!(!explore_exhausted(&ok));
        assert!(!must_stop(&ok));
        let thrash = AgentState {
            explore_streak: MAX_EXPLORE,
            ..Default::default()
        };
        assert!(explore_exhausted(&thrash));
        assert!(must_stop(&thrash));
        assert!(is_explore_tool("read_file"));
        assert!(is_explore_tool("codegraph__codegraph_explore"));
        assert!(!is_explore_tool("run_shell"));
        assert!(is_land_edit_tool("edit_file"));
    }
}
