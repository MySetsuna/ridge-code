# ITER-62 · Windows VT 输入来源与夹具证据

## 根因

Crossterm 0.28.1 的 Windows 输入路径使用 `ReadConsoleInputW`/`INPUT_RECORD`，默认并非
原始 VT 字节 reader。ConPTY 仍可能把 bracketed body 或无包围多行中的 C0 字节改写为成对
`Ctrl+Enter`/`Tab` 事件；该形状与真实快捷键无法区分，应用层不可安全猜测正文或把语义键
重分类为 paste。

## 修复与边界

- `ENABLE_VIRTUAL_TERMINAL_INPUT` 默认关闭，仅精确 `RIDGE_TUI_VT_INPUT=1` 实验性开启 VT
  字节路径；`terminal doctor` 与 TUI `/doctor` 使用并报告同一策略，其他值均保持关闭。
- `reassert_virtual_terminal_input` 只在目标 bit 与当前 console mode 不同时调用 `SetConsoleMode`。
  该重申不改变 Crossterm 默认的 `INPUT_RECORD` reader，也不把 semantic fallback 当作通过。
- `windows-pty-e2e.ps1 -InputFixture` 原子发送含 CSI/OSC 控制码的 bracketed payload，产出
  `axyz`；另测原始 TAB/Shift-Tab。它不设置 `RIDGE_TUI_UNWRAPPED_BRIDGE=1`，不注入或断言
  `axyzraw\n\ttail`，而是在 keylog 边界证明物理 LF 已观察后再独立发送 LF。
- `Event::Paste` 与 raw bracketed fallback 单测覆盖 multiline CRLF/CR 归一及 CSI/OSC 清理。
  ConPTY 若把 bracketed-body C0 改写为成对语义键，夹具保留 keylog/snapshot 并失败；真实
  bracketed multiline 与 unwrapped multiline 仍是明确 transport gap，应用不从 semantic
  `Enter`/`Tab` 推断 paste。

## 证据契约

- 委派验收：`InputFixture` 3/3、`BusyFixture` 1/1。
- 主验收：3 次 `InputFixture` 均 exit 0；其中一份结构化输出的输入证据字段全部为 true，
  `output_bytes=180570`，且受 `-MaxOutputBytes` 上限约束。
- VTI native tests 6 项通过，terminal doctor test 1 项通过；fmt/build 通过。
- Coverage TOTAL：lines 83.37%、regions 82.70%、functions 82.97%；`fail-under-lines 80` 通过。
- bounded soak：PASSED，10 iterations × 3 concurrency；iterations 1/6/7 各有 1 个
  `timed_out_case`，每轮至少 1 个 passed case，不宣称 zero timeout。
- A2A smoke：approved；reconnect 覆盖 2 external sessions。

## 未决边界

Windows ConPTY 对 bracketed-body C0、无包围多行 C0 的重写仍需宿主/终端逐一补证；不宣称
跨终端 bracketed multiline 或 unwrapped multiline 可恢复。SpecTree 已完成投影：graphHash
`50e9da530a13f41e38a9d7e548254aae6b2ca309afa18959469b180063920ce6`，23 nodes / 159 targets /
64 Rust / 23 notes，`ALIGNED`、`VALID`、stale `[]`。
