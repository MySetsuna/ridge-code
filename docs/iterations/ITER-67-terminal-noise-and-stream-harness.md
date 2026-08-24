# ITER-67 · 终端控制噪声与确定性流式 Harness

## 问题

同批 paste+Enter、跨批 SGR mouse/focus 残片及子进程遗留 Windows VT input mode，皆可令正文多键、少键或重复提交。仅以延时脚本模拟流式响应，又无法确定性复现 chunk 边界竞态。

## 收束

- Windows raw-VT 增量 parser 将 focus、SGR mouse 解为事件，不向 composer 泄漏控制字节。
- Crossterm fallback 先有界批量 drain，再经持久 `CsiNoiseFilter` 过滤跨批 mouse/focus；普通 `[I`、`[O`、`[`、空格、Enter、Tab 原样保留。
- Windows fallback 每次阻塞 poll 前及 read 后重申关闭 `ENABLE_VIRTUAL_TERMINAL_INPUT`。
- `InputFixture` 新增同一次物理 ConPTY 写入的 bracketed paste+CRLF：须恰见一个 Paste、一个 Enter、零输入残留及第二任务 busy。
- `ScriptedProvider` 新增按请求消费的 ordered stream；每个 chunk 可由 oneshot gate 精确放行，gate 关闭则 fail-closed 且不泄漏尾段。

## 证据

- `cargo test --workspace --locked`：agent lib 237、TUI/bin 503、eval 16、eval CLI 2、langgraph 9、MCP 8、provider 58、tools 27（1 ignored）、doctest 1，全绿。
- `cargo fmt --all -- --check`、workspace clippy `-D warnings`、workspace build、`git diff --check` 全绿。
- Windows raw-VT `InputFixture` 到 stage 11：同写入 Paste+CRLF、空输入及 busy 证据全真；输出 425812B，draw p95 2256µs、max 3823µs。
- Windows Crossterm `CompletionFixture+ResizeProbe`：88 帧，p95 2691µs、max 5334µs；completion/diff/fold/table/highlight/resize 全真。`BusyFixture`：队首/FIFO/接管全真，p95 1592µs、max 3934µs。
- WSL Ubuntu-22.04 真实 PTY：BS/DEL/space/Tab/Shift-Tab/bracketed/unwrapped/独立 LF 全过，输出 1385540B，低于 4MiB。
- bounded soak 10×3 全过；kill→restart recovery：首次进程终止后复用 2、执行 1，三 case 全批准。

## 未宣称

macOS/native Unix 物理终端矩阵、24h 多故障耐久、snapshot 写盘与端到端事件循环延迟、外部 A2A 长连接故障注入及远端 Sonar 仍未闭合。
