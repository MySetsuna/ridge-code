# ITER-61 · 输入语义分流与快速收集延迟

## 决策

- Crossterm 已解码的 `Enter`/`Tab` 按下事件属于语义快捷键，不进入快速粘贴候选；故普通提交、补全不再等待粘贴延续窗口。
- 快速收集器对已具来源的 literal C0 `CR/LF/HT` 使用 10ms 探测、100ms 有界延续；普通可打印与已解码语义事件不启动该收集器。
- ConPTY 的 dangling `Enter`/`Tab` release 回退仅在 `RIDGE_TUI_UNWRAPPED_BRIDGE=1`
  显式兼容模式保留；默认成对/孤立语义事件均不得重分类。
- 按键去重身份对 ASCII 字符折叠大小写，修复 Shift 在 key-up 前释放导致的重复字符。
- `BackTab` 在补全浮窗内映射为 `PopupPrev`；普通 Tab 仍为 `PopupAccept`。
- Linux legacy PTY 的相邻 `Ctrl-J`/`Tab` Press 仅在
  `RIDGE_TUI_UNWRAPPED_BRIDGE=1` 显式兼容模式作为无包围多行桥接；独立按键仍保持
  换行/补全语义。
- 普通可打印事件（含空格）与已配对的 `Enter`/`Tab` Press/Release 直通语义路由，
  不启动 10ms 快速收集器；默认仅 literal C0 可取得收集资格。窄版
  `Ctrl-J`/`Tab` bridge 与先验未见 Press 的 ConPTY dangling Release 均须显式兼容开关。

## 证据契约

- 语义回归：rapid 输入 7 项、控制字节矩阵、Shift key-up 修饰符变化、BackTab popup 路由。
- `cargo test --workspace --locked --offline`、`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`。
- Windows `-InputFixture` / `-BusyFixture` 复跑，保留 raw BS/DEL/TAB/LF、bracketed fallback、unwrapped exact-path 或丢失正文失败证据与输出上限。
- Windows 夹具每次使用 GUID 隔离临时 profile/config，重复回放不继承旧 snapshot、keylog 或 trace。
- SpecTree `export → check` 与 `stc validate/status` 必须无 stale、无 error；native Windows console、macOS/Linux 物理 PTY 仍不作已验证声明。

## 未决边界

跨终端物理矩阵、Unix SSH/Jupyter/IDE bridge 的硬独占自定义 reader，以及 raw-byte→Crossterm event→semantic action 的实机回放仍需独立环境证据；本轮仅收紧可由本地事件序列确定的根因。

## 续证（2026-08-23）

- 增加同源终端诊断：`ridgecode terminal doctor`（无需进入 TUI）与 `/doctor` 共用
  `terminal_keyboard_policy`，只输出 TERM/终端桥接/复用器等白名单事实、KKP 策略及
  Enter/Tab 安全回退；不打印任意环境变量。故 IDE、SSH、tmux/screen、Windows native
  等“事件未到达/被改写”边界有可执行定位路径，文档接口与代码一致。
- Linux legacy PTY 会把无包围粘贴的 LF/HT 解码成 `Ctrl-J`/`Tab` Press；新增有界
  Ctrl-J+Tab bridge，但仅 `RIDGE_TUI_UNWRAPPED_BRIDGE=1` 时在相邻多行形状中转为同一
  `Event::Paste`，独立 Ctrl-J/Tab 默认仍走原语义。
- `scripts/linux-pty-input.py` 以真实 POSIX PTY（显式 120×40 geometry）回放 BS/DEL/space、Tab/Shift-Tab、CSI/OSC bracketed payload、`raw\n\ttail` 与独立 LF；仅为 legacy raw-LF/HT 断言显式设置 `RIDGE_TUI_UNWRAPPED_BRIDGE=1`；WSL Ubuntu-22.04 smoke 通过，输出低于 4 MiB。
- 未宣称 macOS/native terminal 或 Unix SSH/Jupyter/IDE 硬独占 reader/replay 已完成。

## 终端能力策略修正（2026-08-23）

终端名称与 `VTE_VERSION` 只能识别宿主，不能证明所有 PTY 跳转均保留 Kitty
keyboard protocol；故 `terminal_keyboard_policy` 默认保持 legacy。仅用户确认
字节链路后设置 `RIDGE_TUI_KITTY=1`，普通路径使用 `Alt+Enter/Ctrl+J`，避免自动
推送协议导致 Enter/Tab/空格失真。

## 输入来源修正（2026-08-23）

Windows ConPTY 在无包围多行回放中可能只留下 `Enter`/`Tab` Release，抹掉
literal C0 来源。实现现以已消费的 Press 集合作 provenance 门：只有未匹配的
Release 才能进入窄回退；普通字符、空格和成对 Press/Release 永不因 10ms 窗口
被延迟。复跑发现另一种真实失败：ConPTY 可把 `LF/HT` 改写为成对
`Ctrl+Enter/Tab`，导致正文丢失；该事件序列与真实快捷键不可区分，故
`windows-pty-e2e.ps1 -InputFixture` 不再接受 semantic fallback，必须取得精确
`axyzraw\n\ttail` 快照后才算通过。默认路径的语义 Enter/Tab 不进入该推断桥。
