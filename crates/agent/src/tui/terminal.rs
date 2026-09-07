//! Terminal capability policy.
//!
//! Keyboard enhancement is opt-in by explicit user evidence.  A terminal
//! which cannot preserve Kitty keyboard protocol bytes must stay on the legacy
//! path; otherwise a modified Enter/Tab can arrive as an ordinary character
//! or a literal escape tail.  Terminal names and version variables identify a
//! host, but do not prove that every PTY hop has enabled/preserved KKP.  The
//! policy is pure over an environment map so the matrix remains testable
//! without a real terminal.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TerminalKeyboardPolicy {
    pub(crate) keyboard_enhancement: bool,
    pub(crate) reason: &'static str,
}

impl TerminalKeyboardPolicy {
    pub(crate) fn newline_label(self) -> &'static str {
        if self.keyboard_enhancement {
            "Shift/Alt+Enter newline"
        } else {
            "Alt+Enter/Ctrl+J newline"
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalInputBackendKind {
    RawVt,
    Crossterm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TerminalInputBackend {
    pub(crate) kind: TerminalInputBackendKind,
    pub(crate) reason: &'static str,
}

impl TerminalInputBackend {
    pub(crate) const fn raw_vt(reason: &'static str) -> Self {
        Self {
            kind: TerminalInputBackendKind::RawVt,
            reason,
        }
    }

    pub(crate) const fn crossterm(reason: &'static str) -> Self {
        Self {
            kind: TerminalInputBackendKind::Crossterm,
            reason,
        }
    }

    pub(crate) const fn is_raw_vt(self) -> bool {
        matches!(self.kind, TerminalInputBackendKind::RawVt)
    }

    pub(crate) const fn label(self) -> &'static str {
        match self.kind {
            TerminalInputBackendKind::RawVt => "raw-vt",
            TerminalInputBackendKind::Crossterm => "crossterm",
        }
    }
}

/// Decide the requested Windows input transport without touching process
/// environment state during tests.  `auto`/unset selects raw VT on Windows;
/// activation still has to succeed in `native_input::enter` before it is
/// reported as the active backend.
pub(crate) fn terminal_input_backend_from_env(
    env: &HashMap<String, String>,
    host_is_windows: bool,
) -> TerminalInputBackend {
    if !host_is_windows {
        return TerminalInputBackend::crossterm("non_windows");
    }
    match env_value(env, "RIDGECODE_TUI_VT_INPUT") {
        Some("0") => TerminalInputBackend::crossterm("explicit_disable"),
        Some("1") => TerminalInputBackend::raw_vt("explicit_override"),
        Some("auto") | None => TerminalInputBackend::raw_vt("windows_auto"),
        Some(_) => TerminalInputBackend::crossterm("invalid_override"),
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn terminal_input_backend() -> TerminalInputBackend {
    let env = std::env::vars().collect::<HashMap<_, _>>();
    terminal_input_backend_from_env(&env, cfg!(windows))
}

/// Non-secret terminal facts and the actionable fallback for `/doctor`.
///
/// Keep this report environment-only: never print arbitrary variables because
/// shells and IDEs may expose credentials through their environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalDiagnostics {
    pub(crate) host: &'static str,
    pub(crate) term: Option<String>,
    pub(crate) term_program: Option<String>,
    pub(crate) colorterm: Option<String>,
    pub(crate) vte_version: Option<u32>,
    pub(crate) bridge: Option<&'static str>,
    pub(crate) multiplexer: Option<&'static str>,
    pub(crate) policy: TerminalKeyboardPolicy,
    pub(crate) input_backend: TerminalInputBackend,
    pub(crate) virtual_terminal_input: bool,
    pub(crate) hints: Vec<&'static str>,
}

impl TerminalDiagnostics {
    pub(crate) fn report(&self) -> String {
        let mut lines = vec![
            "terminal doctor".to_string(),
            format!("host: {}", self.host),
            format!("term: {}", self.term.as_deref().unwrap_or("(unset)")),
            format!(
                "program: {}",
                self.term_program.as_deref().unwrap_or("(unset)")
            ),
            format!("color: {}", self.colorterm.as_deref().unwrap_or("(unset)")),
            format!(
                "vte: {}",
                self.vte_version
                    .map(|version| version.to_string())
                    .as_deref()
                    .unwrap_or("(unset)")
            ),
            format!("bridge: {}", self.bridge.unwrap_or("direct")),
            format!(
                "multiplexer: {}",
                self.multiplexer.unwrap_or("none detected")
            ),
            format!(
                "kitty keyboard: {} ({})",
                if self.policy.keyboard_enhancement {
                    "enabled"
                } else {
                    "legacy"
                },
                self.policy.reason
            ),
            format!(
                "VT input: {}",
                if self.virtual_terminal_input {
                    "enabled"
                } else {
                    "disabled"
                }
            ),
            format!(
                "input backend: {} ({})",
                self.input_backend.label(),
                self.input_backend.reason
            ),
            format!("newline: {}", self.policy.newline_label()),
        ];
        if self.hints.is_empty() {
            lines.push("hints: none".to_string());
        } else {
            lines.push("hints:".to_string());
            lines.extend(self.hints.iter().map(|hint| format!("- {hint}")));
        }
        lines.join("\n")
    }
}

/// Resolve the keyboard protocol policy for the current process.
pub(crate) fn terminal_keyboard_policy() -> TerminalKeyboardPolicy {
    let env = std::env::vars().collect::<HashMap<_, _>>();
    terminal_keyboard_policy_from_env(&env, cfg!(windows))
}

/// Render a bounded, non-secret terminal report for the CLI and TUI.
pub(crate) fn terminal_doctor_report() -> String {
    let env = std::env::vars().collect::<HashMap<_, _>>();
    let mut diagnostics = terminal_diagnostics_from_env(&env, cfg!(windows));
    if let Some(active) = active_input_backend() {
        diagnostics.virtual_terminal_input = active.is_raw_vt();
        diagnostics.input_backend = active;
    }
    diagnostics.report()
}

static ACTIVE_INPUT_BACKEND: OnceLock<Mutex<Option<TerminalInputBackend>>> = OnceLock::new();

fn active_input_backend() -> Option<TerminalInputBackend> {
    ACTIVE_INPUT_BACKEND
        .get_or_init(|| Mutex::new(None))
        .lock()
        .ok()
        .and_then(|backend| *backend)
}

pub(crate) fn set_active_input_backend(backend: TerminalInputBackend) {
    if let Ok(mut active) = ACTIVE_INPUT_BACKEND.get_or_init(|| Mutex::new(None)).lock() {
        *active = Some(backend);
    }
}

pub(crate) fn clear_active_input_backend() {
    if let Ok(mut active) = ACTIVE_INPUT_BACKEND.get_or_init(|| Mutex::new(None)).lock() {
        *active = None;
    }
}

pub(crate) fn terminal_diagnostics_from_env(
    env: &HashMap<String, String>,
    host_is_windows: bool,
) -> TerminalDiagnostics {
    let policy = terminal_keyboard_policy_from_env(env, host_is_windows);
    let input_backend = terminal_input_backend_from_env(env, host_is_windows);
    let term = visible_env_value(env, "TERM");
    let term_program =
        visible_env_value(env, "TERM_PROGRAM").or_else(|| visible_env_value(env, "LC_TERMINAL"));
    let colorterm = visible_env_value(env, "COLORTERM");
    let vte_version = env_value(env, "VTE_VERSION").and_then(|value| value.parse().ok());
    let bridge = if env_value(env, "SSH_TTY").is_some() {
        Some("ssh")
    } else if is_vscode_family(env) {
        Some("ide-pty")
    } else if env_value(env, "JUPYTER_RUNTIME_DIR").is_some() {
        Some("jupyter")
    } else {
        None
    };
    let multiplexer = if env_value(env, "TMUX").is_some() {
        Some("tmux")
    } else if env_value(env, "STY").is_some() {
        Some("screen")
    } else {
        None
    };
    let hints = diagnostic_hints(policy.reason);
    TerminalDiagnostics {
        host: if host_is_windows { "windows" } else { "unix" },
        term,
        term_program,
        colorterm,
        vte_version,
        bridge,
        multiplexer,
        policy,
        virtual_terminal_input: input_backend.is_raw_vt(),
        input_backend,
        hints,
    }
}

/// Pure capability matrix used by the TUI and its tests.
pub(crate) fn terminal_keyboard_policy_from_env(
    env: &HashMap<String, String>,
    host_is_windows: bool,
) -> TerminalKeyboardPolicy {
    if env_value(env, "RIDGECODE_TUI_KITTY") == Some("1") {
        return TerminalKeyboardPolicy {
            keyboard_enhancement: true,
            reason: "explicit_override",
        };
    }
    if env_value(env, "RIDGECODE_TUI_KITTY") == Some("0") {
        return TerminalKeyboardPolicy {
            keyboard_enhancement: false,
            reason: "explicit_disable",
        };
    }

    // Windows input uses the raw-VT backend policy below, but Kitty keyboard
    // negotiation remains disabled: the byte parser already owns the common
    // CSI/CSI-u forms and Windows hosts do not need a push/pop handshake.
    if host_is_windows {
        return TerminalKeyboardPolicy {
            keyboard_enhancement: false,
            reason: "windows_native_input",
        };
    }

    if is_vscode_family(env) {
        return TerminalKeyboardPolicy {
            keyboard_enhancement: false,
            reason: "vscode_family",
        };
    }
    if is_apple_terminal(env) {
        return TerminalKeyboardPolicy {
            keyboard_enhancement: false,
            reason: "apple_terminal",
        };
    }
    if env_value(env, "TERMINAL_EMULATOR")
        .is_some_and(|value| value.to_ascii_lowercase().contains("jetbrains"))
    {
        return TerminalKeyboardPolicy {
            keyboard_enhancement: false,
            reason: "jetbrains",
        };
    }
    if env_value(env, "VTE_VERSION").is_some() {
        let version = env_value(env, "VTE_VERSION")
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or_default();
        return TerminalKeyboardPolicy {
            keyboard_enhancement: false,
            reason: if version >= 8_200 {
                "vte_known_legacy"
            } else {
                "legacy_vte"
            },
        };
    }

    // Multiplexers can strip or rewrite the negotiation unless their own
    // extended-keys mode is known.  An explicit override remains available
    // for a user who has configured the outer layer.
    if env_value(env, "TMUX").is_some() || env_value(env, "STY").is_some() {
        return TerminalKeyboardPolicy {
            keyboard_enhancement: false,
            reason: "multiplexer_unknown",
        };
    }

    let terminal = env_value(env, "TERM_PROGRAM")
        .map(normalize)
        .or_else(|| env_value(env, "LC_TERMINAL").map(normalize));
    let known_terminal = matches!(
        terminal.as_deref(),
        Some("ghostty" | "kitty" | "wezterm" | "alacritty" | "rio" | "iterm" | "iterm2" | "warp")
    );
    TerminalKeyboardPolicy {
        keyboard_enhancement: false,
        reason: if known_terminal {
            "known_terminal_legacy"
        } else {
            "unknown_terminal"
        },
    }
}

fn env_value<'a>(env: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    env.get(key)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
}

fn visible_env_value(env: &HashMap<String, String>, key: &str) -> Option<String> {
    let value: String = env_value(env, key)?
        .chars()
        .filter(|character| !character.is_control())
        .take(80)
        .collect();
    (!value.is_empty()).then_some(value)
}

fn diagnostic_hints(reason: &'static str) -> Vec<&'static str> {
    match reason {
        "windows_native_input" => vec![
            "Windows input uses the raw-vt backend when console/ConPTY activation succeeds; RIDGECODE_TUI_VT_INPUT=0 forces Crossterm",
            "keep RIDGECODE_TUI_KITTY=0; raw-vt already parses the bounded CSI/CSI-u key forms",
        ],
        "vscode_family" => vec![
            "IDE PTYs may drop Ctrl modifiers; use Alt+Enter/Ctrl+J for a newline",
            "compare with a terminal-native shell before enabling Kitty keyboard mode",
        ],
        "apple_terminal" => vec!["legacy path is intentional; use Alt+Enter/Ctrl+J for a newline"],
        "jetbrains" => vec!["IDE terminal stays on the legacy path; use Alt+Enter/Ctrl+J"],
        "legacy_vte" => vec!["legacy VTE stays on the legacy path; use Alt+Enter/Ctrl+J"],
        "multiplexer_unknown" => vec![
            "tmux/screen may rewrite keyboard negotiation; configure passthrough or keep KKP disabled",
        ],
        "unknown_terminal" => vec![
            "Kitty keyboard mode is disabled; set RIDGECODE_TUI_KITTY=1 only after verifying the PTY",
        ],
        "known_terminal_legacy" | "vte_known_legacy" => vec![
            "host identity alone cannot prove KKP survives every PTY hop; set RIDGECODE_TUI_KITTY=1 only after a verified replay",
            "use Alt+Enter/Ctrl+J for a newline, or set RIDGECODE_TUI_KITTY=0 to force the legacy comparison",
        ],
        "explicit_override" => vec![
            "Kitty keyboard mode was forced; set RIDGECODE_TUI_KITTY=0 to compare the legacy path",
        ],
        "explicit_disable" => vec!["Kitty keyboard mode was explicitly disabled"],
        _ => vec!["use Alt+Enter/Ctrl+J if Enter or Tab is not preserved by the host"],
    }
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|character| !matches!(character, ' ' | '-' | '_' | '.'))
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

fn is_vscode_family(env: &HashMap<String, String>) -> bool {
    env_value(env, "VSCODE_GIT_ASKPASS_MAIN").is_some_and(|value| {
        let lower = value.to_ascii_lowercase();
        lower.contains("vscode") || lower.contains("cursor") || lower.contains("windsurf")
    }) || matches!(
        env_value(env, "TERM_PROGRAM").map(normalize).as_deref(),
        Some("vscode" | "cursor" | "windsurf" | "zed")
    ) || env_value(env, "CURSOR_TRACE_ID").is_some()
}

fn is_apple_terminal(env: &HashMap<String, String>) -> bool {
    matches!(
        env_value(env, "TERM_PROGRAM").map(normalize).as_deref(),
        Some("appleterminal")
    ) || env_value(env, "TERM_SESSION_ID").is_some()
        && env_value(env, "TERM_PROGRAM").is_none()
        && env_value(env, "LC_TERMINAL").is_some_and(|value| normalize(value) == "appleterminal")
}

#[cfg(test)]
mod tests {
    use super::{
        clear_active_input_backend, set_active_input_backend, terminal_diagnostics_from_env,
        terminal_doctor_report, terminal_input_backend_from_env, terminal_keyboard_policy_from_env,
        TerminalInputBackend, TerminalInputBackendKind, TerminalKeyboardPolicy,
    };
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn conservative_unknown_and_hostile_terminals_stay_legacy() {
        for variables in [
            vec![("TERM", "xterm-256color")],
            vec![("TERM_PROGRAM", "vscode")],
            vec![("TERM_PROGRAM", "Apple_Terminal")],
            vec![("VTE_VERSION", "7402")],
            vec![("TMUX", "/tmp/tmux,1,1")],
        ] {
            let policy = terminal_keyboard_policy_from_env(&env(&variables), false);
            assert!(!policy.keyboard_enhancement, "{variables:?}");
            assert_eq!(policy.newline_label(), "Alt+Enter/Ctrl+J newline");
        }
    }

    #[test]
    fn terminal_identity_does_not_enable_kitty_protocol_by_itself() {
        for variables in [
            vec![("TERM_PROGRAM", "kitty")],
            vec![("TERM_PROGRAM", "WezTerm")],
            vec![("TERM_PROGRAM", "ghostty")],
            vec![("VTE_VERSION", "8200")],
        ] {
            let policy = terminal_keyboard_policy_from_env(&env(&variables), false);
            assert_eq!(
                policy,
                TerminalKeyboardPolicy {
                    keyboard_enhancement: false,
                    reason: if variables[0].0 == "VTE_VERSION" {
                        "vte_known_legacy"
                    } else {
                        "known_terminal_legacy"
                    },
                },
                "{variables:?}"
            );
        }
    }

    #[test]
    fn windows_and_explicit_override_are_deterministic() {
        let windows = terminal_keyboard_policy_from_env(&env(&[("TERM_PROGRAM", "kitty")]), true);
        assert_eq!(windows.reason, "windows_native_input");
        assert!(!windows.keyboard_enhancement);
        let windows_diag = terminal_diagnostics_from_env(&env(&[]), true);
        assert!(windows_diag.virtual_terminal_input);
        assert_eq!(windows_diag.input_backend.reason, "windows_auto");
        assert!(windows_diag
            .report()
            .contains("input backend: raw-vt (windows_auto)"));
        for value in ["0", "true", "yes", "2"] {
            let disabled_vt =
                terminal_diagnostics_from_env(&env(&[("RIDGECODE_TUI_VT_INPUT", value)]), true);
            assert_eq!(
                disabled_vt.input_backend.kind,
                if ["0", "true", "yes", "2"].contains(&value) {
                    TerminalInputBackendKind::Crossterm
                } else {
                    TerminalInputBackendKind::RawVt
                },
                "value={value}"
            );
        }
        let enabled_vt =
            terminal_diagnostics_from_env(&env(&[("RIDGECODE_TUI_VT_INPUT", "1")]), true);
        assert!(enabled_vt.virtual_terminal_input);
        assert!(enabled_vt.report().contains("VT input: enabled"));

        let forced = terminal_keyboard_policy_from_env(
            &env(&[("RIDGECODE_TUI_KITTY", "1"), ("TERM_PROGRAM", "vscode")]),
            true,
        );
        assert_eq!(forced.reason, "explicit_override");
        assert!(forced.keyboard_enhancement);

        let disabled = terminal_keyboard_policy_from_env(
            &env(&[("RIDGECODE_TUI_KITTY", "0"), ("TERM_PROGRAM", "kitty")]),
            false,
        );
        assert_eq!(disabled.reason, "explicit_disable");
        assert!(!disabled.keyboard_enhancement);
    }

    #[test]
    fn input_backend_policy_is_pure_and_bounded() {
        assert_eq!(
            terminal_input_backend_from_env(&env(&[]), false),
            TerminalInputBackend::crossterm("non_windows")
        );
        assert_eq!(
            terminal_input_backend_from_env(&env(&[]), true),
            TerminalInputBackend::raw_vt("windows_auto")
        );
        assert_eq!(
            terminal_input_backend_from_env(&env(&[("RIDGECODE_TUI_VT_INPUT", "auto")]), true),
            TerminalInputBackend::raw_vt("windows_auto")
        );
        assert_eq!(
            terminal_input_backend_from_env(&env(&[("RIDGECODE_TUI_VT_INPUT", "1")]), true),
            TerminalInputBackend::raw_vt("explicit_override")
        );
        assert_eq!(
            terminal_input_backend_from_env(&env(&[("RIDGECODE_TUI_VT_INPUT", "0")]), true),
            TerminalInputBackend::crossterm("explicit_disable")
        );
        assert_eq!(
            terminal_input_backend_from_env(&env(&[("RIDGECODE_TUI_VT_INPUT", "bogus")]), true),
            TerminalInputBackend::crossterm("invalid_override")
        );
    }

    #[test]
    fn doctor_reports_bridge_facts_without_arbitrary_environment() {
        let diagnostics = terminal_diagnostics_from_env(
            &env(&[
                ("TERM", "xterm\n256color"),
                ("TERM_PROGRAM", "vscode"),
                ("SSH_TTY", "\\.\\pipe\\ridge"),
                ("TMUX", "/tmp/tmux,1,1"),
                ("RIDGECODE_API_KEY", "must-not-appear"),
            ]),
            false,
        );
        assert_eq!(diagnostics.host, "unix");
        assert_eq!(diagnostics.bridge, Some("ssh"));
        assert_eq!(diagnostics.multiplexer, Some("tmux"));
        assert_eq!(diagnostics.policy.reason, "vscode_family");
        assert_eq!(diagnostics.term.as_deref(), Some("xterm256color"));
        let report = diagnostics.report();
        assert!(report.contains("bridge: ssh"));
        assert!(report.contains("multiplexer: tmux"));
        assert!(report.contains("keyboard: legacy (vscode_family)"));
        assert!(!report.contains("RIDGECODE_API_KEY"));
        assert!(!report.contains("must-not-appear"));
    }

    #[test]
    fn doctor_gives_known_terminal_isolation_hint() {
        let diagnostics = terminal_diagnostics_from_env(
            &env(&[("TERM_PROGRAM", "WezTerm"), ("COLORTERM", "truecolor")]),
            false,
        );
        assert_eq!(diagnostics.policy.reason, "known_terminal_legacy");
        assert!(!diagnostics.policy.keyboard_enhancement);
        assert!(diagnostics.report().contains("RIDGECODE_TUI_KITTY=1"));
    }

    #[test]
    fn explicit_kitty_opt_in_is_the_only_non_windows_activation() {
        let diagnostics = terminal_diagnostics_from_env(
            &env(&[("TERM_PROGRAM", "WezTerm"), ("RIDGECODE_TUI_KITTY", "1")]),
            false,
        );
        assert_eq!(diagnostics.policy.reason, "explicit_override");
        assert!(diagnostics.policy.keyboard_enhancement);
        assert!(diagnostics.report().contains("kitty keyboard: enabled"));
    }

    #[test]
    fn active_backend_state_overrides_doctor_until_guard_releases_it() {
        clear_active_input_backend();
        set_active_input_backend(TerminalInputBackend::raw_vt("test_raw"));
        assert!(terminal_doctor_report().contains("input backend: raw-vt (test_raw)"));
        set_active_input_backend(TerminalInputBackend::crossterm("raw_reader_error"));
        assert!(terminal_doctor_report().contains("input backend: crossterm (raw_reader_error)"));
        clear_active_input_backend();
    }
}
