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
/// 读文件只显路径(不倒内容)、写文件显首几行预览、改文件显 ± 着色 diff(形如 git diff)。
/// **全文/全量在 run trace**;inline 已提交行不可回改,故预览截断并标注(替代「展开」)。
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
                    "read_file" => vec![(format!("  ⋯ 读 {}", arg("path")), info)],
                    "write_file" => {
                        let mut out = vec![(format!("  ⋯ 写 {}", arg("path")), info)];
                        out.extend(preview_lines(arg("contents"), 8));
                        out
                    }
                    "edit_file" => {
                        let mut out = vec![(format!("  ⋯ 改 {}", arg("path")), info)];
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
                    "search" => vec![(format!("  ⋯ 搜 {}", arg("pattern")), info)],
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
                    out.push((format!("  │ … (+{} 行,全文见 trace)", lines.len() - 9), err));
                }
                return out;
            }
            let ok = role_color(Role::Success);
            if name == "read_file" {
                return vec![(format!("  ✓ 读完 ({} 字)", obs.chars().count()), ok)];
            }
            let head = clip(obs.lines().next().unwrap_or(""), 200);
            return vec![(format!("  ✓ {name}: {head}"), ok)];
        }
    }
    vec![(format_event_plain(m), event_color(m))]
}

/// 将结构化工具事件转换为可折叠块；详情仅在 live 视口中按需显示。
pub(crate) fn tool_preview(m: &str) -> Option<ToolBlock> {
    let tool_call = m
        .strip_prefix("reason#")
        .map(|rest| rest.contains(": tool_call "))
        .unwrap_or(false);
    let observation = m
        .strip_prefix("act: ")
        .map(|rest| rest.contains(" -> "))
        .unwrap_or(false);
    if tool_call || observation {
        ToolBlock::from_lines(summarize_event(m))
    } else {
        None
    }
}

const MAX_BATCH_EDIT_SUMMARY_PATHS: usize = 3;
const MAX_BATCH_EDIT_DETAIL_EDITS: usize = 4;
const MAX_BATCH_EDIT_DETAIL_LINES: usize = 18;

/// 批量编辑的有界投影：折叠态显文件范围，展开态显前几处 ± 预览。
/// 只读 tool-call 参数，不访问磁盘；成功/失败仍由对应 `act:` 观察行裁决。
pub(crate) fn apply_edits_summary(args: &serde_json::Value, info: Color) -> Vec<(String, Color)> {
    let Some(edits) = args.get("edits").and_then(|value| value.as_array()) else {
        return vec![("  ⋯ 批量改 (缺少 edits)".to_owned(), info)];
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
        return vec![("  ⋯ 批量改 (无有效 edits)".to_owned(), info)];
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
            "… +{} 个",
            paths.len() - MAX_BATCH_EDIT_SUMMARY_PATHS
        ));
    }
    let mut out = vec![(
        format!(
            "  ⋯ 批量改 {} 文件 / {} 处: {}",
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
            "  … (详情已限量，全文见 trace)".to_owned(),
            role_color(Role::Muted),
        ));
    }
    out
}

/// 写文件内容预览:首 `max` 行(每行截断),超出标注剩余行数(全文见 trace)。
pub(crate) fn preview_lines(content: &str, max: usize) -> Vec<(String, Color)> {
    let muted = role_color(Role::Muted);
    let lines: Vec<&str> = content.lines().collect();
    let mut out: Vec<(String, Color)> = lines
        .iter()
        .take(max)
        .map(|l| (format!("  │ {}", clip(l, 200)), muted))
        .collect();
    if lines.len() > max {
        out.push((
            format!("  │ … (+{} 行,全文见 trace)", lines.len() - max),
            muted,
        ));
    }
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
/// 事件行配色:经语义角色取色(iter-28 收口);终答用 White(具名 ANSI,非角色)。
pub(crate) fn event_color(m: &str) -> Color {
    if m.starts_with("verify: PASS") {
        role_color(Role::Success)
    } else if m.starts_with("verify: FAIL") {
        role_color(Role::Error)
    } else if m.starts_with("act:") {
        role_color(Role::Warn)
    } else if is_final_event(m) {
        Color::White
    } else {
        role_color(Role::Info)
    }
}
