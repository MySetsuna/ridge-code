use crate::knowledge::*;
use crate::state::*;
use langgraph::{CompiledGraph, GraphError, StateGraph, END};
use std::convert::Infallible;
use std::sync::Arc;

/// 通用 agent 的基础 system prompt(不再只面向编码)。
pub(crate) const BASE_SYSTEM: &str =
    "You are a capable agent. Use the provided tools to accomplish the \
     user's task. To change existing files, prefer edit_file (surgical, unique-match replace) over \
     rewriting the whole file with write_file; use search and ranged read_file to explore before \
     editing. For external/real-time info, web_search to find links then fetch_url to read the \
     actual page — trust the page text, not just the snippet. When there is an objective way to \
     verify (compiler exit code, tests), rely on it and don't trust your own claim. \
     Harness contract: large tool outputs are truncated to a head+tail preview — for detail from a \
     big file use ranged read_file or search, never rely on one giant read. Never delete or empty \
     tests to make a check pass: it is blocked and counts as failure. Record a reusable finding, \
     pitfall or todo with signal_write so the next session inherits it. \
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
        "路径用 C:\\ 原生形式,勿用 /c/ 式 MSYS 路径;默认非 bash —— 发命令用 PowerShell 语法,或显式传 shell 字段。"
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
pub(crate) fn build_system_prompt(skills: &[Skill]) -> String {
    let mut s = String::from(BASE_SYSTEM);
    s.push_str(&host_env_block());
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

/// 确定性**成功**信号(编码任务:shell `exit 0` 或测试 `passed`)。
/// shell 成功恒以 harness 产出的前缀 `"exit 0:"` 打头 —— 用 `starts_with` 而非 `contains`:
/// ①修正确性 bug(失败命令 `exit 7: ...` 正文若含 "exit 0" 文本会被 `contains` 误判成功);
/// ②堵奖励黑客(模型无法伪造位于**行首**的退出码前缀)。
pub(crate) fn tool_output_ok(o: &str) -> bool {
    o.starts_with("exit 0:") || (o.contains("passed") && !o.contains("failed"))
}

/// 确定性**失败**信号(编译/测试出错、非 0 退出、被拦截/拒绝)。`pub`:除 verify 判据外,
/// TUI `summarize_event` 也复用它把失败观察显红(**单一真相**:显红 ⇔ verify 判失败)。
pub fn tool_output_failed(o: &str) -> bool {
    o.contains("failed")
        || o.contains("error")
        || o.contains("BLOCKED")
        || o.contains("permission denied")
        || (o.starts_with("exit ") && !o.starts_with("exit 0"))
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

/// 多层独立退出:到回合上限 / 超预算 / 无进展 / 熔断任一命中,循环都该停(loop engineering:停机是设计的一半)。
/// 全是 O(1) 字段判定;上下文腐烂(需算压缩)不进此热路径,只在终态 [`halt_reason`] 里作诊断重标签。
pub(crate) fn must_stop(s: &AgentState) -> bool {
    s.steps >= MAX_STEPS || over_budget(s) || stalled(s) || circuit_broken(s)
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

/// verify 之后的路由(共用):通过或需停机 → END,否则回 reason。
pub(crate) fn verify_route(s: &AgentState) -> Vec<String> {
    if s.approved || must_stop(s) {
        vec![END.to_string()]
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
        Patch::Batch(vec![
            Patch::Approved(false),
            Patch::Issues(vec!["build/tests not passing".to_string()]),
            Patch::Message("verify: FAIL -> back to reason".to_string()),
        ])
    };
    Ok(patch)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::*;

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

    /// harness-aware 系统提示词:把 iter-17/19/20 后新成的**物理契约**讲给模型 ——
    /// 输出截断(用 ranged read)、勿删测试(被拦=失败)、signal_write 沉淀复利。
    #[test]
    fn base_system_states_harness_contract() {
        assert!(BASE_SYSTEM.contains("truncated"), "应告知输出被截断");
        assert!(BASE_SYSTEM.contains("ranged read_file"), "应导向分段读");
        assert!(BASE_SYSTEM.contains("delete or empty"), "应禁删/清空测试");
        assert!(BASE_SYSTEM.contains("signal_write"), "应鼓励沉淀复利信号");
    }
}
