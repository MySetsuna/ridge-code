---
id: L3-AGENT-UI-001
level: L3
parent: L2-AGENT-001
title: TUI state, panels, rendering, and presentation
status: VALID
code_targets:
  - crates/agent/src/tui/app.rs
  - crates/agent/src/tui/clipboard.rs
  - crates/agent/src/tui/command.rs
  - crates/agent/src/tui/draw.rs
  - crates/agent/src/tui/eventfmt.rs
  - crates/agent/src/tui/keymap.rs
  - crates/agent/src/tui/panel.rs
  - crates/agent/src/tui/presentation.rs
  - crates/agent/src/tui/render.rs
  - crates/agent/src/tui/status.rs
  - crates/agent/src/tui/transcript.rs
  - crates/agent/src/tui/turn_view.rs
  - crates/agent/src/main.rs
  - scripts/windows-pty-e2e.ps1
test_targets:
  - crates/agent/src/tui/tests.rs
  - crates/agent/src/tui/idle_submit_tests.rs
  - crates/agent/src/tui/turn_chrome_tests.rs
  - crates/agent/src/main.rs
  - scripts/windows-pty-e2e.ps1
public_interface:
  - agent::tui::run
  - agent::tui::Panel
  - ridgecode TUI panel, transcript, and status rendering
known_gap:
  - Reusable frame-sequence/golden assertions remain outside the workspace gate; combined Completion+Resize evidence is covered by the hermetic PTY fixture.
---

# TUI state, panels, rendering, and presentation

The hermetic `CompletionFixture + ResizeProbe` path covers combined evidence for the real `read_file -> edit_file -> final` sequence, folded tool output, answer table/highlight rendering, and runtime viewport changes. When `RIDGECODE_TUI_SNAPSHOT` is enabled, telemetry retains at most 4096 exact samples and reports nearest-rank p95/max for draw rendering, event-to-frame latency, snapshot serialization/write, and payload bytes. The Windows ConPTY gate keeps draw at 16/50 ms p95/max, event-to-frame at 25/100 ms, snapshot I/O at 25/300 ms, and snapshot payloads at 512 KiB/1 MiB. Snapshot writes use a bounded background queue so Windows filesystem outliers do not block input; normal runs still perform no snapshot serialization or file I/O.

TUI v2 routes configurable global actions through one registry. `Ctrl+P` opens the searchable command palette and `F1`/`/help` opens the effective keybinding panel. `Config.keybindings` is an atomic action-to-chord override: invalid actions, reserved editor/safety keys, malformed chords, or conflicts revert the complete override to defaults and surface a diagnostic. Editor, approval, submission, and takeover keys remain fixed so terminal normalization and safety behavior cannot be remapped.

TTY 交互由 app/panel/command 维护状态；draw/render/presentation/status 将其投影
为稳定视口。剪贴板、事件格式化与 transcript/turn view 属同一显示边界，测试
覆盖语义动作与布局纯函数；跨终端像素级验证另行由 PTY/截图闸门承担。
