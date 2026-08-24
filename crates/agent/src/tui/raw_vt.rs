//! Incremental Windows VT input decoding.
//!
//! The byte reader is intentionally separate from Crossterm's event reader.
//! Once a raw reader owns stdin, this parser is the only place that turns the
//! byte stream into Crossterm events.  It never classifies a burst by timing.

use std::time::{Duration, Instant};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

const ESC: u8 = 0x1b;
const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";
const MAX_PENDING_BYTES: usize = 128;
const MAX_PASTE_BYTES: usize = 4 * 1024 * 1024;
const CRLF_DEDUPE_MS: u64 = 25;

/// Incremental, bounded parser for a VT byte stream.
#[derive(Debug, Default)]
pub(crate) struct RawVtParser {
    pending: Vec<u8>,
    paste: Option<Vec<u8>>,
    paste_marker: Vec<u8>,
    pending_since: Option<Instant>,
    last_cr_since: Option<Instant>,
}

impl RawVtParser {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Feed one arbitrary read chunk.  A sequence split at any byte boundary
    /// is held until enough bytes arrive; completed sequences emit exactly one
    /// event and are removed from the parser state.
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Vec<Event> {
        let mut events = Vec::new();
        if bytes.is_empty() {
            return events;
        }
        self.feed_bytes(bytes, &mut events);
        self.parse_pending(&mut events);
        self.enforce_pending_bound(&mut events);
        events
    }

    /// Replay an incomplete escape sequence without dropping ordinary text.
    /// The reader calls this after its bounded idle window; tests can use it
    /// directly for deterministic recovery checks.
    pub(crate) fn flush(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        if let Some(mut body) = self.paste.take() {
            let marker = std::mem::take(&mut self.paste_marker);
            append_bounded(&mut body, &marker);
            if !body.is_empty() {
                events.push(Event::Paste(bytes_to_text(&body)));
            }
        }
        if !self.pending.is_empty() {
            let bytes = std::mem::take(&mut self.pending);
            if !is_string_control_prefix(&bytes) {
                replay_literal(&bytes, &mut events);
            }
        }
        self.pending_since = None;
        self.last_cr_since = None;
        events
    }

    /// Flush an incomplete sequence after a bounded wait.  A normal parser
    /// call remains chunk-driven; the timeout exists only for a lone Escape or
    /// malformed sequence that otherwise has no next byte to disambiguate it.
    pub(crate) fn flush_expired(&mut self, now: Instant, timeout: Duration) -> Vec<Event> {
        if self.last_cr_since.is_some_and(|started| {
            now.saturating_duration_since(started).as_millis() as u64 >= CRLF_DEDUPE_MS
        }) {
            self.last_cr_since = None;
        }
        // A bracketed paste has an explicit terminator.  Never use the short
        // escape disambiguation timeout to emit a partial paste; only EOF or
        // a fatal reader error may flush it.
        if self.paste.is_some() {
            return Vec::new();
        }
        let Some(started) = self.pending_since else {
            return Vec::new();
        };
        if now.saturating_duration_since(started) < timeout {
            return Vec::new();
        }
        self.flush()
    }

    fn feed_bytes(&mut self, bytes: &[u8], events: &mut Vec<Event>) {
        let mut offset = 0;
        while offset < bytes.len() {
            if self.paste.is_some() {
                offset += self.consume_paste(&bytes[offset..], events);
                continue;
            }
            self.pending.extend_from_slice(&bytes[offset..]);
            return;
        }
    }

    fn consume_paste(&mut self, bytes: &[u8], events: &mut Vec<Event>) -> usize {
        let mut offset = 0;
        while offset < bytes.len() {
            self.paste_marker.push(bytes[offset]);
            if BRACKETED_PASTE_END.starts_with(&self.paste_marker) {
                offset += 1;
                if self.paste_marker == BRACKETED_PASTE_END {
                    let body = self.paste.take().unwrap_or_default();
                    self.paste_marker.clear();
                    events.push(Event::Paste(bytes_to_text(&body)));
                    return offset;
                }
                continue;
            }

            let suffix_len = (1..self.paste_marker.len())
                .rev()
                .find(|length| {
                    BRACKETED_PASTE_END
                        .starts_with(&self.paste_marker[self.paste_marker.len() - length..])
                })
                .unwrap_or(0);
            let flush_len = self.paste_marker.len() - suffix_len;
            let body_bytes = self.paste_marker.drain(..flush_len).collect::<Vec<_>>();
            if let Some(body) = self.paste.as_mut() {
                append_bounded(body, &body_bytes);
            }
            offset += 1;
        }
        offset
    }

    fn parse_pending(&mut self, events: &mut Vec<Event>) {
        loop {
            if self.pending.is_empty() {
                self.pending_since = None;
                return;
            }
            if self.pending[0] == ESC {
                match self.parse_escape(events) {
                    ParseResult::Complete => continue,
                    ParseResult::Incomplete => {
                        self.pending_since.get_or_insert_with(Instant::now);
                        return;
                    }
                }
            }
            match self.parse_plain(events) {
                ParseResult::Complete => continue,
                ParseResult::Incomplete => {
                    self.pending_since.get_or_insert_with(Instant::now);
                    return;
                }
            }
        }
    }

    fn parse_plain(&mut self, events: &mut Vec<Event>) -> ParseResult {
        let first = self.pending[0];
        if first < 0x80 {
            if first == b'\n'
                && self
                    .last_cr_since
                    .is_some_and(|started| (started.elapsed().as_millis() as u64) < CRLF_DEDUPE_MS)
            {
                self.pending.remove(0);
                self.last_cr_since = None;
                return ParseResult::Complete;
            }
            self.pending.remove(0);
            self.last_cr_since = None;
            if first == b'\r' {
                events.push(key_event(KeyCode::Enter, KeyModifiers::NONE));
                self.last_cr_since = Some(Instant::now());
            } else {
                emit_control_or_char(first, KeyModifiers::NONE, events);
            }
            return ParseResult::Complete;
        }
        let width = utf8_width(first);
        if width == 0 {
            self.pending.remove(0);
            events.push(key_event(KeyCode::Char('\u{fffd}'), KeyModifiers::NONE));
            return ParseResult::Complete;
        }
        if self.pending.len() < width {
            return ParseResult::Incomplete;
        }
        let candidate = self.pending[..width].to_vec();
        match std::str::from_utf8(&candidate) {
            Ok(text) => {
                self.pending.drain(..width);
                for character in text.chars() {
                    events.push(key_event(KeyCode::Char(character), KeyModifiers::NONE));
                }
            }
            Err(_) => {
                self.pending.remove(0);
                events.push(key_event(KeyCode::Char('\u{fffd}'), KeyModifiers::NONE));
            }
        }
        ParseResult::Complete
    }

    fn parse_escape(&mut self, events: &mut Vec<Event>) -> ParseResult {
        if self.pending.len() == 1 {
            return ParseResult::Incomplete;
        }
        match self.pending[1] {
            b'[' => self.parse_csi(events),
            b'O' => self.parse_ss3(events),
            b']' | b'P' | b'^' | b'_' => self.parse_string_control(events),
            _ => {
                let second = self.pending[1];
                if second >= 0x80 {
                    let width = utf8_width(second);
                    if width == 0 || self.pending.len() < width + 1 {
                        return ParseResult::Incomplete;
                    }
                    let candidate = self.pending[1..=width].to_vec();
                    let Ok(text) = std::str::from_utf8(&candidate) else {
                        self.pending.drain(..2);
                        emit_alt_byte(Some(second), events);
                        return ParseResult::Complete;
                    };
                    self.pending.drain(..=width);
                    for character in text.chars() {
                        events.push(key_event(KeyCode::Char(character), KeyModifiers::ALT));
                    }
                    return ParseResult::Complete;
                }
                self.pending.drain(..2);
                emit_alt_byte(Some(second), events);
                ParseResult::Complete
            }
        }
    }

    fn parse_csi(&mut self, events: &mut Vec<Event>) -> ParseResult {
        let Some(final_index) = self
            .pending
            .iter()
            .enumerate()
            .skip(2)
            .find_map(|(index, byte)| (0x40..=0x7e).contains(byte).then_some(index))
        else {
            return ParseResult::Incomplete;
        };
        let sequence = self.pending[..=final_index].to_vec();
        self.pending.drain(..=final_index);
        if sequence == BRACKETED_PASTE_START {
            self.paste = Some(Vec::new());
            self.paste_marker.clear();
            let tail = std::mem::take(&mut self.pending);
            if !tail.is_empty() {
                self.feed_bytes(&tail, events);
            }
            return ParseResult::Complete;
        }
        let decoded = decode_csi(&sequence);
        match decoded {
            Some(event) => events.push(event),
            None => replay_literal(&sequence, events),
        }
        ParseResult::Complete
    }

    fn parse_ss3(&mut self, events: &mut Vec<Event>) -> ParseResult {
        if self.pending.len() < 3 {
            return ParseResult::Incomplete;
        }
        let sequence = self.pending.drain(..3).collect::<Vec<_>>();
        if let Some(event) = decode_ss3(&sequence) {
            events.push(event);
        } else {
            replay_literal(&sequence, events);
        }
        ParseResult::Complete
    }

    fn parse_string_control(&mut self, events: &mut Vec<Event>) -> ParseResult {
        let terminator = self.pending[1];
        let mut end = None;
        for index in 2..self.pending.len() {
            if self.pending[index] == 0x07
                || (self.pending[index] == ESC
                    && self.pending.get(index + 1).copied() == Some(b'\\'))
            {
                end = Some(if self.pending[index] == 0x07 {
                    index + 1
                } else {
                    index + 2
                });
                break;
            }
        }
        let Some(end) = end else {
            return ParseResult::Incomplete;
        };
        self.pending.drain(..end);
        let _ = terminator;
        // OSC/DCS/APC/PM are terminal controls, never text input.
        let _ = events;
        ParseResult::Complete
    }

    fn enforce_pending_bound(&mut self, events: &mut Vec<Event>) {
        if self.pending.len() > MAX_PENDING_BYTES {
            let bytes = std::mem::take(&mut self.pending);
            if !is_string_control_prefix(&bytes) {
                replay_literal(&bytes, events);
            }
            self.pending_since = None;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParseResult {
    Complete,
    Incomplete,
}

fn key_event(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new_with_kind(
        code,
        modifiers,
        KeyEventKind::Press,
    ))
}

fn key_event_with_kind(code: KeyCode, modifiers: KeyModifiers, kind: KeyEventKind) -> Event {
    Event::Key(KeyEvent::new_with_kind(code, modifiers, kind))
}

fn emit_control_or_char(byte: u8, modifiers: KeyModifiers, events: &mut Vec<Event>) {
    let code = match byte {
        b'\r' | b'\n' => KeyCode::Enter,
        b'\t' => KeyCode::Tab,
        0x08 | 0x7f => KeyCode::Backspace,
        0x01..=0x1a => {
            let character = (b'a' + byte - 1) as char;
            events.push(key_event(
                KeyCode::Char(character),
                modifiers | KeyModifiers::CONTROL,
            ));
            return;
        }
        0x00 => KeyCode::Null,
        value => KeyCode::Char(value as char),
    };
    events.push(key_event(code, modifiers));
}

fn emit_alt_byte(byte: Option<u8>, events: &mut Vec<Event>) {
    let Some(byte) = byte else {
        events.push(key_event(KeyCode::Esc, KeyModifiers::NONE));
        return;
    };
    if byte < 0x80 {
        match byte {
            b'\r' | b'\n' => events.push(key_event(KeyCode::Enter, KeyModifiers::ALT)),
            b'\t' => events.push(key_event(KeyCode::Tab, KeyModifiers::ALT)),
            0x08 | 0x7f => events.push(key_event(KeyCode::Backspace, KeyModifiers::ALT)),
            value => events.push(key_event(KeyCode::Char(value as char), KeyModifiers::ALT)),
        }
    } else {
        events.push(key_event(
            KeyCode::Char(char::from_u32(byte as u32).unwrap_or('\u{fffd}')),
            KeyModifiers::ALT,
        ));
    }
}

fn decode_ss3(sequence: &[u8]) -> Option<Event> {
    let code = match sequence.get(2).copied()? {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        b'P' => KeyCode::F(1),
        b'Q' => KeyCode::F(2),
        b'R' => KeyCode::F(3),
        b'S' => KeyCode::F(4),
        _ => return None,
    };
    Some(key_event(code, KeyModifiers::NONE))
}

fn decode_csi(sequence: &[u8]) -> Option<Event> {
    let final_byte = *sequence.last()?;
    let body = std::str::from_utf8(sequence.get(2..sequence.len().saturating_sub(1))?).ok()?;
    if body.is_empty() {
        match final_byte {
            b'I' => return Some(Event::FocusGained),
            b'O' => return Some(Event::FocusLost),
            _ => {}
        }
    }
    if body.starts_with('<') && matches!(final_byte, b'M' | b'm') {
        return decode_sgr_mouse(body, final_byte);
    }
    if final_byte == b'u' {
        return decode_csi_u(body);
    }
    if final_byte == b'~' {
        if let Some(event) = decode_modify_other_keys(body) {
            return Some(event);
        }
    }
    let (params, mut modifiers) = parse_csi_params(body);
    if final_byte == b'Z' && params.get(1).is_none() {
        modifiers.insert(KeyModifiers::SHIFT);
    }
    let code = match final_byte {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        b'Z' => KeyCode::BackTab,
        b'~' => match params.first().copied()? {
            1 | 7 => KeyCode::Home,
            3 => KeyCode::Delete,
            4 | 8 => KeyCode::End,
            5 => KeyCode::PageUp,
            6 => KeyCode::PageDown,
            11..=24 => function_key(params[0])?,
            _ => return None,
        },
        _ => return None,
    };
    Some(Event::Key(KeyEvent::new_with_kind(
        code,
        modifiers,
        KeyEventKind::Press,
    )))
}

fn decode_sgr_mouse(body: &str, final_byte: u8) -> Option<Event> {
    let mut fields = body.strip_prefix('<')?.split(';');
    let encoded = fields.next()?.parse::<u16>().ok()?;
    let column = fields.next()?.parse::<u16>().ok()?.saturating_sub(1);
    let row = fields.next()?.parse::<u16>().ok()?.saturating_sub(1);
    if fields.next().is_some() {
        return None;
    }

    let button = match encoded & 0b11 {
        0 => MouseButton::Left,
        1 => MouseButton::Middle,
        2 => MouseButton::Right,
        _ => MouseButton::Left,
    };
    let kind = if encoded & 0b0100_0000 != 0 {
        match encoded & 0b11 {
            0 => MouseEventKind::ScrollUp,
            1 => MouseEventKind::ScrollDown,
            2 => MouseEventKind::ScrollLeft,
            _ => MouseEventKind::ScrollRight,
        }
    } else if encoded & 0b0010_0000 != 0 {
        if encoded & 0b11 == 3 {
            MouseEventKind::Moved
        } else {
            MouseEventKind::Drag(button)
        }
    } else if final_byte == b'm' || encoded & 0b11 == 3 {
        MouseEventKind::Up(button)
    } else {
        MouseEventKind::Down(button)
    };
    let mut modifiers = KeyModifiers::NONE;
    if encoded & 0b0000_0100 != 0 {
        modifiers.insert(KeyModifiers::SHIFT);
    }
    if encoded & 0b0000_1000 != 0 {
        modifiers.insert(KeyModifiers::ALT);
    }
    if encoded & 0b0001_0000 != 0 {
        modifiers.insert(KeyModifiers::CONTROL);
    }
    Some(Event::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers,
    }))
}

fn decode_modify_other_keys(body: &str) -> Option<Event> {
    let fields = body.split(';').collect::<Vec<_>>();
    if fields.first()?.parse::<u32>().ok()? != 27 {
        return None;
    }
    let encoded_modifiers = fields.get(1)?.parse::<u16>().ok()?;
    let codepoint = fields.get(2)?.parse::<u32>().ok()?;
    let code = match codepoint {
        9 => KeyCode::Tab,
        13 => KeyCode::Enter,
        27 => KeyCode::Esc,
        127 => KeyCode::Backspace,
        value => KeyCode::Char(char::from_u32(value)?),
    };
    Some(key_event(code, kitty_modifiers(encoded_modifiers)))
}

fn decode_csi_u(body: &str) -> Option<Event> {
    let fields = body.split([';', ':']).collect::<Vec<_>>();
    let codepoint = fields.first()?.parse::<u32>().ok()?;
    let encoded_modifiers = fields
        .get(1)
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(1);
    let modifiers = kitty_modifiers(encoded_modifiers);
    let kind = match fields.get(2).and_then(|value| value.parse::<u8>().ok()) {
        Some(2) => KeyEventKind::Repeat,
        Some(3) => KeyEventKind::Release,
        _ => KeyEventKind::Press,
    };
    let code = match codepoint {
        9 => KeyCode::Tab,
        13 => KeyCode::Enter,
        27 => KeyCode::Esc,
        127 => KeyCode::Backspace,
        value => KeyCode::Char(char::from_u32(value)?),
    };
    Some(key_event_with_kind(code, modifiers, kind))
}

fn parse_csi_params(body: &str) -> (Vec<u16>, KeyModifiers) {
    let values = body
        .split(';')
        .filter_map(|value| value.parse::<u16>().ok())
        .collect::<Vec<_>>();
    let modifiers = values
        .get(1)
        .copied()
        .map(kitty_modifiers)
        .unwrap_or_else(|| {
            if body.ends_with('Z') {
                KeyModifiers::SHIFT
            } else {
                KeyModifiers::NONE
            }
        });
    (values, modifiers)
}

fn kitty_modifiers(encoded: u16) -> KeyModifiers {
    let bits = encoded.saturating_sub(1);
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
    if bits & 8 != 0 {
        modifiers.insert(KeyModifiers::SUPER);
    }
    modifiers
}

fn replay_literal(bytes: &[u8], events: &mut Vec<Event>) {
    let mut offset = 0;
    while offset < bytes.len() {
        if bytes[offset] == ESC {
            events.push(key_event(KeyCode::Esc, KeyModifiers::NONE));
            offset += 1;
            continue;
        }
        let width = utf8_width(bytes[offset]);
        if width > 0 && offset + width <= bytes.len() {
            if let Ok(text) = std::str::from_utf8(&bytes[offset..offset + width]) {
                for character in text.chars() {
                    events.push(key_event(KeyCode::Char(character), KeyModifiers::NONE));
                }
                offset += width;
                continue;
            }
        }
        emit_control_or_char(bytes[offset], KeyModifiers::NONE, events);
        offset += 1;
    }
}

fn function_key(value: u16) -> Option<KeyCode> {
    Some(match value {
        11 => KeyCode::F(1),
        12 => KeyCode::F(2),
        13 => KeyCode::F(3),
        14 => KeyCode::F(4),
        15 => KeyCode::F(5),
        17 => KeyCode::F(6),
        18 => KeyCode::F(7),
        19 => KeyCode::F(8),
        20 => KeyCode::F(9),
        21 => KeyCode::F(10),
        23 => KeyCode::F(11),
        24 => KeyCode::F(12),
        _ => return None,
    })
}

fn bytes_to_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn append_bounded(body: &mut Vec<u8>, bytes: &[u8]) {
    let remaining = MAX_PASTE_BYTES.saturating_sub(body.len());
    body.extend_from_slice(&bytes[..bytes.len().min(remaining)]);
}

fn is_string_control_prefix(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0] == ESC && matches!(bytes[1], b']' | b'P' | b'^' | b'_')
}

fn utf8_width(first: u8) -> usize {
    match first {
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::{RawVtParser, BRACKETED_PASTE_END, BRACKETED_PASTE_START, MAX_PASTE_BYTES};
    use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
    use std::time::{Duration, Instant};

    fn key(event: &Event) -> (KeyCode, KeyModifiers, KeyEventKind) {
        let Event::Key(value) = event else {
            panic!("expected key event");
        };
        (value.code, value.modifiers, value.kind)
    }

    #[test]
    fn parser_keeps_utf8_and_control_bytes_incremental() {
        let mut parser = RawVtParser::new();
        let bytes = "你好".as_bytes();
        assert!(parser.push(&bytes[..2]).is_empty());
        let mut events = parser.push(&bytes[2..]);
        events.extend(parser.push(b" \r\n\t\x08\x7f"));
        assert_eq!(key(&events.remove(0)).0, KeyCode::Char('你'));
        assert_eq!(key(&events.remove(0)).0, KeyCode::Char('好'));
        assert_eq!(key(&events.remove(0)).0, KeyCode::Char(' '));
        assert_eq!(key(&events.remove(0)).0, KeyCode::Enter);
        assert_eq!(key(&events.remove(0)).0, KeyCode::Tab);
        assert_eq!(key(&events.remove(0)).0, KeyCode::Backspace);
        assert_eq!(key(&events.remove(0)).0, KeyCode::Backspace);
    }

    #[test]
    fn parser_deduplicates_crlf_across_chunks_without_swallowing_text() {
        let mut parser = RawVtParser::new();
        assert_eq!(key(&parser.push(b"\r")[0]).0, KeyCode::Enter);
        assert!(parser.push(b"\n").is_empty());

        let events = parser.push(b"x\n");
        assert_eq!(key(&events[0]).0, KeyCode::Char('x'));
        assert_eq!(key(&events[1]).0, KeyCode::Enter);

        let mut parser = RawVtParser::new();
        let events = parser.push(b"\rtext");
        assert_eq!(key(&events[0]).0, KeyCode::Enter);
        assert_eq!(events.len(), 5);
        assert_eq!(key(&events[4]).0, KeyCode::Char('t'));
    }

    #[test]
    fn parser_decodes_navigation_modifiers_and_csi_u_lifecycle() {
        let mut parser = RawVtParser::new();
        let mut events = parser.push(b"\x1b[");
        assert!(events.is_empty());
        events.extend(parser.push(b"1;5A\x1b[Z\x1b[3~"));
        assert_eq!(
            key(&events.remove(0)),
            (KeyCode::Up, KeyModifiers::CONTROL, KeyEventKind::Press)
        );
        assert_eq!(key(&events.remove(0)).0, KeyCode::BackTab);
        assert_eq!(key(&events.remove(0)).0, KeyCode::Delete);

        let events = parser.push(b"\x1b[13;2u\x1b[9;1:2u\x1b[120;1:3u");
        assert_eq!(
            key(&events[0]),
            (KeyCode::Enter, KeyModifiers::SHIFT, KeyEventKind::Press)
        );
        assert_eq!(
            key(&events[1]),
            (KeyCode::Tab, KeyModifiers::NONE, KeyEventKind::Repeat)
        );
        assert_eq!(
            key(&events[2]),
            (
                KeyCode::Char('x'),
                KeyModifiers::NONE,
                KeyEventKind::Release
            )
        );
    }

    #[test]
    fn parser_decodes_alt_and_split_ss3() {
        let mut parser = RawVtParser::new();
        let mut events = parser.push("é".as_bytes());
        events.extend(parser.push(b"\x1b\r\x1b"));
        events.extend(parser.push(b"OP"));
        assert_eq!(
            key(&events[0]),
            (KeyCode::Char('é'), KeyModifiers::NONE, KeyEventKind::Press)
        );
        assert_eq!(
            key(&events[1]),
            (KeyCode::Enter, KeyModifiers::ALT, KeyEventKind::Press)
        );
        assert_eq!(key(&events[2]).0, KeyCode::F(1));
    }

    #[test]
    fn parser_preserves_bracketed_body_and_marker_boundaries() {
        let mut parser = RawVtParser::new();
        let mut events = parser.push(&BRACKETED_PASTE_START[..3]);
        assert!(events.is_empty());
        events = parser.push(&BRACKETED_PASTE_START[3..]);
        assert!(events.is_empty());
        let body = "一\r\n二\t\x1b[31m三".as_bytes();
        assert!(parser.push(&body[..5]).is_empty());
        let mut tail = body[5..].to_vec();
        tail.extend_from_slice(&BRACKETED_PASTE_END[..2]);
        assert!(parser.push(&tail).is_empty());
        let events = parser.push(&BRACKETED_PASTE_END[2..]);
        let Event::Paste(text) = &events[0] else {
            panic!("expected bracketed paste event");
        };
        assert_eq!(text, "一\r\n二\t\x1b[31m三");
        assert!(!text.contains("200~") && !text.contains("201~"));
    }

    #[test]
    fn paste_and_immediate_enter_in_one_read_stay_ordered_and_unique() {
        let mut parser = RawVtParser::new();
        let events = parser.push(b"\x1b[200~payload\x1b[201~\r\n");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], Event::Paste("payload".into()));
        assert_eq!(key(&events[1]).0, KeyCode::Enter);
    }

    #[test]
    fn terminal_focus_and_mouse_reports_never_become_typed_text() {
        let mut parser = RawVtParser::new();
        let events = parser.push(b"\x1b[I\x1b[O\x1b[<64;12;4M");
        assert_eq!(events[0], Event::FocusGained);
        assert_eq!(events[1], Event::FocusLost);
        let Event::Mouse(mouse) = events[2] else {
            panic!("expected decoded mouse report");
        };
        assert_eq!(mouse.kind, MouseEventKind::ScrollUp);
        assert_eq!((mouse.column, mouse.row), (11, 3));
    }

    #[test]
    fn bracketed_paste_survives_long_inter_chunk_pauses() {
        let mut parser = RawVtParser::new();
        assert!(parser.push(BRACKETED_PASTE_START).is_empty());
        assert!(parser
            .flush_expired(
                Instant::now() + Duration::from_secs(1),
                Duration::from_millis(80),
            )
            .is_empty());
        assert!(parser.push(b"first").is_empty());
        assert!(parser
            .flush_expired(
                Instant::now() + Duration::from_secs(2),
                Duration::from_millis(80),
            )
            .is_empty());
        assert!(parser.push(b" second").is_empty());
        let events = parser.push(BRACKETED_PASTE_END);
        assert_eq!(events, vec![Event::Paste("first second".into())]);
    }

    #[test]
    fn oversized_paste_keeps_end_marker_and_tail() {
        let mut parser = RawVtParser::new();
        assert!(parser.push(BRACKETED_PASTE_START).is_empty());
        let mut payload = vec![b'x'; MAX_PASTE_BYTES + 16];
        payload.extend_from_slice(BRACKETED_PASTE_END);
        payload.extend_from_slice(b"tail");
        let events = parser.push(&payload);
        let Event::Paste(text) = &events[0] else {
            panic!("expected bounded paste event");
        };
        assert_eq!(text.len(), MAX_PASTE_BYTES);
        assert_eq!(events[1..].len(), 4);
        assert_eq!(key(&events[1]).0, KeyCode::Char('t'));
    }

    #[test]
    fn parser_decodes_modify_other_keys_for_control_keys() {
        let mut parser = RawVtParser::new();
        let events = parser.push(b"\x1b[27;5;13~\x1b[27;2;9~\x1b[27;3;127~");
        assert_eq!(
            key(&events[0]),
            (KeyCode::Enter, KeyModifiers::CONTROL, KeyEventKind::Press)
        );
        assert_eq!(
            key(&events[1]),
            (KeyCode::Tab, KeyModifiers::SHIFT, KeyEventKind::Press)
        );
        assert_eq!(
            key(&events[2]),
            (KeyCode::Backspace, KeyModifiers::ALT, KeyEventKind::Press)
        );
    }

    #[test]
    fn unknown_sequences_replay_and_timeout_does_not_swallow_text() {
        let mut parser = RawVtParser::new();
        let events = parser.push(b"\x1b[999~abc");
        assert_eq!(key(&events[0]).0, KeyCode::Esc);
        assert_eq!(key(&events[1]).0, KeyCode::Char('['));
        assert_eq!(key(&events[2]).0, KeyCode::Char('9'));
        assert_eq!(key(&events[5]).0, KeyCode::Char('~'));
        assert_eq!(key(&events[6]).0, KeyCode::Char('a'));

        let mut parser = RawVtParser::new();
        assert!(parser.push(b"\x1b[").is_empty());
        let flushed = parser.flush_expired(
            Instant::now() + Duration::from_millis(100),
            Duration::from_millis(50),
        );
        assert_eq!(key(&flushed[0]).0, KeyCode::Esc);
        assert_eq!(key(&flushed[1]).0, KeyCode::Char('['));
    }

    #[test]
    fn incomplete_string_control_is_not_replayed_as_text() {
        let mut parser = RawVtParser::new();
        assert!(parser.push(b"\x1b]0;secret").is_empty());
        assert!(parser
            .flush_expired(
                Instant::now() + Duration::from_millis(100),
                Duration::from_millis(50),
            )
            .is_empty());
    }

    #[test]
    fn oversized_incomplete_string_control_is_discarded_without_leaking_payload() {
        let mut parser = RawVtParser::new();
        let mut control = b"\x1b]0;secret".to_vec();
        control.extend(std::iter::repeat_n(b'x', 256));
        assert!(parser.push(&control).is_empty());
        assert!(parser.flush().is_empty());
    }

    #[test]
    fn repeated_payload_is_not_duplicated_inside_one_parse() {
        let mut parser = RawVtParser::new();
        let events = parser.push(b"same");
        assert_eq!(events.len(), 4);
        assert_eq!(parser.push(b"same").len(), 4);
    }
}
