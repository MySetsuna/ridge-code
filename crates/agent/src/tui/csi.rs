use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::normalize_key_event;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum NavFeed {
    Hold,
    Event(KeyEvent),
    /// Held leftover (usually `[`) was not CSI; insert it, then handle `then`.
    PrefixThen(String, KeyEvent),
    /// An explicit escape prefix was not CSI; dispatch Escape, then replay the
    /// literal tail and current key without leaking the escape byte.
    EscapeThen(String, KeyEvent),
}

pub(crate) fn map_csi_seq(seq: &str) -> Option<KeyCode> {
    match seq {
        "\u{1b}[A" | "\u{1b}OA" => Some(KeyCode::Up),
        "\u{1b}[B" | "\u{1b}OB" => Some(KeyCode::Down),
        "\u{1b}[C" | "\u{1b}OC" => Some(KeyCode::Right),
        "\u{1b}[D" | "\u{1b}OD" => Some(KeyCode::Left),
        "\u{1b}[H" | "\u{1b}[1~" | "\u{1b}[7~" => Some(KeyCode::Home),
        "\u{1b}[F" | "\u{1b}[4~" | "\u{1b}[8~" => Some(KeyCode::End),
        "\u{1b}[5~" => Some(KeyCode::PageUp),
        "\u{1b}[6~" => Some(KeyCode::PageDown),
        "\u{1b}[3~" => Some(KeyCode::Delete),
        // VT/xterm Shift-Tab is CSI Z; a few terminals use the modifier
        // parameter spelling instead. Decode both before the literal replay
        // path so `[Z` cannot leak into the prompt.
        "\u{1b}[Z" | "\u{1b}[1;2Z" => Some(KeyCode::BackTab),
        "\u{1b}[200~" | "\u{1b}[201~" => Some(KeyCode::Null),
        "\u{1b}OP" | "\u{1b}[11~" => Some(KeyCode::F(1)),
        "\u{1b}OQ" | "\u{1b}[12~" => Some(KeyCode::F(2)),
        "\u{1b}OR" | "\u{1b}[13~" => Some(KeyCode::F(3)),
        "\u{1b}OS" | "\u{1b}[14~" => Some(KeyCode::F(4)),
        "\u{1b}[15~" => Some(KeyCode::F(5)),
        "\u{1b}[17~" => Some(KeyCode::F(6)),
        "\u{1b}[18~" => Some(KeyCode::F(7)),
        "\u{1b}[19~" => Some(KeyCode::F(8)),
        "\u{1b}[20~" => Some(KeyCode::F(9)),
        "\u{1b}[21~" => Some(KeyCode::F(10)),
        "\u{1b}[23~" => Some(KeyCode::F(11)),
        "\u{1b}[24~" => Some(KeyCode::F(12)),
        _ => None,
    }
}

fn map_bare_csi_seq(seq: &str) -> Option<KeyCode> {
    match seq {
        // ConPTY/legacy input can strip ESC from the standard VT Shift-Tab
        // sequence. This is the only non-numeric bare CSI accepted here;
        // ordinary `[A`/`[B` text remains literal.
        "[Z" | "[1;2Z" => Some(KeyCode::BackTab),
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

const BRACKETED_PASTE_ENDS: &[&str] = &["\u{1b}[201~", "[201~"];

/// Convert a raw key event that arrived instead of `Event::Paste` into one
/// byte of bracketed-paste payload.  Windows ConPTY can surface C0 bytes as
/// Ctrl-letter key events, so recover those bytes before sanitization.
pub(crate) fn bracketed_paste_char(ev: &KeyEvent) -> Option<char> {
    match ev.code {
        KeyCode::Char(c)
            if ev.modifiers.contains(KeyModifiers::CONTROL)
                && !ev
                    .modifiers
                    .contains(KeyModifiers::ALT | KeyModifiers::SUPER) =>
        {
            let upper = c.to_ascii_uppercase();
            match upper {
                '@' => Some('\0'),
                'A'..='Z' => Some((upper as u8 - b'A' + 1) as char),
                '[' => Some('\u{1b}'),
                '\\' => Some('\u{1c}'),
                ']' => Some('\u{1d}'),
                '^' => Some('\u{1e}'),
                '_' => Some('\u{1f}'),
                '?' => Some('\u{7f}'),
                _ => Some(c),
            }
        }
        KeyCode::Char(c) => Some(c),
        KeyCode::Esc => Some('\u{1b}'),
        KeyCode::Enter => Some('\r'),
        KeyCode::Tab => Some('\t'),
        KeyCode::Backspace => Some('\x08'),
        KeyCode::Delete => Some('\x7f'),
        _ => None,
    }
}

/// Append one raw key to a bracketed-paste stream.  Return the payload once
/// the protocol end marker arrives; the marker itself never reaches input.
pub(crate) fn push_bracketed_paste_key(buffer: &mut String, ev: &KeyEvent) -> Option<String> {
    buffer.push(bracketed_paste_char(ev)?);
    let payload_len = BRACKETED_PASTE_ENDS
        .iter()
        .find_map(|marker| buffer.strip_suffix(marker).map(str::len))?;
    let payload = buffer[..payload_len].to_string();
    buffer.clear();
    Some(payload)
}

pub(crate) fn is_bracketed_paste_start(pending: &str, ev: &KeyEvent, decoded: &KeyEvent) -> bool {
    matches!(pending, "\u{1b}[200" | "[200")
        && ev.code == KeyCode::Char('~')
        && decoded.code == KeyCode::Null
}

fn is_csi_prefix(seq: &str) -> bool {
    // Hold explicit CSI and the numeric CSI-u spelling some Windows hosts
    // surface without the leading ESC. Ordinary text such as `[A` remains
    // literal because its body is not numeric/semicolon-only.
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
            | "\u{1b}[Z"
            | "\u{1b}O"
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
        || matches!(seq, "[" | "[1;2")
}

/// Decode an explicit CSI spelling (`ESC [ A`) into a nav key.
pub(crate) fn feed_nav_key(pending: &mut String, ev: &KeyEvent) -> NavFeed {
    feed_nav_key_with_kitty(pending, ev, true)
}

/// Feed one key while optionally accepting a bare Kitty CSI-u sequence.
/// Explicit `ESC[` sequences remain valid regardless of the flag; the bare
/// form is enabled only after the terminal advertised Kitty keyboard mode.
pub(crate) fn feed_nav_key_with_kitty(
    pending: &mut String,
    ev: &KeyEvent,
    allow_bare_kitty: bool,
) -> NavFeed {
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
        let held = std::mem::take(pending);
        if held.is_empty() || held.starts_with('\u{1b}') {
            return NavFeed::Event(ev);
        }
        return NavFeed::PrefixThen(held, ev);
    }
    if ev.code == KeyCode::Esc || matches!(ev.code, KeyCode::Char('\u{1b}')) {
        let held = std::mem::take(pending);
        if held.is_empty() {
            pending.push('\u{1b}');
            return NavFeed::Hold;
        }
        if let Some(prefix) = held.strip_prefix('\u{1b}') {
            // A second physical Escape resolves an orphaned Escape prefix;
            // replaying both would close a panel twice.
            if prefix.is_empty() && ev.code == KeyCode::Esc {
                return NavFeed::Event(ev);
            }
            return NavFeed::EscapeThen(prefix.to_string(), ev);
        }
        return NavFeed::PrefixThen(held, ev);
    }
    // Legacy terminals encode Alt+Enter as ESC followed by CR/LF. Treat the
    // immediate pair as one semantic newline; an isolated Escape still waits
    // for the normal timeout before being dispatched as Escape.
    if pending.as_str() == "\u{1b}" && ev.code == KeyCode::Enter && ev.modifiers.is_empty() {
        pending.clear();
        return NavFeed::Event(KeyEvent::new_with_kind(
            KeyCode::Enter,
            KeyModifiers::ALT,
            KeyEventKind::Press,
        ));
    }
    let KeyCode::Char(incoming) = ev.code else {
        let held = std::mem::take(pending);
        if held.is_empty() {
            return NavFeed::Event(ev);
        }
        if let Some(prefix) = held.strip_prefix('\u{1b}') {
            return NavFeed::EscapeThen(prefix.to_string(), ev);
        }
        return NavFeed::PrefixThen(held, ev);
    };
    pending.push(incoming);
    if allow_bare_kitty || pending.starts_with('\u{1b}') {
        if let Some(event) = map_kitty_csi_seq(pending) {
            pending.clear();
            return NavFeed::Event(event);
        }
    }
    if pending.starts_with('\u{1b}') {
        if let Some(code) = map_csi_seq(pending) {
            pending.clear();
            return NavFeed::Event(KeyEvent::new_with_kind(
                code,
                ev.modifiers,
                KeyEventKind::Press,
            ));
        }
    }
    if let Some(code) = map_bare_csi_seq(pending) {
        pending.clear();
        return NavFeed::Event(KeyEvent::new_with_kind(
            code,
            ev.modifiers,
            KeyEventKind::Press,
        ));
    }
    // Some Windows ConPTY hosts strip the leading ESC before surfacing a
    // bracketed-paste marker. These exact markers stay safe to accept while
    // bare Kitty CSI-u remains gated.
    if matches!(pending.as_str(), "[200~" | "[201~") {
        pending.clear();
        return NavFeed::Event(KeyEvent::new_with_kind(
            KeyCode::Null,
            ev.modifiers,
            KeyEventKind::Press,
        ));
    }
    if is_csi_prefix(pending) {
        return NavFeed::Hold;
    }
    let mut held = std::mem::take(pending);
    let _ = held.pop();
    if held.starts_with('\u{1b}') {
        return NavFeed::EscapeThen(held.chars().skip(1).collect(), ev);
    }
    if !held.is_empty() {
        return NavFeed::PrefixThen(held, ev);
    }
    NavFeed::Event(ev)
}

/// If an explicit `ESC[` is already in the buffer, completing CSI must
/// navigate and drop the residual characters instead of inserting `A`.
pub(crate) fn csi_insert_override(prefix: &str, incoming: char) -> Option<(KeyCode, usize)> {
    const TAILS: &[&str] = &[
        "\u{1b}[",
        "\u{1b}O",
        "\u{1b}[1",
        "\u{1b}[11",
        "\u{1b}[15",
        "\u{1b}[2",
        "\u{1b}[3",
        "\u{1b}[20",
        "\u{1b}[200",
        "\u{1b}[201",
        "\u{1b}[4",
        "\u{1b}[5",
        "\u{1b}[6",
        "\u{1b}[7",
        "\u{1b}[8",
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
    for tail in ["[", "[1;2"] {
        if prefix == tail {
            let mut seq = String::from(tail);
            seq.push(incoming);
            if let Some(code) = map_bare_csi_seq(&seq) {
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
    allow_bare_kitty: bool,
) -> Option<(KeyEvent, usize)> {
    let KeyCode::Char(incoming) = ev.code else {
        return None;
    };
    let prefix: String = buffer.chars().take(cursor).collect();
    if incoming == 'u' {
        let markers: &[&str] = if allow_bare_kitty {
            &["\u{1b}[", "["]
        } else {
            &["\u{1b}["]
        };
        for marker in markers {
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

/// Time out an incomplete escape/literal prefix without dropping user text.
/// Returns `(had_escape, literal_tail)`; the caller dispatches `Esc` first when
/// `had_escape` is true, then inserts the literal tail.
pub(crate) fn flush_pending_literal(pending: &mut String) -> Option<(bool, String)> {
    let held = std::mem::take(pending);
    if held.is_empty() {
        return None;
    }
    let had_escape = held.starts_with('\u{1b}');
    let literal = held.strip_prefix('\u{1b}').unwrap_or(&held).to_string();
    Some((had_escape, literal))
}

#[cfg(test)]
mod tests {
    use super::{
        apply_csi_buffer_nav, bracketed_paste_char, csi_insert_override, feed_nav_key,
        feed_nav_key_with_kitty, flush_pending_literal, map_csi_seq, push_bracketed_paste_key,
        NavFeed,
    };
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn raw_bracketed_paste_stream_stops_at_end_marker() {
        let mut buffer = String::new();
        let mut payload = None;
        for c in "a\u{1b}[31mb\u{1b}]0;title\u{7}c\u{1b}[201~".chars() {
            let ev = if c == '\u{1b}' {
                press(KeyCode::Esc)
            } else {
                press(KeyCode::Char(c))
            };
            payload = push_bracketed_paste_key(&mut buffer, &ev).or(payload);
        }
        assert_eq!(payload.as_deref(), Some("a\u{1b}[31mb\u{1b}]0;title\u{7}c"));
        assert!(buffer.is_empty());
        assert_eq!(
            bracketed_paste_char(&KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL,)),
            Some('\u{7}')
        );
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
                NavFeed::EscapeThen(prefix, ev) => {
                    codes.push(KeyCode::Esc);
                    codes.extend(prefix.chars().map(KeyCode::Char));
                    codes.push(ev.code);
                }
            }
        }
        codes
    }

    #[test]
    fn bare_bracketed_markers_decode_without_bare_kitty_gate() {
        for marker in ["[200~", "[201~"] {
            let mut pending = String::new();
            let mut got = Vec::new();
            for c in marker.chars() {
                match feed_nav_key_with_kitty(&mut pending, &press(KeyCode::Char(c)), false) {
                    NavFeed::Hold => {}
                    NavFeed::Event(ev) => got.push(ev.code),
                    NavFeed::PrefixThen(prefix, ev) => {
                        got.extend(prefix.chars().map(KeyCode::Char));
                        got.push(ev.code);
                    }
                    NavFeed::EscapeThen(prefix, ev) => {
                        got.push(KeyCode::Esc);
                        got.extend(prefix.chars().map(KeyCode::Char));
                        got.push(ev.code);
                    }
                }
            }
            assert_eq!(got, vec![KeyCode::Null], "marker {marker:?}");
            assert!(pending.is_empty());
        }
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
                    NavFeed::Event(out) => got = Some(out.code),
                    NavFeed::PrefixThen(_, out) => got = Some(out.code),
                    NavFeed::EscapeThen(_, out) => got = Some(out.code),
                }
            }
            assert_eq!(got, Some(want), "feed {seq:?}");
            assert!(pending.is_empty(), "pending leftover {pending:?}");
        }
    }

    #[test]
    fn esc_bracket_shift_tab_has_no_literal_residue() {
        for seq in ["\u{1b}[Z", "\u{1b}[1;2Z"] {
            assert_eq!(map_csi_seq(seq), Some(KeyCode::BackTab), "{seq:?}");
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
                    NavFeed::Event(out) => got = Some(out.code),
                    NavFeed::PrefixThen(prefix, out) | NavFeed::EscapeThen(prefix, out) => {
                        panic!("literal residue for {seq:?}: {prefix:?} + {:?}", out.code)
                    }
                }
            }
            assert_eq!(got, Some(KeyCode::BackTab), "feed {seq:?}");
            assert!(pending.is_empty(), "pending leftover {pending:?}");
        }
        // ConPTY can strip ESC, but the standard Shift-Tab marker remains
        // unambiguous at this boundary and must take the same path.
        for seq in ["[Z", "[1;2Z"] {
            let mut pending = String::new();
            let mut got = None;
            for ch in seq.chars() {
                match feed_nav_key_with_kitty(&mut pending, &press(KeyCode::Char(ch)), false) {
                    NavFeed::Hold => {}
                    NavFeed::Event(out) => got = Some(out.code),
                    NavFeed::PrefixThen(prefix, out) | NavFeed::EscapeThen(prefix, out) => {
                        panic!(
                            "bare literal residue for {seq:?}: {prefix:?} + {:?}",
                            out.code
                        )
                    }
                }
            }
            assert_eq!(got, Some(KeyCode::BackTab), "bare feed {seq:?}");
            assert!(pending.is_empty(), "bare pending leftover {pending:?}");
        }
    }

    #[test]
    fn legacy_escape_cr_lf_maps_to_alt_enter() {
        for raw in ['\r', '\n'] {
            let mut pending = String::new();
            assert_eq!(
                feed_nav_key(&mut pending, &press(KeyCode::Esc)),
                NavFeed::Hold
            );
            let decoded = match feed_nav_key(&mut pending, &press(KeyCode::Char(raw))) {
                NavFeed::Event(event) => event,
                other => panic!("legacy Alt+Enter residue for {raw:?}: {other:?}"),
            };
            assert_eq!(decoded.code, KeyCode::Enter);
            assert_eq!(decoded.modifiers, KeyModifiers::ALT);
            assert!(pending.is_empty());
        }
    }

    #[test]
    fn bare_bracket_text_is_not_csi() {
        assert_eq!(map_csi_seq("[A"), None);
        assert_eq!(map_csi_seq("[5~"), None);
        assert_eq!(csi_insert_override("[", 'A'), None);
        assert_eq!(csi_insert_override("[", 'B'), None);
        assert_eq!(csi_insert_override("[", 'C'), None);
        assert_eq!(csi_insert_override("[", 'D'), None);
        assert_eq!(csi_insert_override("[5", '~'), None);
        assert_eq!(csi_insert_override("[6", '~'), None);
        assert_eq!(csi_insert_override("[3", '~'), None);
        assert_eq!(csi_insert_override("[1", '~'), None);
        assert_eq!(csi_insert_override("[4", '~'), None);
        assert_eq!(csi_insert_override("hello[", 'A'), None);
        assert_eq!(csi_insert_override("\u{1b}[", 'A'), Some((KeyCode::Up, 2)));
        assert_eq!(
            csi_insert_override("\u{1b}[5", '~'),
            Some((KeyCode::PageUp, 3))
        );
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
    fn repeated_escape_resolves_orphan_without_double_dispatch() {
        let mut pending = "\u{1b}".to_string();
        match feed_nav_key(&mut pending, &press(KeyCode::Esc)) {
            NavFeed::Event(event) => assert_eq!(event.code, KeyCode::Esc),
            other => panic!("{other:?}"),
        }
        assert!(pending.is_empty());
    }

    #[test]
    fn leftover_kitty_ctrl_enter_has_no_literal_residue() {
        let mut pending = String::new();
        let mut event = None;
        for character in "\u{1b}[13;5u".chars() {
            let key = if character == '\u{1b}' {
                press(KeyCode::Esc)
            } else {
                press(KeyCode::Char(character))
            };
            match feed_nav_key(&mut pending, &key) {
                NavFeed::Hold => {}
                NavFeed::Event(output) => event = Some(output),
                NavFeed::PrefixThen(prefix, _) => panic!("literal residue: {prefix:?}"),
                NavFeed::EscapeThen(prefix, _) => panic!("literal residue: {prefix:?}"),
            }
        }
        let event = event.expect("complete CSI-u emits one key");
        assert_eq!(event.code, KeyCode::Enter);
        assert!(event.modifiers.contains(KeyModifiers::CONTROL));
        assert!(pending.is_empty());

        let (event, consumed) = apply_csi_buffer_nav(
            "queued\u{1b}[13;5",
            "queued\u{1b}[13;5".chars().count(),
            press(KeyCode::Char('u')),
            false,
        )
        .expect("buffer fallback decodes explicit CSI-u");
        assert_eq!(event.code, KeyCode::Enter);
        assert!(event.modifiers.contains(KeyModifiers::CONTROL));
        assert_eq!(consumed, 6);

        let (event, consumed) = apply_csi_buffer_nav(
            "queued[13;5",
            "queued[13;5".chars().count(),
            press(KeyCode::Char('u')),
            true,
        )
        .expect("advertised Kitty host decodes bare CSI-u");
        assert_eq!(event.code, KeyCode::Enter);
        assert!(event.modifiers.contains(KeyModifiers::CONTROL));
        assert_eq!(consumed, 5);

        assert!(apply_csi_buffer_nav(
            "literal[13;5",
            "literal[13;5".chars().count(),
            press(KeyCode::Char('u')),
            false,
        )
        .is_none());

        let mut pending = String::new();
        let mut alt_i = None;
        for character in "\u{1b}[105;3u".chars() {
            let key = if character == '\u{1b}' {
                press(KeyCode::Esc)
            } else {
                press(KeyCode::Char(character))
            };
            if let NavFeed::Event(output) = feed_nav_key(&mut pending, &key) {
                alt_i = Some(output);
            }
        }
        let alt_i = alt_i.expect("printable Kitty CSI-u emits one key");
        assert_eq!(alt_i.code, KeyCode::Char('i'));
        assert!(alt_i.modifiers.contains(KeyModifiers::ALT));
    }

    #[test]
    fn host_without_escape_preserves_kitty_ctrl_enter() {
        let mut pending = String::new();
        let mut event = None;
        for character in "[13;5u".chars() {
            match feed_nav_key(&mut pending, &press(KeyCode::Char(character))) {
                NavFeed::Hold => {}
                NavFeed::Event(output) => event = Some(output),
                NavFeed::PrefixThen(prefix, _) | NavFeed::EscapeThen(prefix, _) => {
                    panic!("literal residue: {prefix:?}")
                }
            }
        }
        let event = event.expect("bare CSI-u emits one key");
        assert_eq!(event.code, KeyCode::Enter);
        assert!(event.modifiers.contains(KeyModifiers::CONTROL));
        assert!(pending.is_empty());
    }

    #[test]
    fn unadvertised_bare_kitty_stays_literal() {
        let mut pending = String::new();
        let mut codes = Vec::new();
        for character in "[13;5u".chars() {
            match feed_nav_key_with_kitty(&mut pending, &press(KeyCode::Char(character)), false) {
                NavFeed::Hold => {}
                NavFeed::Event(output) => codes.push(output.code),
                NavFeed::PrefixThen(prefix, output) => {
                    codes.extend(prefix.chars().map(KeyCode::Char));
                    codes.push(output.code);
                }
                NavFeed::EscapeThen(prefix, output) => {
                    codes.push(KeyCode::Esc);
                    codes.extend(prefix.chars().map(KeyCode::Char));
                    codes.push(output.code);
                }
            }
        }
        assert_eq!(
            codes,
            "[13;5u".chars().map(KeyCode::Char).collect::<Vec<_>>()
        );
        assert!(pending.is_empty());
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

    #[test]
    fn bare_csi_arrow_text_stays_literal() {
        let mut pending = String::new();
        assert_eq!(
            feed_chars(&mut pending, "[A"),
            vec![KeyCode::Char('['), KeyCode::Char('A')]
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn bare_bracket_before_non_character_key_is_replayed() {
        let mut pending = String::new();
        assert_eq!(
            feed_nav_key(&mut pending, &press(KeyCode::Char('['))),
            NavFeed::Hold
        );
        assert_eq!(
            feed_nav_key(&mut pending, &press(KeyCode::Enter)),
            NavFeed::PrefixThen("[".to_string(), press(KeyCode::Enter))
        );
        assert!(pending.is_empty());

        assert_eq!(
            feed_nav_key(&mut pending, &press(KeyCode::Char('['))),
            NavFeed::Hold
        );
        assert_eq!(
            feed_nav_key(&mut pending, &press(KeyCode::Tab)),
            NavFeed::PrefixThen("[".to_string(), press(KeyCode::Tab))
        );
    }

    #[test]
    fn bare_bracket_before_navigation_is_replayed() {
        let mut pending = String::new();
        assert_eq!(
            feed_nav_key(&mut pending, &press(KeyCode::Char('['))),
            NavFeed::Hold
        );
        assert_eq!(
            feed_nav_key(&mut pending, &press(KeyCode::Up)),
            NavFeed::PrefixThen("[".to_string(), press(KeyCode::Up))
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn incomplete_prefix_timeout_preserves_literal_tail() {
        let mut explicit = "\u{1b}[13;5".to_string();
        assert_eq!(
            flush_pending_literal(&mut explicit),
            Some((true, "[13;5".to_string()))
        );
        let mut bare = "[13;5".to_string();
        assert_eq!(
            flush_pending_literal(&mut bare),
            Some((false, "[13;5".to_string()))
        );
    }
}
