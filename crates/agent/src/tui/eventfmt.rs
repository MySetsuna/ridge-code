use super::*;

pub(crate) fn final_answer_text(m: &str) -> Option<&str> {
    m.strip_prefix("(final) ").or_else(|| {
        m.strip_prefix("reason#")
            .and_then(|rest| rest.split_once(": (final) ").map(|(_, text)| text))
    })
}

pub(crate) fn is_final_event(m: &str) -> bool {
    final_answer_text(m).is_some()
}

pub(crate) fn format_event_plain(m: &str) -> String {
    final_answer_text(m)
        .map(|x| format!("🤖 {x}"))
        .unwrap_or_else(|| m.to_owned())
}

/// 按字符数截断(避免单行刷屏);超出加省略。
pub(crate) fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

/// 把一条 agent 消息转成**总览化**显示行(可多行,各带色)。核心:给总览、减细节 ——
/// 读文件折叠显路径与完成计数,展开时显有界内容预览;写文件显首几行预览、改文件显 ± 着色 diff(形如 git diff)。
/// **全文/全量在 run trace**;inline 已提交行不可回改,故预览截断并标注。
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
    // tool_call:`reason#N: tool_call {name} {json}`
    if let Some(rest) = m.strip_prefix("reason#") {
        if let Some(idx) = rest.find(": tool_call ") {
            let body = &rest[idx + ": tool_call ".len()..];
            if let Some((name, args_str)) = body.split_once(' ') {
                let args: serde_json::Value = serde_json::from_str(args_str).unwrap_or_default();
                let arg = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("");
                return match name {
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
                };
            }
        }
    }
    // 观察:`act: {name} -> {obs}`。读文件的内容已在 tool_call 行体现 → act 只回执一行(丢内容噪声)。
    if let Some(rest) = m.strip_prefix("act: ") {
        if let Some((name, obs)) = rest.split_once(" -> ") {
            // 失败观察(非零退出 / error / BLOCKED / permission)→ 红 ✗ + 多行错误正文,别伪装成绿 ✓、
            // 别只留首行把真报错藏掉(用户诉求:报错要看得见)。判据复用 verify 的 tool_output_failed
            // —— 单一真相:显红 ⇔ 确定性验证判失败。错误全文另存 trace,模型经 history 亦收到。
            if tool_output_failed(obs) {
                let err = role_color(Role::Error);
                let lines: Vec<&str> = obs.lines().collect();
                let first = lines.first().copied().unwrap_or("");
                let mut out = vec![(format!("  ✗ {name}: {}", clip(first, 200)), err)];
                for l in lines.iter().skip(1).take(8) {
                    out.push((format!("  │ {}", clip(l, 200)), err));
                }
                if lines.len() > 9 {
                    out.push((
                        format!("  │ … (+{} lines, full text in trace)", lines.len() - 9),
                        err,
                    ));
                }
                return out;
            }
            let ok = role_color(Role::Success);
            if obs.trim().is_empty() {
                return vec![(format!("  ✓ {name}: no output"), ok)];
            }
            if name == "read_file" {
                let mut out = vec![(
                    format!("  ✓ Read complete ({} chars)", obs.chars().count()),
                    ok,
                )];
                out.extend(preview_lines(obs, 12));
                return out;
            }
            let head = clip(obs.lines().next().unwrap_or(""), 200);
            let mut out = vec![(format!("  ✓ {name}: {head}"), ok)];
            if obs.lines().count() > 1 {
                out.extend(preview_lines(obs, 10));
            }
            return out;
        }
    }
    vec![(format_event_plain(m), event_color(m))]
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
const MAX_BATCH_EDIT_DETAIL_EDITS: usize = 4;
const MAX_BATCH_EDIT_DETAIL_LINES: usize = 18;

/// 批量编辑的有界投影：折叠态显文件范围，展开态显前几处 ± 预览。
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

    let mut detail_lines = 0;
    let mut truncated = false;
    for (index, (path, old, new)) in parsed.iter().enumerate() {
        if index >= MAX_BATCH_EDIT_DETAIL_EDITS || detail_lines >= MAX_BATCH_EDIT_DETAIL_LINES {
            truncated = true;
            break;
        }
        out.push((format!("  ── {}", clip(path, 96)), info));
        detail_lines += 1;
        let diff = diff_lines(old, new);
        let remaining = MAX_BATCH_EDIT_DETAIL_LINES.saturating_sub(detail_lines);
        if diff.len() > remaining {
            truncated = true;
        }
        let take = diff.len().min(remaining);
        out.extend(diff.into_iter().take(take));
        detail_lines += take;
    }
    if truncated {
        out.push((
            "  … (details limited, full text in trace)".to_owned(),
            role_color(Role::Muted),
        ));
    }
    out
}

/// 写文件内容预览:保留首尾 `max` 行(每行截断),中间折叠；这样展开时既见入口又见收尾。
pub(crate) fn preview_lines(content: &str, max: usize) -> Vec<(String, Color)> {
    let muted = role_color(Role::Muted);
    let lines: Vec<&str> = content.lines().collect();
    if max == 0 || lines.is_empty() {
        return Vec::new();
    }
    if lines.len() <= max {
        return lines
            .iter()
            .map(|l| (format!("  │ {}", clip(l, 200)), muted))
            .collect();
    }
    let tail = max.min(4);
    let head = max.saturating_sub(tail).max(1);
    let mut out: Vec<(String, Color)> = lines
        .iter()
        .take(head)
        .map(|l| (format!("  │ {}", clip(l, 200)), muted))
        .collect();
    let hidden = lines.len().saturating_sub(head + tail);
    if hidden > 0 {
        out.push((
            format!("  │ … (+{hidden} lines folded; full text in trace)"),
            muted,
        ));
    }
    out.extend(
        lines
            .iter()
            .skip(lines.len().saturating_sub(tail))
            .map(|l| (format!("  │ {}", clip(l, 200)), muted)),
    );
    out
}

/// edit_file 的 git-diff 式呈现:old 行 `-`(红)、new 行 `+`(绿),各截断 + 限行。
pub(crate) fn diff_lines(old: &str, new: &str) -> Vec<(String, Color)> {
    let (red, green) = (role_color(Role::Error), role_color(Role::Success));
    let mut out = Vec::new();
    let cap = 12;
    for l in old.lines().take(cap) {
        out.push((format!("  - {}", clip(l, 200)), red));
    }
    if old.lines().count() > cap {
        out.push(("  - …".to_string(), red));
    }
    for l in new.lines().take(cap) {
        out.push((format!("  + {}", clip(l, 200)), green));
    }
    if new.lines().count() > cap {
        out.push(("  + …".to_string(), green));
    }
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
