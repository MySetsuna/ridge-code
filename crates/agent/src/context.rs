use crate::state::*;
use provider::{Message, Role};

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
    msgs.extend(history);
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

/// 把 Durable State 编译成一段紧凑事实块(已改文件 / 上次报错);无事实 → `None`(不注入)。
/// 体量 O(去重文件数 + 一条报错),**不随步数膨胀** —— 这是「事实驱动而非消息驱动」的 O(1) 关键。
pub(crate) fn durable_state_block(s: &AgentState) -> Option<String> {
    if s.modified_files.is_empty() && s.last_error.is_none() {
        return None;
    }
    let mut b = String::from("<durable_state>\n");
    if !s.modified_files.is_empty() {
        let files: Vec<&str> = s.modified_files.iter().map(String::as_str).collect();
        b.push_str(&format!("已改文件: {}\n", files.join(", ")));
    }
    if let Some(e) = &s.last_error {
        b.push_str(&format!("上次报错: {e}\n"));
    }
    b.push_str("</durable_state>");
    Some(b)
}

/// 上下文压缩(`/compact`,DoD②):历史太长时,保留**首条(原始任务)**+ 一条摘要标记 + **最近 `keep` 条**,
/// 其余压掉。防长会话「上下文腐烂」(Ralph 式,但用确定性截断,不烧一次 LLM)。
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
    let dropped = history.len() - 1 - recent.len();
    let mut out = Vec::with_capacity(recent.len() + 2);
    out.push(history[0].clone()); // 原始任务
    out.push(Message::user(format!(
        "[上下文已压缩:省略 {dropped} 条早期消息]"
    )));
    out.extend(recent.iter().cloned());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brain::{tool_output_failed, tool_output_ok};
    use crate::exec::is_error_observation;
    use crate::*;

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

    /// DoD②:/compact 压缩历史 —— 长度显著减少,保留首条(任务)+ 最近 keep 条。
    #[test]
    fn compact_history_shrinks_but_keeps_task_and_recent() {
        let hist: Vec<Message> = (0..10).map(|i| Message::user(format!("m{i}"))).collect();
        let compacted = compact_history(hist, 4);
        // 1(首)+ 1(摘要)+ 4(最近) = 6 < 10
        assert_eq!(compacted.len(), 6);
        assert_eq!(compacted[0].content, "m0"); // 原始任务保留
        assert!(compacted[1].content.contains("压缩")); // 摘要标记
        assert_eq!(compacted.last().unwrap().content, "m9"); // 最近保留
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
}
