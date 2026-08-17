use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::{activity_role, role_color, ActivityKind, Role};

/// Mark the user prompt as a first-class, readable transcript line.
pub(crate) fn user_prompt_line(text: &str) -> String {
    format!("¶ ASK · {text}")
}

pub(crate) fn is_user_prompt_line(text: &str) -> bool {
    text.starts_with("¶ ASK · ")
}

pub(crate) fn is_process_noise(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("skip-danger")
        || lower.contains("task submitted")
        || (lower.contains("no reasoning") && !lower.contains("history"))
        || (lower.contains("no tool details") && !lower.contains("history"))
        || lower.contains("starting task")
        || lower.contains("waiting")
        || lower.contains("verifying")
        || lower.contains("settling")
        || lower.contains("completed")
        || (lower.contains("approved") && !lower.contains("not approved"))
        || lower.starts_with("verify: pass")
        || lower.contains("deterministic gate")
        || lower.contains("no stream")
        || text.contains("鉁?")
        || text.contains("鈴?")
        || text.contains("鈿?")
        || text.contains("鈥?")
}

pub(crate) fn is_process_activity(kind: ActivityKind) -> bool {
    matches!(
        kind,
        ActivityKind::Run
            | ActivityKind::Waiting
            | ActivityKind::Verification
            | ActivityKind::Conclusion
            | ActivityKind::Completed
            | ActivityKind::System
    )
}

fn clip_step_lines(text: &str) -> String {
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let first = lines.next().unwrap_or("").trim();
    match lines.next() {
        Some(second) => format!("{first}\n{}", second.trim()),
        None => first.to_owned(),
    }
}

fn format_process_step(text: &str, kind: ActivityKind) -> String {
    let clipped = clip_step_lines(text);
    let mut lines = clipped.lines();
    let first = lines.next().unwrap_or("");
    match lines.next() {
        Some(second) => format!("  {} · {first}\n    {second}", kind.tag()),
        None => format!("  {} · {first}", kind.tag()),
    }
}

/// One collection title plus at most two lines per process step.
pub(crate) fn project_process_bundle(steps: &[(&str, ActivityKind)]) -> (String, Vec<String>) {
    let title = format!("§ ACTA · {} steps", steps.len());
    let body = steps
        .iter()
        .map(|(text, kind)| format_process_step(text, *kind))
        .collect();
    (title, body)
}

pub(crate) fn process_bundle_commit_lines(
    steps: &[(&str, ActivityKind)],
    width: u16,
) -> Vec<Line<'static>> {
    let (title, body) = project_process_bundle(steps);
    let mut lines = vec![Line::default()];
    let hint_style = Style::default().fg(role_color(Role::Muted));
    lines.push(Line::from(vec![
        Span::styled(
            title,
            Style::default()
                .fg(process_bundle_title_style())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  [Ctrl+T activity]", hint_style),
    ]));
    for ((_, kind), step) in steps.iter().zip(body) {
        let step_style = Style::default()
            .fg(role_color(activity_role(*kind)))
            .add_modifier(Modifier::BOLD);
        for line in step.lines() {
            lines.push(Line::from(Span::styled(line.to_owned(), step_style)));
        }
    }
    super::wrap_commit_lines(lines, width)
}

fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{3400}'..='\u{4dbf}'
        | '\u{4e00}'..='\u{9fff}'
        | '\u{f900}'..='\u{faff}'
        | '\u{20000}'..='\u{2ceaf}'
    )
}

fn is_cjk_like_text(c: char) -> bool {
    is_cjk(c)
        || matches!(
            c as u32,
            0x2E80..=0x2EFF
                | 0x2F00..=0x2FDF
                | 0x3000..=0x303F
                | 0x3200..=0x32FF
                | 0x3300..=0x33FF
                | 0xFE30..=0xFE4F
                | 0xFE50..=0xFE6F
                | 0xFF00..=0xFFEF
                | 0x2018
                | 0x2019
                | 0x201C
                | 0x201D
        )
}

/// Drop inter-CJK padding spaces while keeping English words intact.
pub(crate) fn tighten_answer_spacing(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(chars.len());
    let mut out_last_space = false;
    let mut index = 0;

    while index < chars.len() {
        let current = chars[index];
        let is_space = matches!(current, ' ' | '\t' | '\u{00A0}' | '　');

        if is_space {
            if index == 0 || index + 1 >= chars.len() {
                if !out_last_space {
                    out.push(' ');
                    out_last_space = true;
                }
                index += 1;
                continue;
            }

            let prev = chars[index - 1];
            let next = chars[index + 1];

            let keep_word_gap = (prev.is_ascii_alphanumeric() && next.is_ascii_alphanumeric())
                || (is_cjk(prev) && next.is_ascii_alphanumeric())
                || (prev.is_ascii_alphanumeric() && is_cjk(next))
                || (matches!(
                    prev,
                    ',' | '.' | ';' | ':' | '，' | '。' | '；' | '：' | '？' | '！'
                ) && next.is_ascii_alphanumeric());

            if !keep_word_gap
                && (is_cjk_like_text(prev)
                    || is_cjk_like_text(next)
                    || matches!(
                        prev,
                        '(' | ')'
                            | '（'
                            | '）'
                            | '「'
                            | '」'
                            | '『'
                            | '』'
                            | '【'
                            | '】'
                            | '《'
                            | '》'
                            | ','
                            | '.'
                            | ';'
                            | ':'
                            | '，'
                            | '。'
                            | '；'
                            | '：'
                            | '！'
                            | '？'
                    )
                    || matches!(
                        next,
                        '(' | ')'
                            | '（'
                            | '）'
                            | '「'
                            | '」'
                            | '『'
                            | '』'
                            | '【'
                            | '】'
                            | '《'
                            | '》'
                            | ','
                            | '.'
                            | ';'
                            | ':'
                            | '，'
                            | '。'
                            | '；'
                            | '：'
                            | '！'
                            | '？'
                    ))
            {
                index += 1;
                continue;
            }

            if !out_last_space {
                out.push(' ');
                out_last_space = true;
            }
            index += 1;
            continue;
        }

        out.push(current);
        out_last_space = false;
        index += 1;
    }
    out
}

pub(crate) fn process_bundle_title_style() -> ratatui::style::Color {
    role_color(Role::Label)
}

#[cfg(test)]
mod tests {
    use super::ActivityKind;
    use super::{project_process_bundle, tighten_answer_spacing, user_prompt_line};

    #[test]
    fn user_prompt_is_special_and_readable() {
        let line = user_prompt_line("把这段话说明白");
        assert!(line.starts_with("¶ ASK · "));
        assert!(line.contains("把这段话说明白"));
        assert!(!line.contains('?'));
    }

    #[test]
    fn process_bundle_has_one_title_and_two_line_steps() {
        let steps = [
            ("starting task", ActivityKind::Run),
            (
                "waiting\nno reasoning\nextra discarded line",
                ActivityKind::Waiting,
            ),
            (
                "verifying checker\nsecond detail\nthird ignored",
                ActivityKind::Verification,
            ),
        ];
        let (title, body) = project_process_bundle(&steps);
        assert_eq!(title, "§ ACTA · 3 steps");
        assert_eq!(body.len(), 3);
        assert!(body[0].starts_with("  RUN · "));
        assert_eq!(body[1].lines().count(), 2);
        assert_eq!(body[2].lines().count(), 2);
        assert!(body[1].contains("waiting"));
        assert!(body[1].contains("no reasoning"));
        assert!(!body[1].contains("extra discarded"));
    }

    #[test]
    fn tighten_drops_cjk_padding_keeps_english_words() {
        let spaced = "这 是 一 段 被 拉 稀 的 中 文";
        let tight = tighten_answer_spacing(spaced);
        assert_eq!(tight, "这是一段被拉稀的中文");
        assert_eq!(
            tighten_answer_spacing("keep English words intact"),
            "keep English words intact"
        );
        assert_eq!(
            tighten_answer_spacing("中文 and English"),
            "中文 and English"
        );
    }
}
