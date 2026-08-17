use crate::state::{AgentState, EXPLORE_NUDGE_AFTER, MAX_DISPATCH_BATCHES};
use provider::{repair_tool_history, Message, Role};

/// 把当前状态铺成给 provider 的消息序列:system(含注入的技能)+ **真实多轮 history**
/// (user / assistant(带 tool_calls) / role=tool 结果),而非把轨迹当 assistant 文本糊上去。
/// history 的**估算 token** 总量超过这么多,发给 LLM 前**自动** compact —— 把 O(n) 全量历史
/// 收敛成有界快照(Runtime State:模型只需知「现在什么情况」,不需知全部「聊过什么」)。此前压缩仅
/// `/compact` 手动;长任务多轮下历史随步数膨胀、爆预算 + 击穿 prompt 缓存。
/// 按**内容体量**触发(而非条数):一条万字日志 ≫ 二十条短问答,条数触发会漏。
/// ponytail: [`est_tokens`] 是本地启发式估算,真实计数(tiktoken)是外置能力不进内核;阈值是可调校准旋钮。
/// 注意:加权触发改善「多条中等消息」的判准;「少数超大单条消息」仍需**单条内容截断**(属外置 squeez 域)。
const AUTO_COMPACT_TOKENS: usize = 6000;
/// 自动 compact 时保留的最近消息条数。
const AUTO_COMPACT_KEEP: usize = 8;
/// **上下文腐烂**硬上限:压缩后估算 token 仍超此值(2× 压缩阈值)= 单条巨消息压不掉,
/// 继续只会烧预算/降智 → 停机(诊断标签,喂 signal 复利)。
pub(crate) const CONTEXT_ROT_TOKENS: usize = 2 * AUTO_COMPACT_TOKENS;

/// 单条工具观察的字符上限(超则截断)。取值宽,使**仅病态巨型**输出被截,常规输出零影响。
/// head+tail 各半,合计即上限。
const OBS_CHAR_CAP: usize = 8000;

/// 上下文卫生(根因):巨型工具观察入 `history` 前**确定性截断**为 head+tail 预览 + 中缝标记 ——
/// 补 `compact_history`(压多条旧消息)压不掉「单条近消息」的缺口。纯函数、**零丢数据**
/// (磁盘文件不动,可 `read_file` 区间重取)。截断标记刻意避开 verify/durable 判据词
/// (error/failed/exit/BLOCKED/permission),免污染成功/失败/错误信号。
pub(crate) fn bound_observation(obs: String) -> String {
    let total = obs.chars().count();
    if total <= OBS_CHAR_CAP {
        return obs;
    }
    const HEAD: usize = OBS_CHAR_CAP / 2;
    const TAIL: usize = OBS_CHAR_CAP - HEAD;
    let head: String = obs.chars().take(HEAD).collect();
    let tail: String = obs.chars().skip(total - TAIL).collect();
    let dropped = total - HEAD - TAIL;
    format!("{head}\n\n…[截断 {dropped} 字符;完整内容已存盘,可 read_file 指定区间重取]…\n\n{tail}")
}

/// 本地 token 估算(不引 tiktoken):CJK 等非 ASCII 字 ≈ 1 token/字,ASCII ≈ 1 token/4 字符。
/// 口径同仓内 `bin`/`token-count.mjs`。粗但零依赖、确定可测 —— 只用于「要不要压缩」的触发判断。
pub fn est_tokens(text: &str) -> usize {
    let (mut cjk, mut ascii) = (0usize, 0usize);
    for c in text.chars() {
        if c.is_ascii() {
            ascii += 1;
        } else {
            cjk += 1;
        }
    }
    cjk + ascii / 4
}

pub(crate) fn to_messages(system: &str, s: &AgentState) -> Vec<Message> {
    let weight: usize = s.history.iter().map(|m| est_tokens(&m.content)).sum();
    let history = if weight > AUTO_COMPACT_TOKENS {
        compact_history(s.history.clone(), AUTO_COMPACT_KEEP)
    } else {
        s.history.clone()
    };
    let mut msgs = vec![Message::new(Role::System, system)];
    msgs.extend(repair_tool_history(&history));
    // 继承信号块(信号复利:上个会话的未决发现),放末尾同 Durable State,有界、仅在有信号时注入。
    if let Some(block) = &s.signal_block {
        msgs.push(Message::new(Role::System, block.clone()));
    }
    // Durable State 事实块放**末尾**(不进冻结的 system prompt):保首部前缀稳定利 Claude 缓存,
    // 又把模型注意力「重锚定」到当前客观事实。仅在有事实时注入,免空噪。
    if let Some(block) = durable_state_block(s) {
        msgs.push(Message::new(Role::System, block));
    }
    msgs
}

/// 上下文是否**腐烂**:按 `to_messages` 同口径做压缩后,历史估算 token 仍超 [`CONTEXT_ROT_TOKENS`]
/// —— 即单条超大消息压不掉(如塞进一个巨型工具输出)。纯函数、离线可测,只在终态分类时算一次。
pub(crate) fn context_rotted(s: &AgentState) -> bool {
    let raw: usize = s.history.iter().map(|m| est_tokens(&m.content)).sum();
    if raw <= AUTO_COMPACT_TOKENS {
        return false; // 未触发压缩,必然 ≤ 压缩阈值 < 硬上限
    }
    let compacted = compact_history(s.history.clone(), AUTO_COMPACT_KEEP);
    compacted
        .iter()
        .map(|m| est_tokens(&m.content))
        .sum::<usize>()
        > CONTEXT_ROT_TOKENS
}

/// 把 Durable State 编译成一段紧凑事实块(已改文件 / 上次报错 / 侦察未落盘提醒);无事实 → `None`。
/// 体量 O(去重文件数 + 一条报错 + 可选一行 explore nudge),**不随步数膨胀**。
pub(crate) fn durable_state_block(s: &AgentState) -> Option<String> {
    let explore_nudge = s.explore_streak >= EXPLORE_NUDGE_AFTER;
    if s.modified_files.is_empty()
        && s.last_error.is_none()
        && s.issues.is_empty()
        && !explore_nudge
        && s.dispatch_wave_count() == 0
        && !s.codegraph_unavailable
        && s.todos.is_empty()
        && s.last_read_paths.is_empty()
        && s.live_shell_jobs.is_empty()
    {
        return None;
    }
    let mut b = String::from("<durable_state>\n");
    if !s.issues.is_empty() {
        b.push_str(&format!("verification_issues: {}\n", s.issues.join("; ")));
        b.push_str(
            "recovery: The previous final answer was rejected. Do not return another final answer yet. Continue the user's required sequence with the next concrete tool call; resolve the issue, then verify before answering.\n",
        );
    }
    if s.todos.iter().any(|todo| todo.status.trim() != "completed") {
        let items: Vec<String> = s
            .todos
            .iter()
            .map(|todo| format!("[{}] {}", todo.status, todo.content))
            .collect();
        b.push_str(&format!("todos: {}\n", items.join("; ")));
    }
    if !s.last_read_paths.is_empty() {
        b.push_str(&format!("located: {}\n", s.last_read_paths.join(", ")));
        if s.modified_files.is_empty() {
            b.push_str(
                "next: continue the user's required sequence. Edit only a task-approved path; a located evidence path is not automatically writable. Do not restart full-repo search.\n",
            );
        }
    }
    if !s.live_shell_jobs.is_empty() {
        b.push_str(&format!(
            "live_jobs: {} (poll run_shell job_id; do not restart; blocks completion)\n",
            s.live_shell_jobs.join(", ")
        ));
    }
    if !s.modified_files.is_empty() {
        let files: Vec<&str> = s.modified_files.iter().map(String::as_str).collect();
        b.push_str(&format!("已改文件: {}\n", files.join(", ")));
    }
    if let Some(e) = &s.last_error {
        b.push_str(&format!("上次报错: {e}\n"));
    }
    if explore_nudge {
        b.push_str(&format!(
            "exploration_streak: {} (read/search calls since last write/edit; no durable change). Stop repeated exploration now. If an approved target is known, take the smallest action and verify it; in read-only mode, answer with supported facts; if blocked, state the concrete blocker. Do not make another read/search call merely to reset this counter.\n",
            s.explore_streak
        ));
    }
    if s.dispatch_wave_count() > 0 {
        let remaining = MAX_DISPATCH_BATCHES.saturating_sub(s.dispatch_wave_count());
        b.push_str(&format!(
            "dispatch_batches: {}/{} used ({} remaining; each wave may run 2-3 sub-agents; do not repeat a failed wave without a concrete reason).\n",
            s.dispatch_wave_count(), MAX_DISPATCH_BATCHES, remaining
        ));
    }
    if s.codegraph_unavailable {
        b.push_str(
            "codegraph_unavailable: true (CodeGraph is not available for this workspace; use built-in read_file/search or the smallest safe action, and do not retry codegraph).\n",
        );
    }
    b.push_str("</durable_state>");
    Some(b)
}

/// 上下文压缩(`/compact` + 自动,DoD②):历史太长时,**保全早期区的每一条 user 消息**(= 用户历次
/// 指令/意图)+ 一条摘要标记 + **最近 `keep` 条**,其余早期 assistant/tool 噪声压掉。
/// 防长会话「上下文腐烂」,更防「**意图漂失**」—— 多轮下用户中段的澄清/纠偏(如「不要 MD 要真页面」)
/// 若被当噪声压掉,模型就只剩最初那条模糊任务,照旧理解乱做。用户消息**少而关键**,是意图的唯一载体,
/// 一律保留;体量噪声(工具输出/助手 tool_call)才是压缩对象。确定性截断,不烧一次 LLM。
pub fn compact_history(history: Vec<Message>, keep: usize) -> Vec<Message> {
    if history.len() <= keep + 1 {
        return history;
    }
    // 保留窗口 = 末 keep 条。若窗口首条是 role=tool(其配对的 assistant 已被压进摘要),
    // 从前端裁掉这些悬空 tool 结果 —— 否则 OpenAI 兼容端点会因「tool 无前置 tool_calls」400。
    let mut recent = &history[history.len() - keep..];
    while recent.first().is_some_and(|m| m.role == Role::Tool) {
        recent = &recent[1..];
    }
    let split = history.len() - recent.len(); // 早期区 = [0, split)
                                              // 早期区里**所有 user 意图消息**全保(含首个原始任务与中段每次澄清),其余压成一条标记。
    let mut out: Vec<Message> = history[..split]
        .iter()
        .filter(|m| m.role == Role::User)
        .cloned()
        .collect();
    let dropped = split - out.len();
    out.push(Message::user(format!(
        "[上下文已压缩:省略 {dropped} 条早期工具/助手消息;上方 user 指令为完整意图,须据此推进]"
    )));
    out.extend(recent.iter().cloned());
    out
}

#[cfg(test)]
mod tests {
    use super::{
        bound_observation, compact_history, context_rotted, durable_state_block, est_tokens,
        to_messages, AUTO_COMPACT_KEEP, CONTEXT_ROT_TOKENS, OBS_CHAR_CAP,
    };
    use crate::brain::{tool_output_failed, tool_output_ok};
    use crate::exec::is_error_observation;
    use crate::state::{AgentState, Todo, EXPLORE_NUDGE_AFTER};
    use provider::{Message, Role};

    /// 上下文腐烂判定:小历史不腐烂;单条超硬上限的巨消息(压不掉)→ 腐烂。
    #[test]
    fn context_rotted_detects_unshrinkable_giant_message() {
        let small = AgentState {
            history: vec![Message::user("短消息")],
            ..Default::default()
        };
        assert!(!context_rotted(&small), "小历史不应判腐烂");

        // 多条普通消息**真的超**压缩阈值(1200×6≈7200tok>6000),但压缩保留尾部 8 条 → 收敛到硬上限内 → 不腐烂。
        let many: Vec<Message> = (0..1200).map(|_| Message::user("噪音消息一段")).collect();
        let compactable = AgentState {
            history: many,
            ..Default::default()
        };
        assert!(!context_rotted(&compactable), "可压缩历史不应判腐烂");

        // 单条巨消息压不掉 → 腐烂。
        let rot = AgentState {
            history: vec![Message::user("字".repeat(CONTEXT_ROT_TOKENS + 1))],
            ..Default::default()
        };
        assert!(context_rotted(&rot), "单条超硬上限的巨消息应判腐烂");
    }

    /// 巨型工具输出确定性截断:超上限 → head+tail 预览;不误伤常规输出;保 verify/durable 判据信号。
    #[test]
    fn bound_observation_truncates_giant_but_preserves_signals() {
        // 未超上限 → 原样(逐字节相等)。
        let small = "短小输出 exit 0: done".to_string();
        assert_eq!(bound_observation(small.clone()), small);

        // 超上限 → 截断:含 head 片段 + tail 片段 + 截断标记,总长有界。
        let giant = format!("HEAD_MARK{}TAIL_MARK", "x".repeat(20000));
        let bounded = bound_observation(giant);
        let n = bounded.chars().count();
        assert!(n <= OBS_CHAR_CAP + 60, "截断后应有界,实际 {n}");
        assert!(bounded.starts_with("HEAD_MARK"), "应保留 head 片段");
        assert!(bounded.ends_with("TAIL_MARK"), "应保留 tail 片段");
        assert!(bounded.contains("截断"), "应含截断标记");

        // 截断标记不含判据词 → 不污染 error/失败信号。
        let plain = bound_observation("平安无事输出".repeat(3000));
        assert!(
            !is_error_observation(&plain),
            "无错巨输出截断后不应被判为错误"
        );
        assert!(!tool_output_failed(&plain), "无错巨输出截断后不应判失败");

        // head 保 `exit 0:` 前缀 → 成功信号存活。
        let okout = bound_observation(format!("exit 0: {}", "y".repeat(20000)));
        assert!(tool_output_ok(&okout), "截断后 exit 0 成功信号应存活");

        // head 保 `exit 7:` → 失败信号存活。
        let failout = bound_observation(format!("exit 7: {}", "z".repeat(20000)));
        assert!(tool_output_failed(&failout), "截断后非零退出失败信号应存活");

        // 相同巨输入 → 截断结果相同(stall 检测不被破坏)。
        let a = bound_observation("同样的巨输出".repeat(3000));
        let b = bound_observation("同样的巨输出".repeat(3000));
        assert_eq!(a, b, "确定性:相同输入截断结果一致");
    }

    /// 消费者接线:state 带 signal_block → to_messages 把它作为 system 消息注入(末尾)。
    #[test]
    fn to_messages_injects_inherited_signal_block() {
        let state = AgentState {
            signal_block: Some(
                "<inherited_signals>\n- [x] (todo) 干这个\n</inherited_signals>".into(),
            ),
            ..Default::default()
        };
        let msgs = to_messages("base system", &state);
        assert!(
            msgs.iter()
                .any(|m| m.role == Role::System && m.content.contains("inherited_signals")),
            "继承信号块应作为 system 消息注入"
        );
    }

    /// DoD②:/compact 压缩历史 —— 显著收缩、含摘要标记、保留最近 keep 条,且**所有 user 意图消息全保**
    /// (尤其多轮下**中段的澄清/纠偏**不得被当噪声压掉 —— 那正是「模型丢意图、照旧乱做」的根因)。
    #[test]
    fn compact_history_keeps_all_user_intent_and_recent() {
        // 真实混合:3 条 user 任务散布在大量 assistant 噪声中(仿实录:507 消息仅 3 条 user)。
        let mut hist = vec![Message::user("任务A")];
        for i in 0..6 {
            hist.push(Message::assistant(format!("a{i}")));
        }
        hist.push(Message::user("任务B:不要MD要真页面")); // 中段澄清 —— 旧实现(只保 history[0])会压掉它
        for i in 0..6 {
            hist.push(Message::assistant(format!("b{i}")));
        }
        hist.push(Message::user("继续"));
        for i in 0..6 {
            hist.push(Message::assistant(format!("c{i}")));
        }
        let n = hist.len(); // 21
        let out = compact_history(hist, 4);
        let users: Vec<&str> = out
            .iter()
            .filter(|m| m.role == Role::User && !m.content.contains("压缩"))
            .map(|m| m.content.as_str())
            .collect();
        assert!(users.contains(&"任务A"), "首个任务须保");
        assert!(
            users.contains(&"任务B:不要MD要真页面"),
            "中段澄清意图绝不能被压掉(意图漂失根因)"
        );
        assert!(users.contains(&"继续"), "最新指令须保");
        assert!(out.len() < n, "仍应显著收缩:{} !< {n}", out.len());
        assert!(out.iter().any(|m| m.content.contains("压缩")), "含摘要标记");
        assert_eq!(out.last().unwrap().content, "c5", "最近一条须保");
        // 短历史不动。
        let short: Vec<Message> = (0..3).map(|i| Message::user(format!("s{i}"))).collect();
        assert_eq!(compact_history(short.clone(), 4).len(), short.len());
    }

    /// 自动压缩:history 估算 token 超阈值(按**内容体量**而非条数)时,`to_messages` 发给 LLM
    /// 的消息收敛为有界快照(O(n)→O(1))。
    #[test]
    fn to_messages_auto_compacts_when_history_heavy() {
        // 40 条较大消息 → 估算 token 总量超阈值 → 触发压缩。
        let mut hist = vec![Message::user("原始任务")];
        for i in 0..40 {
            hist.push(Message::assistant(format!("step {i}: {}", "x".repeat(700))));
        }
        let s = AgentState::new("原始任务").with_history(hist);
        let msgs = to_messages("SYS", &s);
        assert!(
            msgs.len() <= 1 + 2 + AUTO_COMPACT_KEEP,
            "重历史应收敛为有界,实得 {}",
            msgs.len()
        );
        assert_eq!(msgs[0].role, Role::System);
        assert!(
            msgs.iter().any(|m| m.content.contains("压缩")),
            "应有压缩标记"
        );
        assert!(
            msgs.iter().any(|m| m.content == "原始任务"),
            "原始任务须保留"
        );
        // 轻历史(总量未超阈值)不压缩,全量带过 —— 哪怕条数不少也不误伤。
        let light = AgentState::new("t")
            .with_history((0..20).map(|i| Message::user(format!("m{i}"))).collect());
        assert_eq!(to_messages("SYS", &light).len(), 1 + 20);
    }

    /// 触发判据的本地估算:同字符数下 CJK ≈ ASCII 的 4 倍(CJK 1 tok/字,ASCII 1 tok/4 字符)。
    #[test]
    fn est_tokens_weights_cjk_heavier_than_ascii() {
        assert_eq!(est_tokens(&"a".repeat(400)), 100);
        assert_eq!(est_tokens(&"中".repeat(400)), 400);
    }

    /// 事实块注入 messages **末尾**(role=system,冻结的首部 system prompt 不动);无事实则不加。
    #[test]
    fn to_messages_appends_durable_fact_block() {
        let mut st = AgentState::new("原始任务").with_history(vec![Message::user("原始任务")]);
        st.modified_files.insert("a.rs".into());
        st.last_error = Some("boom".into());
        let msgs = to_messages("SYS", &st);
        assert_eq!(msgs[0].content, "SYS", "首部 system prompt 保持冻结");
        let last = msgs.last().unwrap();
        assert_eq!(last.role, Role::System);
        assert!(last.content.contains("a.rs") && last.content.contains("boom"));
        // 无 durable 状态 → 不加尾块。
        let clean = AgentState::new("t").with_history(vec![Message::user("t")]);
        assert!(!to_messages("SYS", &clean)
            .last()
            .unwrap()
            .content
            .contains("durable_state"));

        // 达到 nudge 阈值 → 给真实模型「定位后立即动手」提醒；
        // MAX_EXPLORE 仍负责硬停，nudge 只改 prompt 事实块，不改路由。
        let before_nudge = AgentState {
            explore_streak: EXPLORE_NUDGE_AFTER - 1,
            history: vec![Message::user("t")],
            ..Default::default()
        };
        assert!(
            !to_messages("SYS", &before_nudge)
                .last()
                .unwrap()
                .content
                .contains("侦察未落盘"),
            "nudge should not appear before the threshold"
        );

        // 达到 nudge 阈值 → 注入「定位后立即动手」提醒(无改文件也有块)。
        let thrash = AgentState {
            explore_streak: EXPLORE_NUDGE_AFTER,
            history: vec![Message::user("t")],
            ..Default::default()
        };
        let msgs = to_messages("SYS", &thrash);
        let last = msgs.last().unwrap();
        assert!(
            last.content.contains("exploration_streak")
                && last.content.contains("Stop repeated exploration now")
                && last.content.contains("read-only mode")
                && last.content.contains("approved target"),
            "应 nudge 切入收束: {}",
            last.content
        );
    }

    /// 压缩窗口首端的悬空 role=tool(配对 assistant 已被压掉)必须裁掉,防 OpenAI 兼容端点 400。
    #[test]
    fn compact_history_drops_dangling_tool_result() {
        let mut hist = vec![Message::user("task")];
        for i in 0..8 {
            hist.push(Message::assistant(format!("a{i}")));
        }
        hist.push(Message::tool_result("call1", "tool out A")); // keep=4 时会落在窗口首
        hist.push(Message::assistant("a-final"));
        hist.push(Message::tool_result("call2", "tool out B"));
        hist.push(Message::assistant("a-last"));
        let out = compact_history(hist, 4);
        assert_eq!(out[0].content, "task"); // 原始任务保留
        assert_ne!(
            out[2].role,
            Role::Tool,
            "首条保留消息不应是悬空 tool: {:?}",
            out[2]
        );
    }

    #[test]
    fn compact_keeps_todos_and_located_paths_in_fact_block() {
        let mut history = vec![Message::user("edit pack.txt then package")];
        for i in 0..20 {
            history.push(Message::assistant(format!("noise {i} {}", "x".repeat(800))));
            history.push(Message::tool_result(
                format!("c{i}"),
                format!("search hit {}", "y".repeat(800)),
            ));
        }
        let state = AgentState {
            history,
            todos: vec![Todo {
                content: "pack release".into(),
                status: "in_progress".into(),
            }],
            last_read_paths: vec!["pack.txt".into()],
            live_shell_jobs: vec!["sh-1-2".into()],
            ..Default::default()
        };
        let msgs = to_messages("SYS", &state);
        let last = msgs.last().unwrap();
        assert_eq!(last.role, Role::System);
        assert!(last.content.contains("pack release"), "{}", last.content);
        assert!(last.content.contains("pack.txt"), "{}", last.content);
        assert!(last.content.contains("sh-1-2"), "{}", last.content);
        assert!(
            last.content.contains("live_jobs"),
            "compact must keep parked jobs: {}",
            last.content
        );
    }

    #[test]
    fn durable_state_reports_dispatch_wave_budget_without_blocking_next_wave() {
        let state = AgentState {
            dispatch_batches_used: 2,
            ..AgentState::new("inspect")
        };
        let block = durable_state_block(&state).expect("dispatch budget is durable state");
        assert!(block.contains("dispatch_batches: 2/8 used"));
        assert!(block.contains("6 remaining"));
        assert!(block.contains("each wave may run 2-3 sub-agents"));
    }

    #[test]
    fn durable_state_exposes_verifier_rejection_to_the_next_model_turn() {
        let state = AgentState {
            issues: vec!["target known, matching edit not landed".into()],
            ..AgentState::new("write result.md")
        };
        let messages = to_messages("SYS", &state);
        let feedback = messages.last().expect("verifier feedback system message");
        assert_eq!(feedback.role, Role::System);
        assert!(feedback.content.contains("verification_issues"));
        assert!(feedback
            .content
            .contains("target known, matching edit not landed"));
        assert!(feedback
            .content
            .contains("previous final answer was rejected"));
        assert!(feedback.content.contains("next concrete tool call"));
    }
}
