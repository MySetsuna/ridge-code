---
id: L3-AGENT-INPUT-001
level: L3
parent: L2-AGENT-001
title: Terminal input normalization and routing
status: VALID
code_targets:
  - Cargo.toml
  - crates/agent/src/main.rs
  - crates/agent/src/tui/input.rs
  - crates/agent/src/tui/keymap.rs
  - crates/agent/src/tui/csi.rs
  - crates/agent/src/tui/command.rs
  - crates/agent/src/tui/terminal.rs
  - crates/agent/src/tui/terminal_noise.rs
  - crates/agent/src/tui/raw_vt.rs
  - crates/agent/src/tui/mod.rs
  - crates/agent/src/tui/draw.rs
  - crates/agent/src/tui/render.rs
  - scripts/windows-pty-e2e.ps1
  - scripts/linux-pty-input.py
test_targets:
  - crates/agent/src/tui/command.rs
  - crates/agent/src/tui/idle_submit_tests.rs
  - crates/agent/src/tui/tests.rs
  - crates/agent/src/tui/csi.rs
  - crates/agent/src/tui/terminal.rs
  - crates/agent/src/tui/terminal_noise.rs
  - crates/agent/src/tui/raw_vt.rs
  - crates/agent/src/tui/render.rs
  - scripts/linux-pty-input.py
public_interface:
  - tui::decide_key
  - tui::input_action
  - tui::feed_nav_key
  - ridgecode TUI Enter/Tab/paste behavior
  - ridgecode terminal doctor
  - Config.keybindings action-to-chord map
known_gap:
  - Linux PTY smoke now runs under WSL Ubuntu-22.04; macOS/native terminal matrix and physical host coverage remain pending.
  - Unix SSH/Jupyter/IDE bridge input now opts into Crossterm's raw `/dev/tty` descriptor poll/select path; a hard-exclusive reader and replay path are not yet implemented or verified.
  - Native macOS/Unix terminal matrices and physical host coverage remain pending; Windows raw-VT activation is covered by the deterministic parser matrix and the ConPTY `-InputFixture`, not by every native console/PTY combination.
---

# Terminal input normalization and routing

The TUI consumes Crossterm events through one boundary: raw CR/LF and Ctrl-M
normalize to Enter, BS/DEL normalize to Backspace, and release-only legacy
events become one semantic Press. `feed_nav_key` decodes explicit escape
sequences while preserving ordinary bracket text; bare Kitty CSI-u is accepted
only after keyboard enhancement was advertised, and an orphaned Escape prefix
times out back into literal text. `input_action` owns popup, newline, submit,
queue, and interrupt routing. Bracketed paste is normalized at the same
boundary: CRLF/CR become LF, control characters stay out, and complete CSI,
OSC, DCS, SOS, PM, and APC sequences are discarded so escape-code tails cannot
become visible input. Exact bare `[200~`/`[201~` markers are also accepted for
ConPTY hosts that strip the leading ESC; the raw fallback buffers the payload
and runs the same sanitizer before insertion while bare Kitty CSI-u remains
gated. Standard VT/xterm Shift-Tab (`ESC [ Z`, including the modifier form
`ESC [ 1 ; 2 Z`) maps to `BackTab`; ConPTY hosts that strip ESC are handled by
the equivalent bare `[Z`/`[1;2Z` tails. No bracket or `Z` residue may enter the
editor buffer. Legacy Alt+Enter encodings (`ESC CR` and `ESC LF`) are folded
into one `Enter + Alt` event before routing, so they cannot become Escape plus
an accidental submit.

Keyboard-enhancement negotiation is capability-gated by `terminal_keyboard_policy`.
Windows native input, VS Code/xterm.js, Apple Terminal, JetBrains terminals,
legacy/new VTE, known Kitty/Ghostty/WezTerm/Alacritty/Rio/iTerm/Warp terminals,
unknown multiplexers, and unknown terminals all remain on the legacy path by
default: environment identity cannot prove that every PTY hop preserves KKP.
`RIDGECODE_TUI_KITTY=1` is the only explicit opt-in override, while
`RIDGECODE_TUI_KITTY=0` is an explicit disable. The status hint follows the
negotiated result: enhanced terminals advertise `Shift/Alt+Enter`, legacy
  terminals advertise `Alt+Enter/Ctrl+J`.
On Windows, input has one owner. The default (`RIDGECODE_TUI_VT_INPUT` unset or
`auto`) attempts `ENABLE_VIRTUAL_TERMINAL_INPUT` on a console and uses a raw
byte reader; a ConPTY/redirected pipe is treated as an already-byte-oriented
raw-VT transport. `RIDGECODE_TUI_VT_INPUT=1` forces the same request,
`RIDGECODE_TUI_VT_INPUT=0` forces the legacy Crossterm reader, and an invalid value
falls back to Crossterm. Console activation is atomic: a failed mode change
does not start the raw reader. Raw reader failure is logged as a bounded
reason and falls back on the same reader thread, so the two readers never race.
`terminal doctor`, TUI `/doctor`, and the optional keylog report `raw-vt` or
`crossterm` plus a non-secret reason.
This mirrors Grok Build's conservative terminal-capability policy and avoids
probing terminals that are known to misencode modified keys. `ridgecode terminal doctor`
and TUI `/doctor` render the same non-secret environment facts, negotiated
policy, bridge/multiplexer hints, and safe fallback; this makes a lost
Enter/Tab reportable without guessing or printing arbitrary environment data.

On Unix, the workspace enables Crossterm's `use-dev-tty` raw descriptor poll/select
backend so bridge hosts do not rely on mio polling a redirected stdin stream. This
improves source selection but is not yet a hard-exclusive custom reader.

The raw parser (`raw_vt.rs`) is incremental across arbitrary read chunks. It
decodes UTF-8, CR/LF/HT/BS/DEL, Alt and bounded CSI/SS3 navigation, Home/End,
Delete/Page keys, Shift-Tab, CSI-u modifier/lifecycle fields, focus, SGR mouse,
and exact bracketed-paste `200~`/`201~` markers. Paste bytes remain intact until
the existing `sanitize_paste` boundary normalizes CRLF once and removes
terminal controls. Unknown or incomplete escape sequences wait at most the
bounded sequence timeout, then replay as literal input; pending/paste buffers
are size-bounded. Raw events bypass the legacy rapid/burst classifier.

The Crossterm fallback drains an immediately available batch through one
persistent, bounded `CsiNoiseFilter`. It removes only complete SGR mouse and
focus reports that escaped terminal decoding, including reports split across
poll batches. Ordinary `[`, `[I`, `[O`, spaces, Enter, and Tab remain literal or
semantic input. On Windows the fallback reasserts that console VT input is off
before each blocking poll and after each read, so a child process cannot leave
the shared console in a mode that changes subsequent key meanings.

The PTY contract is intentionally tested as event fixtures, not as simulated
text insertion. `windows-pty-e2e.ps1 -InputFixture` runs a phased state machine:
`a b` -> raw BS -> reinsert `b` -> raw DEL -> cleanup BS -> raw TAB -> VT
Shift-Tab -> one atomic bracketed payload containing CSI/OSC controls. It waits
for distinct snapshot states `a b`, `a `, `a b`, `a `, `a`, then `axyz`, and
uses a fresh keylog boundary for Tab, Shift-Tab, bracketed paste, and final LF.
Thus the final buffer cannot falsely prove an earlier deletion or transport
event. The result records `input_backend=raw-vt` and its reason, and requires
that backend for this fixture; Shift-Tab and paste therefore arrive as parser
events rather than semantic-key guesses. The
fixture does not set `RIDGECODE_TUI_UNWRAPPED_BRIDGE` and does not inject or assert
an unwrapped `raw\n\ttail` body. After the `axyz`/Tab/Shift-Tab evidence is
observed, it sends a separate physical LF; only the keylog bytes appended after
that boundary prove the LF reached the application. After that task completes,
the fixture sends a second bracketed paste plus CRLF in one physical ConPTY
write. A fresh keylog boundary must contain exactly one `Paste` and one `Enter`,
the editor must be empty, and the second task must be busy; this prevents the
classic paste-then-Enter race from duplicating a submit or leaving residue.
ConPTY may rewrite C0
bytes in a bracketed body into paired `Ctrl+Enter`/`Tab` events; that sequence
is source-indistinguishable from real shortcuts, so the fixture fails and the
app never treats semantic keys as paste. Real native-console and non-Windows
physical terminal matrices remain explicit acceptance boundaries. Each Windows
fixture invocation uses
a fresh GUID-scoped temp profile/config, so repeated runs cannot inherit stale
snapshots, keylogs, or traces. Output remains bounded by `-MaxOutputBytes`
(default 4 MiB). The reader uses poll/read retries so one transient Crossterm
parse error cannot kill the input thread. Ordinary printable events, including
a normal space, and decoded semantic `Enter`/`Tab` events route immediately;
they never start the rapid collector or wait behind its timeout. By default the
collector earns its bounded 100ms continuation window only after a literal raw C0 boundary. The
legacy Unix Ctrl-J+Tab Press bridge and the ConPTY dangling `Enter`/`Tab`
Release fallback require the explicit `RIDGECODE_TUI_UNWRAPPED_BRIDGE=1` opt-in.
That flag enables a narrow compatibility decoder; it does not make paired
semantic Ctrl+Enter/Tab events safe to reinterpret.
A multiline shape is coalesced only when it contains a literal raw C0
LF/TAB/CR event, or, in that opt-in mode, the narrow ConPTY dangling `Enter`
`Release` plus `Tab` `Release` fallback or a legacy Unix PTY's adjacent
Ctrl-J/Tab Press pair. If a terminal has translated those bytes into paired
`Enter`/`Tab` press/release events, the sequence stays on the semantic key path;
press/release pairs never qualify, preventing a real `Ctrl+Enter` followed by
`Tab` from becoming pasted text. Press/release bookkeeping case-folds ASCII
character identities and tries only the physical modifier aliases, so a
modifier change between key-down and key-up cannot duplicate a shifted
character or leak Ctrl/Alt-Tab and Ctrl-H as literal input. Unknown
Ctrl/Alt/Super character
events are ignored rather than inserted as stray prompt text. Ctrl/Alt-Tab
and raw C0 HT are canonicalized to the live-history `Ctrl-I` shortcut, while
Shift-Tab becomes `BackTab`; plain Tab alone remains completion. Raw LF with
Ctrl remains the physical `Ctrl+Enter` front-queue spelling, and explicit
`Char('j') + Ctrl` remains the multiline `Ctrl-J` spelling. The other fixtures
cover CR, standard/bare Shift-Tab, and explicit Kitty CSI-u. The keyboard
reader publishes into a bounded 4096-event Tokio channel; backpressure is
preferred to dropping keys or allowing unbounded memory growth.

The Linux smoke harness is dependency-free and uses a real POSIX PTY. It sets
`RIDGECODE_TUI_UNWRAPPED_BRIDGE=1` only for the legacy raw-LF/HT compatibility
assertion; unit replay separately proves the default safe route:

```text
wsl.exe -d Ubuntu-22.04 -- bash -lc "cd /mnt/c/code/ridge-code && cargo build -p agent --bin ridgecode --locked && python3 scripts/linux-pty-input.py --binary target/debug/ridgecode"
```

It sets non-zero PTY geometry, waits for the first durable frame before sending
bytes, exercises BS/DEL/space/Tab/Shift-Tab, sanitised bracketed payload,
unwrapped `raw\n\ttail`, and separate LF submission; it fails on any buffer
residue or unbounded output.
Both `Event::Paste` and the raw bracketed fallback have unit coverage for
multiline CRLF/CR normalization and complete CSI/OSC sanitation.
`terminal_input_normalization_matrix_keeps_byte_and_shortcut_meanings`
locks the CR/LF/HT/BS/DEL, Ctrl-M/Ctrl-H, Ctrl/Alt-Tab, Shift-Tab, and explicit
Ctrl-J spellings before routing; `terminal_input_event_replay_preserves_semantic_actions`
replays those event shapes through release filtering and `input_action` so raw
bytes cannot silently drift into a different submit/newline/popup meaning. The
terminal capability matrix has deterministic pure tests. A macOS/Linux terminal
matrix remains a separate acceptance gate; the WSL smoke does not claim native
macOS or physical-terminal coverage.
