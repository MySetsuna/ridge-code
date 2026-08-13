use ratatui::style::Color;

use super::{role_color, Role, ToolBlock, ToolPhase};
use agent::tool_output_failed;

fn final_payload(rest: &str) -> Option<&str> {
    if rest.is_empty() {
        Some(rest)
    } else {
        rest.strip_prefix(' ')
    }
}

pub(crate) fn final_answer_text(m: &str) -> Option<&str> {
    m.strip_prefix("(final)")
        .and_then(final_payload)
        .or_else(|| {
            m.strip_prefix("reason#").and_then(|rest| {
                rest.split_once(": (final)")
                    .and_then(|(_, text)| final_payload(text))
            })
        })
}

pub(crate) fn is_final_event(m: &str) -> bool {
    final_answer_text(m).is_some()
}

pub(crate) fn format_event_plain(m: &str) -> String {
    final_answer_text(m)
        .map(|x| {
            if x.trim().is_empty() {
                "🤖 [empty answer]".to_owned()
            } else {
                format!("🤖 {x}")
            }
        })
        .unwrap_or_else(|| m.to_owned())
}

/// 普通状态行按字符数截断；审阅详情使用原始内容路径，不调用此函数。
pub(crate) fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

/// 把一条 agent 消息转成可折叠显示行：折叠态显摘要，展开态显完整详情。
/// provider/运行错误是否**值得重试**(瞬时 vs 永久)。TUI 自动重试只该管瞬时失败;永久性失败
/// (余额/鉴权/坏请求)重试同样输入只白烧 —— 命中永久标记 → 不重试,余(含未知)默认可重试(不回退既有瞬时容错)。
pub(crate) fn is_retryable_error(msg: &str) -> bool {
    let m = msg.to_lowercase();
    // 永久:HTTP 4xx 中重试无益者(坏请求/鉴权/权限/找不到)。5xx / 429 不在此(可能瞬时)。
    const PERMANENT_HTTP: [&str; 4] = ["http 400", "http 401", "http 403", "http 404"];
    // 永久:余额/配额/欠费/鉴权关键词 —— 兜住「429 实为余额耗尽」(本 provider 用 429 表余额不足)。
    const PERMANENT_KW: [&str; 12] = [
        "余额不足",
        "无可用资源",
        "欠费",
        "充值",
        "quota",
        "insufficient",
        "payment",
        "billing",
        "invalid api key",
        "unauthorized",
        "authentication",
        "api key",
    ];
    !(PERMANENT_HTTP.iter().any(|p| m.contains(p)) || PERMANENT_KW.iter().any(|k| m.contains(k)))
}

pub(crate) fn summarize_event(m: &str) -> Vec<(String, Color)> {
    let info = role_color(Role::Info);
    if let Some(lines) = summarize_tool_call(m, info) {
        return lines;
    }
    if let Some(lines) = summarize_observation(m) {
        return lines;
    }
    vec![(format_event_plain(m), event_color(m))]
}

fn summarize_tool_call(m: &str, info: Color) -> Option<Vec<(String, Color)>> {
    let rest = m.strip_prefix("reason#")?;
    let idx = rest.find(": tool_call ")?;
    let body = &rest[idx + ": tool_call ".len()..];
    let (name, args_str) = body.split_once(' ')?;
    let args: serde_json::Value = serde_json::from_str(args_str).unwrap_or_default();
    let arg = |key: &str| args.get(key).and_then(|v| v.as_str()).unwrap_or("");
    Some(match name {
        "read_file" => vec![(format!("  ⋯ Read {}", arg("path")), info)],
        "write_file" => {
            let mut out = vec![(format!("  ⋯ Write {}", arg("path")), info)];
            out.extend(preview_lines(arg("contents"), 8));
            out
        }
        "edit_file" => {
            let mut out = vec![(format!("  ⋯ Edit {}", arg("path")), info)];
            out.extend(diff_lines(arg("old_string"), arg("new_string")));
            out
        }
        "apply_edits" => apply_edits_summary(&args, info),
        "run_shell" => vec![(
            format!(
                "  ⋯ $ {}",
                clip(arg("cmd").lines().next().unwrap_or(""), 160)
            ),
            info,
        )],
        "search" => vec![(format!("  ⋯ Search {}", arg("pattern")), info)],
        other => vec![(format!("  ⋯ {other}"), info)],
    })
}

fn summarize_observation(m: &str) -> Option<Vec<(String, Color)>> {
    let rest = m.strip_prefix("act: ")?;
    let (name, obs) = rest.split_once(" -> ")?;
    if tool_output_failed(obs) {
        return Some(format_failed_observation(name, obs));
    }
    let ok = role_color(Role::Success);
    if obs.trim().is_empty() {
        return Some(vec![(format!("  ✓ {name}: no output"), ok)]);
    }
    if name == "read_file" {
        let mut out = vec![(
            format!("  ✓ Read complete ({} chars)", obs.chars().count()),
            ok,
        )];
        out.extend(preview_lines(obs, 12));
        return Some(out);
    }
    let head = clip(obs.lines().next().unwrap_or(""), 200);
    let mut out = vec![(format!("  ✓ {name}: {head}"), ok)];
    out.extend(preview_lines(obs, 10));
    Some(out)
}

fn format_failed_observation(name: &str, obs: &str) -> Vec<(String, Color)> {
    let err = role_color(Role::Error);
    let lines: Vec<&str> = obs.lines().collect();
    let first = lines.first().copied().unwrap_or("");
    let mut out = vec![(format!("  ✗ {name}: {first}"), err)];
    out.extend(
        lines
            .iter()
            .skip(1)
            .map(|line| (format!("  │ {line}"), err)),
    );
    out
}

/// 将结构化工具事件转换为可折叠块；详情仅在 live 视口中按需显示。
pub(crate) fn tool_preview(m: &str) -> Option<ToolBlock> {
    if let Some(rest) = m.strip_prefix("reason#") {
        let body = rest.split_once(": tool_call ").map(|(_, body)| body)?;
        let name = body.split_whitespace().next()?.to_owned();
        return ToolBlock::from_lines_with_phase(summarize_event(m), ToolPhase::Call, Some(name));
    }
    let rest = m.strip_prefix("act: ")?;
    let (name, _) = rest.split_once(" -> ")?;
    ToolBlock::from_lines_with_phase(
        summarize_event(m),
        ToolPhase::Observation,
        Some(name.to_owned()),
    )
}

const MAX_BATCH_EDIT_SUMMARY_PATHS: usize = 3;

/// 批量编辑的摘要投影：折叠态显文件范围，展开态保留全部 ± 内容。
/// 只读 tool-call 参数，不访问磁盘；成功/失败仍由对应 `act:` 观察行裁决。
pub(crate) fn apply_edits_summary(args: &serde_json::Value, info: Color) -> Vec<(String, Color)> {
    let Some(edits) = args.get("edits").and_then(|value| value.as_array()) else {
        return vec![("  ⋯ Batch edit (missing edits)".to_owned(), info)];
    };

    let parsed: Vec<(&str, &str, &str)> = edits
        .iter()
        .filter_map(|edit| {
            let path = edit.get("path").and_then(|value| value.as_str())?;
            let old = edit
                .get("old_string")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let new = edit
                .get("new_string")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            (!path.is_empty()).then_some((path, old, new))
        })
        .collect();
    if parsed.is_empty() {
        return vec![("  ⋯ Batch edit (no valid edits)".to_owned(), info)];
    }

    let mut paths = Vec::new();
    for (path, _, _) in &parsed {
        if !paths.contains(path) {
            paths.push(*path);
        }
    }
    let mut visible_paths: Vec<String> = paths
        .iter()
        .take(MAX_BATCH_EDIT_SUMMARY_PATHS)
        .map(|path| clip(path, 96))
        .collect();
    if paths.len() > MAX_BATCH_EDIT_SUMMARY_PATHS {
        visible_paths.push(format!(
            "… +{} more",
            paths.len() - MAX_BATCH_EDIT_SUMMARY_PATHS
        ));
    }
    let mut out = vec![(
        format!(
            "  ⋯ Batch edit {} files / {} edits: {}",
            paths.len(),
            parsed.len(),
            visible_paths.join(", ")
        ),
        info,
    )];

    for (path, old, new) in parsed {
        out.push((format!("  ── {path}"), info));
        let diff = diff_lines(old, new);
        out.extend(diff);
    }
    out
}

/// 写文件/工具观察详情：保留收到的全部行，由详情视口负责滚动。
pub(crate) fn preview_lines(content: &str, _max: usize) -> Vec<(String, Color)> {
    let muted = role_color(Role::Muted);
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }
    lines
        .iter()
        .map(|line| (format!("  │ {line}"), muted))
        .collect()
}

/// edit_file 的 git-diff 式呈现:old 行 `-`(红)、new 行 `+`(绿),保留全部行与行内文字。
pub(crate) fn diff_lines(old: &str, new: &str) -> Vec<(String, Color)> {
    let (red, green) = (role_color(Role::DiffDel), role_color(Role::DiffAdd));
    let mut out = Vec::new();
    out.extend(old.lines().map(|line| (format!("  - {line}"), red)));
    out.extend(new.lines().map(|line| (format!("  + {line}"), green)));
    out
}
/// 事件行配色：所有语义色经 Role 取色，避免终答绕过主题集中点。
pub(crate) fn event_color(m: &str) -> Color {
    if m.starts_with("verify: PASS") {
        role_color(Role::Success)
    } else if m.starts_with("verify: FAIL") {
        role_color(Role::Error)
    } else if m.starts_with("act:") {
        role_color(Role::Warn)
    } else if is_final_event(m) {
        role_color(Role::Answer)
    } else {
        role_color(Role::Info)
    }
}
