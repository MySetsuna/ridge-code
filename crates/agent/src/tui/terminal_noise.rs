//! Filters terminal reports that Crossterm occasionally exposes as key fragments.
//!
//! The normal Crossterm decoder owns complete control sequences. Some PTY hops,
//! however, split an SGR mouse or focus report into ordinary key events. Keep the
//! recognizer narrow so typed bracket expressions remain unchanged.

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

const MAX_HELD_EVENTS: usize = 64;

pub(crate) struct CsiNoiseFilter {
    state: CsiState,
    held: Vec<Event>,
}

impl CsiNoiseFilter {
    pub(crate) fn new() -> Self {
        Self {
            state: CsiState::Idle,
            held: Vec::new(),
        }
    }

    /// Remove only complete SGR mouse reports and focus reports with an
    /// immediately preceding Escape. Deeper SGR prefixes survive across calls;
    /// a lone `[` never does, lest ordinary typing wait for another key.
    pub(crate) fn filter(&mut self, events: Vec<Event>) -> Vec<Event> {
        let mut output = Vec::with_capacity(self.held.len() + events.len());
        let mut escape_before_run = false;

        for event in events {
            if is_bare_escape(&event) {
                output.append(&mut self.held);
                self.state = CsiState::Idle;
                output.push(event);
                escape_before_run = true;
                continue;
            }

            let Some(character) = filterable_character(&event) else {
                output.append(&mut self.held);
                self.state = CsiState::Idle;
                escape_before_run = false;
                output.push(event);
                continue;
            };

            match self.state.advance(character) {
                Advance::Continue(next) => {
                    self.state = next;
                    self.held.push(event);
                    if self.held.len() > MAX_HELD_EVENTS {
                        output.append(&mut self.held);
                        self.state = CsiState::Idle;
                        escape_before_run = false;
                    }
                }
                Advance::MouseComplete => {
                    self.held.clear();
                    if escape_before_run {
                        let _ = output.pop();
                    }
                    self.state = CsiState::Idle;
                    escape_before_run = false;
                }
                Advance::FocusComplete => {
                    if escape_before_run {
                        self.held.clear();
                        let _ = output.pop();
                        output.push(if character == 'I' {
                            Event::FocusGained
                        } else {
                            Event::FocusLost
                        });
                    } else {
                        output.append(&mut self.held);
                        output.push(event);
                    }
                    self.state = CsiState::Idle;
                    escape_before_run = false;
                }
                Advance::Reject => {
                    output.append(&mut self.held);
                    self.state = CsiState::Idle;
                    escape_before_run = false;
                    if matches!(CsiState::Idle.advance(character), Advance::Continue(_)) {
                        self.state = CsiState::Bracket;
                        self.held.push(event);
                    } else {
                        output.push(event);
                    }
                }
            }
        }

        if self.state == CsiState::Bracket {
            output.append(&mut self.held);
            self.state = CsiState::Idle;
        }
        output
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CsiState {
    Idle,
    Bracket,
    LessThan,
    Button,
    ColumnStart,
    Column,
    RowStart,
    Row,
}

enum Advance {
    Continue(CsiState),
    MouseComplete,
    FocusComplete,
    Reject,
}

impl CsiState {
    fn advance(self, character: char) -> Advance {
        use CsiState::*;
        match (self, character) {
            (Idle, '[') => Advance::Continue(Bracket),
            (Bracket, '<') => Advance::Continue(LessThan),
            (Bracket, 'I' | 'O') => Advance::FocusComplete,
            (LessThan | Button, value) if value.is_ascii_digit() => Advance::Continue(Button),
            (Button, ';') => Advance::Continue(ColumnStart),
            (ColumnStart | Column, value) if value.is_ascii_digit() => Advance::Continue(Column),
            (Column, ';') => Advance::Continue(RowStart),
            (RowStart | Row, value) if value.is_ascii_digit() => Advance::Continue(Row),
            (Row, 'M' | 'm') => Advance::MouseComplete,
            _ => Advance::Reject,
        }
    }
}

fn is_bare_escape(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(key)
            if key.kind == KeyEventKind::Press
                && key.code == KeyCode::Esc
                && key.modifiers.is_empty()
    )
}

fn filterable_character(event: &Event) -> Option<char> {
    let Event::Key(key) = event else {
        return None;
    };
    if key.kind != KeyEventKind::Press
        || !(key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
    {
        return None;
    }
    match key.code {
        KeyCode::Char(character) => Some(character),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::CsiNoiseFilter;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    fn key(character: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))
    }

    fn escape() -> Event {
        Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
    }

    #[test]
    fn fragmented_mouse_report_never_reaches_composer() {
        let mut filter = CsiNoiseFilter::new();
        assert!(filter
            .filter("[<64;91;".chars().map(key).collect())
            .is_empty());
        assert!(filter.filter("51M".chars().map(key).collect()).is_empty());
    }

    #[test]
    fn typed_brackets_and_rejected_prefixes_are_preserved() {
        let mut filter = CsiNoiseFilter::new();
        assert_eq!(filter.filter(vec![key('[')]), vec![key('[')]);
        assert_eq!(
            filter.filter("[item]".chars().map(key).collect()),
            "[item]".chars().map(key).collect::<Vec<_>>()
        );
    }

    #[test]
    fn focus_report_is_reassembled_but_typed_pair_is_preserved() {
        let mut filter = CsiNoiseFilter::new();
        assert_eq!(
            filter.filter(vec![escape(), key('['), key('I')]),
            vec![Event::FocusGained]
        );
        assert_eq!(
            filter.filter(vec![key('['), key('O')]),
            vec![key('['), key('O')]
        );
    }

    #[test]
    fn enter_tab_and_spaces_pass_unchanged() {
        let events = vec![
            key(' '),
            Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        ];
        assert_eq!(CsiNoiseFilter::new().filter(events.clone()), events);
    }
}
