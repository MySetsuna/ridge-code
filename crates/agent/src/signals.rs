use crate::context::bound_observation;
use crate::graph::*;
use crate::state::*;
use provider::{CompletionRequest, LlmProvider, Message, Role};

// ───────────────────────── 信号复利(多 loop 共享大脑)─────────────────────────
// 「标准存储库」不止审计留痕:一个会话探测到的事实(发现/摩擦/待办)落成结构化 signal,
// 下个会话**自动继承**之 —— 解 agent「每会话冷启动、重新学项目」的根本损耗。这是把 agent
// 从孤立脚本升为跨会话复利系统的心脏(证据研判 iter-15:单二进制单用户下证据最硬的差异化长板)。

/// 信号复利:跨会话共享的知识层落盘目录(**项目级**,cwd 本地,像 `.ridge/runs`)。
pub const SIGNALS_DIR: &str = ".ridge/signals";
/// 注入上下文的信号块**硬字符上限** —— 有界,防复利知识膨胀反噬 token 节约成果。
const SIGNALS_BLOCK_MAX: usize = 1200;

/// 一条可跨会话复用的**信号**(发现 / 摩擦点 / 待办)。落盘为带 frontmatter 的 markdown。
#[derive(Clone, Debug, PartialEq)]
pub struct Signal {
    pub id: String,
    pub kind: String,   // frontmatter 里的 `type`(避 Rust 关键字)
    pub status: String, // open / resolved
    pub source: String, // 产它的 run id 或 "manual"
    pub body: String,
}

/// slug 化:留字母数字、小写、其余转 `-`,截 24 字 —— 拼进文件名/id,须文件系统安全。
fn slugify(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let t = out.trim_matches('-');
    if t.is_empty() {
        "signal".to_string()
    } else {
        t.chars().take(24).collect()
    }
}

/// 信号 id = `<slug(kind)>-<内容哈希>`。用 `DefaultHasher`(固定 key、确定性、无时间戳):
/// **同内容 → 同 id** → 天然幂等去重(重复记同一发现不产重复文件),且离线可测。
fn signal_id(kind: &str, body: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    kind.hash(&mut h);
    body.hash(&mut h);
    format!("{}-{:08x}", slugify(kind), h.finish() as u32)
}

fn render_signal(sig: &Signal) -> String {
    format!(
        "---\nid: {}\ntype: {}\nstatus: {}\nsource: {}\n---\n{}\n",
        sig.id,
        sig.kind,
        sig.status,
        sig.source,
        sig.body.trim()
    )
}

/// 解析一份 signal markdown(frontmatter + 正文);缺 id / 格式坏 → `None`。
fn parse_signal(text: &str) -> Option<Signal> {
    let rest = text.strip_prefix("---\n")?;
    let (front, body) = rest.split_once("\n---\n")?;
    let (mut id, mut kind, mut status, mut source) =
        (String::new(), String::new(), String::new(), String::new());
    for line in front.lines() {
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim().to_string();
            match k.trim() {
                "id" => id = v,
                "type" => kind = v,
                "status" => status = v,
                "source" => source = v,
                _ => {}
            }
        }
    }
    if id.is_empty() {
        return None;
    }
    if status.is_empty() {
        status = "open".to_string();
    }
    Some(Signal {
        id,
        kind,
        status,
        source,
        body: body.trim().to_string(),
    })
}

/// **产者**:把一条 `open` 信号落盘 `dir/<id>.md`(同内容 id 相同 → 幂等去重)。返回 id。
pub fn signal_create(
    dir: impl AsRef<std::path::Path>,
    kind: &str,
    body: &str,
    source: &str,
) -> std::io::Result<String> {
    let dir = dir.as_ref();
    std::fs::create_dir_all(dir)?;
    let id = signal_id(kind, body);
    let sig = Signal {
        id: id.clone(),
        kind: kind.to_string(),
        status: "open".to_string(),
        source: source.to_string(),
        body: body.to_string(),
    };
    std::fs::write(dir.join(format!("{id}.md")), render_signal(&sig))?;
    Ok(id)
}

/// **消解**:把 `dir` 里 id 匹配的信号 status 改 `resolved`(闭环,免下轮重复消费)。找不到 → `Ok(false)`。
pub fn signal_resolve(dir: impl AsRef<std::path::Path>, id: &str) -> std::io::Result<bool> {
    let path = dir.as_ref().join(format!("{id}.md"));
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(false);
    };
    let Some(mut sig) = parse_signal(&text) else {
        return Ok(false);
    };
    if sig.status != "resolved" {
        sig.status = "resolved".to_string();
        std::fs::write(&path, render_signal(&sig))?;
    }
    Ok(true)
}

/// **消费者**:读 `dir` 下全部 signal,取 `status=open`,按 id 排序(有序稳态、利缓存/确定性)。
pub fn load_open_signals(dir: impl AsRef<std::path::Path>) -> Vec<Signal> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<Signal> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|t| parse_signal(&t))
        .filter(|s| s.status == "open")
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// 把 open 信号编成**有界**注入块(超 [`SIGNALS_BLOCK_MAX`] 截断,防复利知识膨胀)。无 → `None`。
fn signals_block(sigs: &[Signal]) -> Option<String> {
    if sigs.is_empty() {
        return None;
    }
    let mut b = String::from(
        "<inherited_signals>\n上个会话留下的未决信号;处理完请调 signal_write(resolve=<id>)消解:\n",
    );
    for s in sigs {
        let line = format!("- [{}] ({}) {}\n", s.id, s.kind, s.body.replace('\n', " "));
        if b.len() + line.len() > SIGNALS_BLOCK_MAX {
            b.push_str("- …(更多信号已省略)\n");
            break;
        }
        b.push_str(&line);
    }
    b.push_str("</inherited_signals>");
    Some(b)
}

/// 供 CLI 在建 `AgentState` 前调用:读项目级 `.ridge/signals` 的 open 信号 → 有界注入块。
pub fn load_signal_block() -> Option<String> {
    signals_block(&load_open_signals(SIGNALS_DIR))
}

/// **自动产者**:run 收尾时,失败(非成功停机 / 有报错)自动落一条 `failure` 信号 ——
/// loop engineering「preserve mistakes so the loop can learn」:下个会话开局即继承「上次卡在哪」,
/// 不重蹈覆辙。成功 run 不产噪。同内容幂等去重(反复失败于同处 → 不刷屏)。返回 signal id(无可记 → None)。
pub fn auto_signal_from_run(
    out: &AgentState,
    dir: impl AsRef<std::path::Path>,
    source: &str,
) -> Option<String> {
    let reason = halt_reason(out);
    if reason.is_success() && out.last_error.is_none() {
        return None;
    }
    let task: String = out.task.chars().take(80).collect();
    let mut body = format!("任务未竟: {task} | 停机: {}", reason.as_str());
    if let Some(e) = &out.last_error {
        let e: String = e.chars().take(160).collect();
        body.push_str(&format!(" | 末错: {e}"));
    }
    signal_create(dir, "failure", &body, source).ok()
}

// ─────────── 自动 signal 抽取器(复利环产者的「发现/待办」侧)───────────
// iter-17 的自动产者只记**失败**;本抽取器补另一半:run 收尾用 provider **一次性**把执行轨迹
// 提炼成可跨会话复用的信号(发现/摩擦/待办),喂已建的产→消→解复利环。**opt-in**(env,默认关):
// 尊重 token 北极星,不默认给每轮加一次 LLM 成本;`--every` 常驻用户可开启以求复利。
// (对抗评审 iter-18:采纳「安全内核版」—— 喂已有 signals 环;**驳回**「自动改写 harness」,单用户样本不足、改写无 checker。)

/// 一次抽取最多提炼多少条(宁缺毋滥 + 有界成本/防刷屏)。
const MAX_EXTRACTED_SIGNALS: usize = 5;

const SIGNAL_EXTRACT_SYSTEM: &str =
    "你是复盘助手。从本次执行轨迹提炼**可跨会话复用**的信号,助下个会话免重新摸索。\
只输出新增的、具体的、可复用条目;每行一条,格式严格 `kind: body`,kind ∈ {discovery, friction, todo}:\
discovery=项目事实/结构发现;friction=踩的坑/易错处;todo=本次未竟、下次该做。\
最多 5 条,宁缺毋滥;无可复用信号则只回 NONE。勿复述任务,勿客套。";

/// run 是否有「值得抽取」的实质轨迹(动过工具或改过文件)。纯轻量运行不抽,省一次 LLM 调用。
fn run_has_substance(out: &AgentState) -> bool {
    !out.modified_files.is_empty() || out.messages.iter().any(|m| m.starts_with("act:"))
}

/// 构造抽取请求(有界轨迹 → 提炼复利信号)。无实质轨迹 → `None`(不抽)。
fn signal_extract_request(out: &AgentState) -> Option<CompletionRequest> {
    if !run_has_substance(out) {
        return None;
    }
    let task: String = out.task.chars().take(200).collect();
    // 轨迹有界:复用 bound_observation(head+tail 预览),免巨型轨迹撑爆这一次抽取调用。
    let traj = bound_observation(out.messages.join("\n"));
    Some(CompletionRequest {
        messages: vec![
            Message::new(Role::System, SIGNAL_EXTRACT_SYSTEM),
            Message::new(Role::User, format!("任务:{task}\n\n执行轨迹:\n{traj}")),
        ],
        tools: vec![],
    })
}

/// 解析抽取器输出为 `(kind, body)` 列表。**纯函数**:每行 `kind: body`(冒号中英皆可),
/// kind 须在允许集内、body 非空;`NONE`/空行/不合规行/markdown 项目符号一律容错忽略;上限 [`MAX_EXTRACTED_SIGNALS`]。
fn parse_extracted_signals(text: &str) -> Vec<(String, String)> {
    const ALLOWED: [&str; 3] = ["discovery", "friction", "todo"];
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim().trim_start_matches(['-', '*', '•', ' ']).trim();
        let Some((k, b)) = line.split_once([':', '：']) else {
            continue;
        };
        let kind = k.trim().to_lowercase();
        let body = b.trim();
        if !ALLOWED.contains(&kind.as_str()) || body.is_empty() {
            continue;
        }
        out.push((kind, body.to_string()));
        if out.len() >= MAX_EXTRACTED_SIGNALS {
            break;
        }
    }
    out
}

/// 自动 signal 抽取是否启用(**opt-in**,env `RIDGE_EXTRACT_SIGNALS` = 1/true/on/yes)。
/// 默认关 —— 尊重 token 北极星,不默认给每轮加一次 LLM 成本。
pub fn signal_extract_enabled() -> bool {
    std::env::var("RIDGE_EXTRACT_SIGNALS")
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "on" | "yes"
            )
        })
        .unwrap_or(false)
}

/// **自动抽取器**:run 收尾用 provider 一次性把轨迹提炼成复利信号,落 `dir`(经 `signal_create` 内容哈希
/// **幂等去重**,反复同一发现不刷屏)。返回落盘的 signal id 列表。抽取失败/无所得/无实质轨迹 → 空
/// (best-effort,**绝不掀翻主流程**)。source = 本 run id(溯源回指 `.ridge/runs/<id>`)。
pub async fn extract_signals_from_run(
    provider: &dyn LlmProvider,
    out: &AgentState,
    dir: impl AsRef<std::path::Path>,
    source: &str,
) -> Vec<String> {
    let Some(req) = signal_extract_request(out) else {
        return Vec::new();
    };
    let text = match provider.complete(&req).await {
        Ok(c) => c.text,
        Err(_) => return Vec::new(),
    };
    let dir = dir.as_ref();
    parse_extracted_signals(&text)
        .into_iter()
        .filter_map(|(kind, body)| signal_create(dir, &kind, &body, source).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;

    /// 信号复利·产→消:产者落盘 open 信号,消费者读回;同内容 id 相同(幂等去重)。
    #[test]
    fn signal_create_then_load_open_roundtrips() {
        let dir = std::env::temp_dir().join("ridge_signal_test_load");
        let _ = std::fs::remove_dir_all(&dir);

        let id = signal_create(&dir, "friction", "构建慢:cold build 90s", "run-1").unwrap();
        // 幂等:同 type+body 再产一次 → 同 id、不产重复文件。
        let id2 = signal_create(&dir, "friction", "构建慢:cold build 90s", "run-2").unwrap();
        assert_eq!(id, id2, "同内容应得同 id(内容哈希幂等去重)");
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "幂等:只该有一个文件"
        );

        let open = load_open_signals(&dir);
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, id);
        assert_eq!(open[0].kind, "friction");
        assert_eq!(open[0].status, "open");
        assert!(open[0].body.contains("cold build 90s"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 信号复利·消解闭环:resolve 翻 status → 不再被消费者扫入(免下轮重复消费)。
    #[test]
    fn signal_resolve_removes_from_open() {
        let dir = std::env::temp_dir().join("ridge_signal_test_resolve");
        let _ = std::fs::remove_dir_all(&dir);

        let id = signal_create(&dir, "todo", "补 X 的单测", "run-1").unwrap();
        assert_eq!(load_open_signals(&dir).len(), 1);

        assert!(signal_resolve(&dir, &id).unwrap(), "应找到并消解");
        assert!(load_open_signals(&dir).is_empty(), "resolved 不该再被消费");
        assert!(
            !signal_resolve(&dir, "nonexistent-00000000").unwrap(),
            "不存在的 id → false"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 信号注入块**有界**:再多再长信号,块字符数不超硬上限;空信号 → None(不注入)。
    #[test]
    fn signals_block_is_bounded_and_none_when_empty() {
        assert!(signals_block(&[]).is_none(), "无信号不注入");

        let many: Vec<Signal> = (0..200)
            .map(|i| Signal {
                id: format!("sig-{i:08x}"),
                kind: "note".to_string(),
                status: "open".to_string(),
                source: "run-x".to_string(),
                body: format!("一条相当长的信号正文用于撑爆上限 {i} ").repeat(4),
            })
            .collect();
        let block = signals_block(&many).unwrap();
        assert!(
            block.len() <= SIGNALS_BLOCK_MAX + 64,
            "注入块须有界,得 {} 字节",
            block.len()
        );
        assert!(block.contains("…(更多信号已省略)"), "超限应截断并标注");
    }

    /// 自动产者:失败 run 落 failure 信号(preserve mistakes);成功 run 不产噪。
    #[test]
    fn auto_signal_records_failures_only() {
        let dir = std::env::temp_dir().join("ridge_auto_signal_test");
        let _ = std::fs::remove_dir_all(&dir);

        // 成功 run → 不产信号。
        let ok = AgentState {
            approved: true,
            task: "任务甲".into(),
            ..Default::default()
        };
        assert!(
            auto_signal_from_run(&ok, &dir, "run-ok").is_none(),
            "成功不该产噪"
        );

        // 失败 run(到回合上限未通过)→ 落一条 failure 信号,含任务与停机原因。
        let bad = AgentState {
            task: "任务乙".into(),
            steps: MAX_STEPS,
            last_error: Some("build error: E0433".into()),
            ..Default::default()
        };
        let id = auto_signal_from_run(&bad, &dir, "run-bad").expect("失败应产信号");
        let open = load_open_signals(&dir);
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, id);
        assert_eq!(open[0].kind, "failure");
        assert!(
            open[0].body.contains("任务乙") && open[0].body.contains("step_cap"),
            "body 应含任务名 + 停机原因 step_cap:{}",
            open[0].body
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 抽取器解析:合规 `kind: body` 收下,NONE/不合规 kind/空 body/项目符号一律容错,上限截断。
    #[test]
    fn parse_extracted_signals_filters_and_caps() {
        let text = "NONE\n\
            discovery: 构建脚本在 crates/tools\n\
            - friction: MCP 路径需对服务器可见\n\
            todo：中文冒号也认\n\
            garbage: 不在允许集\n\
            discovery:   \n\
            随便一行没冒号\n\
            todo: 甲\ndiscovery: 乙\nfriction: 丙\ntodo: 丁(第6条应被截断)";
        let got = parse_extracted_signals(text);
        // 允许集内 + body 非空的:discovery(构建)、friction(MCP)、todo(中文冒号)、todo(甲)、discovery(乙) = 上限 5 条。
        assert_eq!(got.len(), MAX_EXTRACTED_SIGNALS);
        assert_eq!(
            got[0],
            ("discovery".into(), "构建脚本在 crates/tools".into())
        );
        assert_eq!(got[1].0, "friction");
        assert_eq!(got[2], ("todo".into(), "中文冒号也认".into()));
        // garbage/空 body/无冒号行被过滤;第 6 条被截断。
        assert!(got
            .iter()
            .all(|(k, _)| ["discovery", "friction", "todo"].contains(&k.as_str())));
        assert!(parse_extracted_signals("NONE").is_empty());
    }

    /// 抽取门:无实质轨迹(未动工具/未改文件)→ 不抽(省 LLM 调用);有 act/改文件 → 构造请求。
    #[test]
    fn extract_request_gated_on_substance() {
        let empty = AgentState {
            task: "查天气".into(),
            ..Default::default()
        };
        assert!(!run_has_substance(&empty));
        assert!(signal_extract_request(&empty).is_none());

        let worked = AgentState {
            task: "改代码".into(),
            messages: vec!["act: edit_file -> edited src/x.rs".into()],
            ..Default::default()
        };
        assert!(run_has_substance(&worked));
        assert!(signal_extract_request(&worked).is_some());
    }

    /// 抽取器端到端:假 provider 回 canned 轨迹提炼 → 解析 → 落盘为 open 信号(幂等去重)。
    #[tokio::test]
    async fn extract_signals_from_run_writes_parsed_signals() {
        use provider::{Completion, ScriptedProvider};
        let dir = std::env::temp_dir().join("ridge_extract_test");
        let _ = std::fs::remove_dir_all(&dir);

        let out = AgentState {
            task: "重构 tools".into(),
            messages: vec!["act: edit_file -> edited crates/tools/src/lib.rs".into()],
            ..Default::default()
        };
        let canned = "discovery: Edit 结构字段 path/old/new 皆 pub\nfriction: apply_edits 原子性,任一越狱整批拒\nNONE";
        let provider = ScriptedProvider::new(vec![Completion {
            text: canned.into(),
            ..Default::default()
        }]);

        let ids = extract_signals_from_run(&provider, &out, &dir, "run-xyz").await;
        assert_eq!(ids.len(), 2, "应落 2 条(discovery+friction)");
        let open = load_open_signals(&dir);
        assert_eq!(open.len(), 2);
        assert!(open
            .iter()
            .any(|s| s.kind == "discovery" && s.source == "run-xyz"));
        assert!(open.iter().any(|s| s.kind == "friction"));

        // 幂等:同一 provider 输出再抽一次 → 内容哈希 id 相同,不新增文件。
        let provider2 = ScriptedProvider::new(vec![Completion {
            text: canned.into(),
            ..Default::default()
        }]);
        let ids2 = extract_signals_from_run(&provider2, &out, &dir, "run-xyz").await;
        assert_eq!(ids2, ids, "同内容幂等,id 一致");
        assert_eq!(load_open_signals(&dir).len(), 2, "幂等不新增");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
