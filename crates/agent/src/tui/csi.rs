use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::normalize_key_event;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum NavFeed {
    Hold,
    Event(KeyEvent),
    /// Held leftover (usually `[`) was not CSI; insert it, then handle `then`.
    PrefixThen(String, KeyEvent),
}

pub(crate) fn map_csi_seq(seq: &str) -> Option<KeyCode> {
    match seq {
        "\u{1b}[A" | "[A" | "\u{1b}OA" | "OA" => Some(KeyCode::Up),
        "\u{1b}[B" | "[B" | "\u{1b}OB" | "OB" => Some(KeyCode::Down),
        "\u{1b}[C" | "[C" | "\u{1b}OC" | "OC" => Some(KeyCode::Right),
        "\u{1b}[D" | "[D" | "\u{1b}OD" | "OD" => Some(KeyCode::Left),
        "\u{1b}[H" | "[H" | "\u{1b}[1~" | "[1~" | "\u{1b}[7~" | "[7~" => Some(KeyCode::Home),
        "\u{1b}[F" | "[F" | "\u{1b}[4~" | "[4~" | "\u{1b}[8~" | "[8~" => Some(KeyCode::End),
        "\u{1b}[5~" | "[5~" => Some(KeyCode::PageUp),
        "\u{1b}[6~" | "[6~" => Some(KeyCode::PageDown),
        "\u{1b}[3~" | "[3~" => Some(KeyCode::Delete),
        "\u{1b}[200~" | "[200~" | "\u{1b}[201~" | "[201~" => Some(KeyCode::Null),
        "\u{1b}OP" | "OP" | "\u{1b}[11~" | "[11~" => Some(KeyCode::F(1)),
        "\u{1b}OQ" | "OQ" | "\u{1b}[12~" | "[12~" => Some(KeyCode::F(2)),
        "\u{1b}OR" | "OR" | "\u{1b}[13~" | "[13~" => Some(KeyCode::F(3)),
        "\u{1b}OS" | "OS" | "\u{1b}[14~" | "[14~" => Some(KeyCode::F(4)),
        "\u{1b}[15~" | "[15~" => Some(KeyCode::F(5)),
        "\u{1b}[17~" | "[17~" => Some(KeyCode::F(6)),
        "\u{1b}[18~" | "[18~" => Some(KeyCode::F(7)),
        "\u{1b}[19~" | "[19~" => Some(KeyCode::F(8)),
        "\u{1b}[20~" | "[20~" => Some(KeyCode::F(9)),
        "\u{1b}[21~" | "[21~" => Some(KeyCode::F(10)),
        "\u{1b}[23~" | "[23~" => Some(KeyCode::F(11)),
        "\u{1b}[24~" | "[24~" => Some(KeyCode::F(12)),
        _ => None,
    }
}

fn map_kitty_csi_seq(seq: &str) -> Option<KeyEvent> {
    let body = seq
        .strip_prefix("\u{1b}[")
        .or_else(|| seq.strip_prefix('['))?
        .strip_suffix('u')?;
    let (codepoint, encoded_modifiers) = body.split_once(';')?;
    let codepoint = codepoint.parse::<u32>().ok()?;
    let bits = encoded_modifiers.parse::<u8>().ok()?.saturating_sub(1);
    let code = match codepoint {
        9 => KeyCode::Tab,
        13 => KeyCode::Enter,
        27 => KeyCode::Esc,
        value => KeyCode::Char(char::from_u32(value)?),
    };
    let mut modifiers = KeyModifiers::NONE;
    if bits & 1 != 0 {
        modifiers.insert(KeyModifiers::SHIFT);
    }
    if bits & 2 != 0 {
        modifiers.insert(KeyModifiers::ALT);
    }
    if bits & 4 != 0 {
        modifiers.insert(KeyModifiers::CONTROL);
    }
    Some(KeyEvent::new_with_kind(
        code,
        modifiers,
        KeyEventKind::Press,
    ))
}

fn is_csi_prefix(seq: &str) -> bool {
    // Hold incomplete CSI, including leftover `[200` paste markers and Fn
    // tails. A lone `[` still inserts so typing `[hello]` works.
    matches!(
        seq,
        "\u{1b}"
            | "\u{1b}["
            | "\u{1b}[1"
            | "\u{1b}[11"
            | "\u{1b}[12"
            | "\u{1b}[13"
            | "\u{1b}[14"
            | "\u{1b}[15"
            | "\u{1b}[17"
            | "\u{1b}[18"
            | "\u{1b}[19"
            | "\u{1b}[2"
            | "\u{1b}[3"
            | "\u{1b}[20"
            | "\u{1b}[200"
            | "\u{1b}[201"
            | "\u{1b}[21"
            | "\u{1b}[23"
            | "\u{1b}[24"
            | "\u{1b}[4"
            | "\u{1b}[5"
            | "\u{1b}[6"
            | "\u{1b}[7"
            | "\u{1b}[8"
            | "\u{1b}O"
            | "["
            | "[1"
            | "[11"
            | "[12"
            | "[13"
            | "[14"
            | "[15"
            | "[17"
            | "[18"
            | "[19"
            | "[2"
            | "[3"
            | "[20"
            | "[200"
            | "[201"
            | "[21"
            | "[23"
            | "[24"
            | "[4"
            | "[5"
            | "[6"
            | "[7"
            | "[8"
    ) || seq
        .strip_prefix("\u{1b}[")
        .or_else(|| seq.strip_prefix('['))
        .is_some_and(|body| {
            body.len() <= 16
                && !body.ends_with('u')
                && body
                    .chars()
                    .all(|character| character.is_ascii_digit() || character == ';')
        })
}

/// Decode a Windows/ConPTY CSI spelling (`ESC [ A`, leftover `[A`) into a nav key.
pub(crate) fn feed_nav_key(pending: &mut String, ev: &KeyEvent) -> NavFeed {
    let ev = normalize_key_event(ev);
    if matches!(
        ev.code,
        KeyCode::Up
            | KeyCode::Down
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::PageUp
            | KeyCode::PageDown
    ) {
        pending.clear();
        return NavFeed::Event(ev);
    }
    if ev.code == KeyCode::Esc || matches!(ev.code, KeyCode::Char('\u{1b}')) {
        pending.clear();
        pending.push('\u{1b}');
        return NavFeed::Event(ev);
    }
    let KeyCode::Char(incoming) = ev.code else {
        pending.clear();
        return NavFeed::Event(ev);
    };
    pending.push(incoming);
    if let Some(event) = map_kitty_csi_seq(pending) {
        pending.clear();
        return NavFeed::Event(event);
    }
    if let Some(code) = map_csi_seq(pending) {
        pending.clear();
        return NavFeed::Event(KeyEvent::new_with_kind(
            code,
            ev.modifiers,
            KeyEventKind::Press,
        ));
    }
    if is_csi_prefix(pending) {
        return NavFeed::Hold;
    }
    let mut held = std::mem::take(pending);
    let _ = held.pop();
    if !held.is_empty() && !held.starts_with('\u{1b}') {
        return NavFeed::PrefixThen(held, ev);
    }
    NavFeed::Event(ev)
}

/// If `[` (or `ESC[`) is already in the buffer, completing CSI must navigate
/// and drop the residual characters instead of inserting `A`.
pub(crate) fn csi_insert_override(prefix: &str, incoming: char) -> Option<(KeyCode, usize)> {
    const TAILS: &[&str] = &[
        "\u{1b}[",
        "[",
        "\u{1b}O",
        "\u{1b}[1",
        "[1",
        "\u{1b}[11",
        "[11",
        "\u{1b}[15",
        "[15",
        "\u{1b}[2",
        "[2",
        "\u{1b}[3",
        "[3",
        "\u{1b}[20",
        "[20",
        "\u{1b}[200",
        "[200",
        "\u{1b}[201",
        "[201",
        "\u{1b}[4",
        "[4",
        "\u{1b}[5",
        "[5",
        "\u{1b}[6",
        "[6",
        "\u{1b}[7",
        "[7",
        "\u{1b}[8",
        "[8",
    ];
    for tail in TAILS {
        if prefix.ends_with(tail) {
            let mut seq = String::from(*tail);
            seq.push(incoming);
            if let Some(code) = map_csi_seq(&seq) {
                return Some((code, tail.chars().count()));
            }
        }
    }
    None
}

pub(crate) fn apply_csi_buffer_nav(
    buffer: &str,
    cursor: usize,
    ev: KeyEvent,
) -> Option<(KeyEvent, usize)> {
    let KeyCode::Char(incoming) = ev.code else {
        return None;
    };
    let prefix: String = buffer.chars().take(cursor).collect();
    if incoming == 'u' {
        for marker in ["\u{1b}[", "["] {
            if let Some(start) = prefix.rfind(marker) {
                let tail = &prefix[start..];
                let mut sequence = tail.to_owned();
                sequence.push(incoming);
                if let Some(event) = map_kitty_csi_seq(&sequence) {
                    return Some((event, tail.chars().count()));
                }
            }
        }
    }
    let (code, consume) = csi_insert_override(&prefix, incoming)?;
    Some((
        KeyEvent::new_with_kind(code, ev.modifiers, KeyEventKind::Press),
        consume,
    ))
}

#[cfg(test)]
mod tests {
    use super::{apply_csi_buffer_nav, csi_insert_override, feed_nav_key, map_csi_seq, NavFeed};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn feed_chars(pending: &mut String, chars: &str) -> Vec<KeyCode> {
        let mut codes = Vec::new();
        for ch in chars.chars() {
            match feed_nav_key(pending, &press(KeyCode::Char(ch))) {
                NavFeed::Hold => {}
                NavFeed::Event(ev) => codes.push(ev.code),
                NavFeed::PrefixThen(prefix, ev) => {
                    codes.extend(prefix.chars().map(KeyCode::Char));
                    codes.push(ev.code);
                }
            }
        }
        codes
    }

    #[test]
    fn esc_bracket_letters_become_arrows() {
        for (seq, want) in [
            ("\u{1b}[A", KeyCode::Up),
            ("\u{1b}[B", KeyCode::Down),
            ("\u{1b}[C", KeyCode::Right),
            ("\u{1b}[D", KeyCode::Left),
            ("\u{1b}[H", KeyCode::Home),
            ("\u{1b}[F", KeyCode::End),
            ("\u{1b}[5~", KeyCode::PageUp),
            ("\u{1b}[6~", KeyCode::PageDown),
            ("\u{1b}[3~", KeyCode::Delete),
        ] {
            assert_eq!(map_csi_seq(seq), Some(want), "{seq:?}");
            let mut pending = String::new();
            let mut got = None;
            for ch in seq.chars() {
                let ev = if ch == '\u{1b}' {
                    press(KeyCode::Esc)
                } else {
                    press(KeyCode::Char(ch))
                };
                match feed_nav_key(&mut pending, &ev) {
                    NavFeed::Hold => {}
                    NavFeed::Event(out) if out.code == KeyCode::Esc => {}
                    NavFeed::Event(out) => got = Some(out.code),
                    NavFeed::PrefixThen(_, out) => got = Some(out.code),
                }
            }
            assert_eq!(got, Some(want), "feed {seq:?}");
            assert!(pending.is_empty(), "pending leftover {pending:?}");
        }
    }

    #[test]
    fn leftover_bracket_a_is_up_not_insert() {
        assert_eq!(csi_insert_override("[", 'A'), Some((KeyCode::Up, 1)));
        assert_eq!(csi_insert_override("[", 'B'), Some((KeyCode::Down, 1)));
        assert_eq!(csi_insert_override("[", 'C'), Some((KeyCode::Right, 1)));
        assert_eq!(csi_insert_override("[", 'D'), Some((KeyCode::Left, 1)));
        assert_eq!(csi_insert_override("[5", '~'), Some((KeyCode::PageUp, 2)));
        assert_eq!(csi_insert_override("[6", '~'), Some((KeyCode::PageDown, 2)));
        assert_eq!(csi_insert_override("[3", '~'), Some((KeyCode::Delete, 2)));
        assert_eq!(csi_insert_override("[1", '~'), Some((KeyCode::Home, 2)));
        assert_eq!(csi_insert_override("[4", '~'), Some((KeyCode::End, 2)));
        assert_eq!(csi_insert_override("hello[", 'A'), Some((KeyCode::Up, 1)));
        assert_eq!(csi_insert_override("hello", 'A'), None);
        assert_eq!(csi_insert_override("[", 'h'), None);
        assert_eq!(csi_insert_override("O", 'A'), None);
        assert_eq!(csi_insert_override("GO", 'A'), None);
        assert_eq!(csi_insert_override("\u{1b}O", 'A'), Some((KeyCode::Up, 2)));
    }

    #[test]
    fn held_bracket_then_letter_replays_bracket() {
        let mut pending = String::from("[");
        match feed_nav_key(&mut pending, &press(KeyCode::Char('x'))) {
            NavFeed::PrefixThen(prefix, ev) => {
                assert_eq!(prefix, "[");
                assert_eq!(ev.code, KeyCode::Char('x'));
            }
            other => panic!("{other:?}"),
        }
        assert!(pending.is_empty());
    }

    #[test]
    fn leftover_kitty_ctrl_enter_has_no_literal_residue() {
        let mut pending = String::new();
        let mut event = None;
        for character in "[13;5u".chars() {
            match feed_nav_key(&mut pending, &press(KeyCode::Char(character))) {
                NavFeed::Hold => {}
                NavFeed::Event(output) => event = Some(output),
                NavFeed::PrefixThen(prefix, _) => panic!("literal residue: {prefix:?}"),
            }
        }
        let event = event.expect("complete CSI-u emits one key");
        assert_eq!(event.code, KeyCode::Enter);
        assert!(event.modifiers.contains(KeyModifiers::CONTROL));
        assert!(pending.is_empty());

        let (event, consumed) = apply_csi_buffer_nav(
            "queued[13;5",
            "queued[13;5".chars().count(),
            press(KeyCode::Char('u')),
        )
        .expect("buffer fallback decodes CSI-u");
        assert_eq!(event.code, KeyCode::Enter);
        assert!(event.modifiers.contains(KeyModifiers::CONTROL));
        assert_eq!(consumed, 5);

        let mut pending = String::new();
        let mut alt_i = None;
        for character in "[105;3u".chars() {
            if let NavFeed::Event(output) =
                feed_nav_key(&mut pending, &press(KeyCode::Char(character)))
            {
                alt_i = Some(output);
            }
        }
        let alt_i = alt_i.expect("printable Kitty CSI-u emits one key");
        assert_eq!(alt_i.code, KeyCode::Char('i'));
        assert!(alt_i.modifiers.contains(KeyModifiers::ALT));
    }

    #[test]
    fn bare_bracket_is_not_held_as_csi() {
        let mut pending = String::new();
        let codes = feed_chars(&mut pending, "[hello]");
        assert_eq!(
            codes,
            vec![
                KeyCode::Char('['),
                KeyCode::Char('h'),
                KeyCode::Char('e'),
                KeyCode::Char('l'),
                KeyCode::Char('l'),
                KeyCode::Char('o'),
                KeyCode::Char(']'),
            ]
        );
    }
}
