use std::collections::{BTreeMap, BTreeSet};
use std::sync::{OnceLock, RwLock};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ActionId {
    CommandPalette,
    ContextHelp,
    LiveSearch,
    Queue,
    LiveInspector,
    AnswerHistory,
    ReasoningHistory,
    ToolHistory,
    Activity,
    InputEditor,
    LiveHold,
}

impl ActionId {
    pub(crate) const ALL: [Self; 11] = [
        Self::CommandPalette,
        Self::ContextHelp,
        Self::LiveSearch,
        Self::Queue,
        Self::LiveInspector,
        Self::AnswerHistory,
        Self::ReasoningHistory,
        Self::ToolHistory,
        Self::Activity,
        Self::InputEditor,
        Self::LiveHold,
    ];

    pub(crate) const fn id(self) -> &'static str {
        match self {
            Self::CommandPalette => "command_palette",
            Self::ContextHelp => "context_help",
            Self::LiveSearch => "live_search",
            Self::Queue => "queue",
            Self::LiveInspector => "live_inspector",
            Self::AnswerHistory => "answer_history",
            Self::ReasoningHistory => "reasoning_history",
            Self::ToolHistory => "tool_history",
            Self::Activity => "activity",
            Self::InputEditor => "input_editor",
            Self::LiveHold => "live_hold",
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::CommandPalette => "Command palette",
            Self::ContextHelp => "Context help",
            Self::LiveSearch => "Search live output",
            Self::Queue => "Pending queue",
            Self::LiveInspector => "Live inspector",
            Self::AnswerHistory => "Answer history",
            Self::ReasoningHistory => "Reasoning history",
            Self::ToolHistory => "Tool history",
            Self::Activity => "Agent activity",
            Self::InputEditor => "Full-screen editor",
            Self::LiveHold => "Hold/follow live output",
        }
    }

    pub(crate) const fn command(self) -> Option<&'static str> {
        match self {
            Self::ContextHelp => Some("/help"),
            Self::CommandPalette | Self::InputEditor | Self::LiveHold => None,
            Self::LiveSearch => Some("/find"),
            Self::Queue => Some("/queue"),
            Self::LiveInspector => Some("/inspect"),
            Self::AnswerHistory => Some("/answers"),
            Self::ReasoningHistory => Some("/reasoning"),
            Self::ToolHistory => Some("/history"),
            Self::Activity => Some("/activity"),
        }
    }

    fn default_chords(self) -> &'static [&'static str] {
        match self {
            Self::CommandPalette => &["ctrl+p"],
            Self::ContextHelp => &["f1"],
            Self::LiveSearch => &["ctrl+f"],
            Self::Queue => &["ctrl+q"],
            Self::LiveInspector => &["alt+i"],
            Self::AnswerHistory => &["alt+a"],
            Self::ReasoningHistory => &["alt+r"],
            Self::ToolHistory => &["alt+t"],
            Self::Activity => &["alt+g"],
            Self::InputEditor => &["ctrl+e"],
            Self::LiveHold => &["ctrl+space"],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Chord {
    code: KeyCode,
    modifiers: KeyModifiers,
}

impl Chord {
    fn matches(&self, key: &KeyEvent) -> bool {
        let code = match super::canonical_key_code(key) {
            KeyCode::Char(value) if value.is_ascii_uppercase() => {
                KeyCode::Char(value.to_ascii_lowercase())
            }
            other => other,
        };
        key.kind == KeyEventKind::Press && code == self.code && key.modifiers == self.modifiers
    }
}

#[derive(Debug, Clone)]
struct ActiveKeymap {
    bindings: BTreeMap<ActionId, Vec<Chord>>,
    labels: BTreeMap<ActionId, Vec<String>>,
}

static ACTIVE_KEYMAP: OnceLock<RwLock<ActiveKeymap>> = OnceLock::new();
static KEYMAP_WARNING: OnceLock<RwLock<Option<String>>> = OnceLock::new();

fn parse_chord(raw: &str) -> Result<Chord, String> {
    let normalized = raw.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Err("empty key chord".into());
    }
    let parts: Vec<&str> = normalized.split('+').collect();
    let key = parts.last().copied().unwrap_or_default();
    let mut modifiers = KeyModifiers::NONE;
    for modifier in &parts[..parts.len().saturating_sub(1)] {
        let flag = match *modifier {
            "ctrl" | "control" => KeyModifiers::CONTROL,
            "alt" => KeyModifiers::ALT,
            "shift" => KeyModifiers::SHIFT,
            _ => return Err(format!("unknown modifier `{modifier}` in `{raw}`")),
        };
        if modifiers.contains(flag) {
            return Err(format!("duplicate modifier in `{raw}`"));
        }
        modifiers.insert(flag);
    }
    let code = match key {
        "space" => KeyCode::Char(' '),
        "f1" => KeyCode::F(1),
        value if value.chars().count() == 1 => KeyCode::Char(value.chars().next().unwrap()),
        _ => return Err(format!("unsupported key `{key}` in `{raw}`")),
    };
    if modifiers.is_empty() && !matches!(code, KeyCode::F(_)) {
        return Err(format!("global chord `{raw}` requires a modifier"));
    }
    if matches!(
        (modifiers, code),
        (KeyModifiers::CONTROL, KeyCode::Char('c' | 'j' | 'v'))
    ) {
        return Err(format!("reserved editor/safety chord `{raw}`"));
    }
    Ok(Chord { code, modifiers })
}

fn build_keymap(overrides: &BTreeMap<String, Vec<String>>) -> Result<ActiveKeymap, String> {
    let known: BTreeSet<&str> = ActionId::ALL.iter().map(|action| action.id()).collect();
    if let Some(unknown) = overrides.keys().find(|key| !known.contains(key.as_str())) {
        return Err(format!("unknown action `{unknown}`"));
    }
    let mut bindings = BTreeMap::new();
    let mut labels = BTreeMap::new();
    let mut occupied: Vec<(Chord, ActionId)> = Vec::new();
    for action in ActionId::ALL {
        let raw: Vec<String> = overrides.get(action.id()).cloned().unwrap_or_else(|| {
            action
                .default_chords()
                .iter()
                .map(|s| (*s).to_string())
                .collect()
        });
        let mut parsed = Vec::with_capacity(raw.len());
        for label in &raw {
            let chord = parse_chord(label)?;
            if let Some((_, other)) = occupied.iter().find(|(used, _)| used == &chord) {
                return Err(format!(
                    "key `{label}` conflicts between `{}` and `{}`",
                    other.id(),
                    action.id()
                ));
            }
            occupied.push((chord.clone(), action));
            parsed.push(chord);
        }
        bindings.insert(action, parsed);
        labels.insert(action, raw);
    }
    Ok(ActiveKeymap { bindings, labels })
}

fn defaults() -> ActiveKeymap {
    build_keymap(&BTreeMap::new()).expect("built-in keymap must be valid")
}

pub(crate) fn install_keymap(overrides: &BTreeMap<String, Vec<String>>) -> Result<(), String> {
    let lock = ACTIVE_KEYMAP.get_or_init(|| RwLock::new(defaults()));
    let warning = KEYMAP_WARNING.get_or_init(|| RwLock::new(None));
    let candidate = match build_keymap(overrides) {
        Ok(candidate) => candidate,
        Err(error) => {
            *lock
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = defaults();
            *warning
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(error.clone());
            return Err(error);
        }
    };
    *lock
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = candidate;
    *warning
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    Ok(())
}

pub(crate) fn keymap_warning() -> Option<String> {
    KEYMAP_WARNING.get().and_then(|warning| {
        warning
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    })
}

fn with_keymap<T>(f: impl FnOnce(&ActiveKeymap) -> T) -> T {
    let lock = ACTIVE_KEYMAP.get_or_init(|| RwLock::new(defaults()));
    let guard = lock.read().unwrap_or_else(|poisoned| poisoned.into_inner());
    f(&guard)
}

pub(crate) fn keymap_action(key: &KeyEvent) -> Option<ActionId> {
    with_keymap(|keymap| {
        ActionId::ALL.into_iter().find(|action| {
            if *action == ActionId::LiveHold && super::is_momentary_hold_key(key) {
                return true;
            }
            keymap
                .bindings
                .get(action)
                .is_some_and(|chords| chords.iter().any(|chord| chord.matches(key)))
        })
    })
}

pub(crate) fn keymap_release_action(key: &KeyEvent) -> Option<ActionId> {
    if key.kind != KeyEventKind::Release {
        return None;
    }
    let pressed = KeyEvent::new_with_kind(key.code, key.modifiers, KeyEventKind::Press);
    keymap_action(&pressed)
}

pub(crate) fn shortcut_label(action: ActionId) -> String {
    with_keymap(|keymap| {
        keymap
            .labels
            .get(&action)
            .map(|labels| labels.join(" / "))
            .unwrap_or_default()
    })
}

pub(crate) fn keybinding_rows() -> Vec<(String, String)> {
    ActionId::ALL
        .into_iter()
        .map(|action| {
            let keys = shortcut_label(action);
            (
                action.label().to_string(),
                if keys.is_empty() {
                    "disabled".into()
                } else {
                    keys
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_unique_and_parseable() {
        let map = build_keymap(&BTreeMap::new()).unwrap();
        assert_eq!(map.bindings.len(), ActionId::ALL.len());
    }

    #[test]
    fn overrides_are_atomic_and_validate_conflicts() {
        let mut overrides = BTreeMap::new();
        overrides.insert("command_palette".into(), vec!["alt+x".into()]);
        let map = build_keymap(&overrides).unwrap();
        assert_eq!(map.labels[&ActionId::CommandPalette], ["alt+x"]);
        overrides.insert("context_help".into(), vec!["alt+x".into()]);
        assert!(build_keymap(&overrides).unwrap_err().contains("conflicts"));
        overrides.clear();
        overrides.insert("missing".into(), vec!["alt+x".into()]);
        assert!(build_keymap(&overrides)
            .unwrap_err()
            .contains("unknown action"));
        overrides.clear();
        overrides.insert("command_palette".into(), vec!["ctrl+c".into()]);
        assert!(build_keymap(&overrides).unwrap_err().contains("reserved"));
    }

    #[test]
    fn empty_override_disables_an_action() {
        let mut overrides = BTreeMap::new();
        overrides.insert("activity".into(), Vec::new());
        let map = build_keymap(&overrides).unwrap();
        assert!(map.bindings[&ActionId::Activity].is_empty());
    }
}
