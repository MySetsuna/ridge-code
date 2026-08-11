# RidgeCode

RidgeCode 是一个模块化、跨领域可扩展的通用 agent 框架，发布为单一命令 ridgecode。它既能做代码任务，也能做调研、摘要、翻译、分诊等工作；新增能力主要靠 SKILL.md、MCP 配置或自定义命令，不必改 Rust 内核。

底层是纯 Rust 的 LangGraph 风格有状态图引擎，外层接 provider、工具、MCP、Skills 与 TUI。TUI 重点呈现实际收到的 Answer 与 reasoning_content：Reasoning 单独显示，工具默认收起，长回答可在终端原生 scrollback 中保留。

## 先跑起来

### 其他 PC：通过 GitHub 快速安装（无需 Rust/Cargo）

新 PC 无需 Rust/Cargo：复制当前平台的一条命令，安装器会从 GitHub Release 下载对应归档、校验 SHA256，并把 `ridgecode` 加入用户 PATH。当前稳定版为 `v0.5.16`；每个归档内同时带有完整 `README.md` 与安装脚本，便于离线转交和审计。

Windows PowerShell：

~~~powershell
# 最新稳定版：安装到 %LOCALAPPDATA%\Programs\ridgecode，并写入用户 PATH
irm https://raw.githubusercontent.com/MySetsuna/ridge-code/main/scripts/install.ps1 | iex

# 可复现安装：固定脚本与 Release 版本 v0.5.16
$s = irm https://raw.githubusercontent.com/MySetsuna/ridge-code/v0.5.16/scripts/install.ps1
& ([scriptblock]::Create($s)) -Version v0.5.16

# 新开终端后验证
ridgecode --version
Get-Command ridgecode
~~~

Linux / macOS：

~~~bash
# 最新稳定版：安装到 ~/.local/bin
curl -fsSL https://raw.githubusercontent.com/MySetsuna/ridge-code/main/scripts/install.sh | sh

# 可复现安装：固定脚本与 Release 版本 v0.5.16
curl -fsSL https://raw.githubusercontent.com/MySetsuna/ridge-code/v0.5.16/scripts/install.sh | sh -s -- --version v0.5.16

# 验证
ridgecode --version
command -v ridgecode
~~~

安装器首次运行会生成 `~/.ridge/config.json`（Windows 为 `%USERPROFILE%\.ridge\config.json`）与 `config.example.json`；填入 API Key 或设置 `RIDGE_API_KEY` 后即可启动真实模型。安装完成后若当前 shell 尚未刷新 PATH，请新开终端。

首次启动示例：

~~~powershell
$env:RIDGE_API_KEY = "your-key"
ridgecode
~~~

Linux / macOS 将 `$env:RIDGE_API_KEY` 改为 `export RIDGE_API_KEY="your-key"`。安装器支持 Windows x86_64、Linux x86_64/ARM64、macOS Intel/Apple Silicon；Windows ARM64 可改用 WSL 或从源码构建。企业或审计环境可先下载 `scripts/install.ps1` / `scripts/install.sh` 检查内容，再用本地脚本执行；安装器也支持 `-Dir`、`--dir` 自定义目录。

升级时重复执行对应平台的最新版安装命令即可；安装器会覆盖旧二进制，不改已有 `~/.ridge/config.json`。卸载仅需删除安装目录中的二进制（配置默认保留）：Windows 删除 `%LOCALAPPDATA%\Programs\ridgecode\ridgecode.exe`，Linux / macOS 删除 `~/.local/bin/ridgecode`。

当前 Release：[v0.5.16](https://github.com/MySetsuna/ridge-code/releases/tag/v0.5.16)。手动下载时按平台选择：

| 平台 | Release 资产 |
| --- | --- |
| Windows x86_64 | `ridgecode-x86_64-pc-windows-msvc.zip` |
| Linux x86_64 / ARM64 | `ridgecode-x86_64-unknown-linux-gnu.tar.gz` / `ridgecode-aarch64-unknown-linux-gnu.tar.gz` |
| macOS Intel / Apple Silicon | `ridgecode-x86_64-apple-darwin.tar.gz` / `ridgecode-aarch64-apple-darwin.tar.gz` |

每个归档旁均有同名 `.sha256` 文件；手动下载后先校验，再解压并将二进制放入 PATH。Windows 可用 `Get-FileHash .\ridgecode-*.zip -Algorithm SHA256`，Linux / macOS 可用 `sha256sum ridgecode-*.tar.gz` 或 `shasum -a 256 ridgecode-*.tar.gz` 对照校验文件。安装器参数：Unix 支持 `--version <tag>`、`--local <binary>`、`--dir <dir>`；Windows 支持 `-Version <tag>`、`-Local <path>`、`-Dir <dir>`。本地安装不联网：`scripts/install.sh --local target/release/ridgecode` 或 `scripts/install.ps1 -Local target/release/ridgecode.exe`。

若目标 PC 不能执行远程脚本，可用 GitHub CLI 下载完整归档，再离线传给目标 PC：

~~~bash
gh release download v0.5.16 --repo MySetsuna/ridge-code --pattern 'ridgecode-*'
~~~

归档内包含二进制、完整 `README.md` 与对应安装脚本；解压后可用 `--local` / `-Local` 安装，仍会校验归档旁的 `.sha256`。

### 从源码运行

需要 Rust stable 与 Cargo：

~~~bash
cargo build --workspace
cargo run -p agent --bin ridgecode -- --help
cargo run -p agent --bin ridgecode -- --version
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
~~~

## 命令行用法

~~~text
ridgecode                              启动交互式 TUI；非 TTY 自动进入 headless
ridgecode "任务"                       执行一次任务
ridgecode --cwd <dir> "任务"           在指定项目目录执行
ridgecode --resume                     恢复 ~/.ridge/session.json 的会话
ridgecode --continue                   --resume 别名
ridgecode --every <duration> "任务"    一次性任务按周期重复执行
ridgecode --read-only "任务"           只提供读取、搜索、研究能力
ridgecode --yolo "任务"                自动批准工具；灾难命令仍硬拦截
ridgecode --skip-permissions "任务"    --yolo 别名
ridgecode --dangerously-skip-permissions "任务"  --yolo 别名
ridgecode -h / --help                  显示帮助
ridgecode -V / --version               显示版本
ridgecode login ...                    接入内置 provider 或 OAuth 订阅
~~~

--every 接受 30s、5m、1h 或不带单位的秒数，只用于带任务文本的一次性模式。--read-only 也可用 --readonly；环境变量 RIDGE_READ_ONLY=1 与 RIDGE_SKIP_PERMISSIONS=1 分别提供对应默认值。

管道或 CI 中，stdin 每行作为一个独立任务串行执行，不启用 TUI 和斜杠命令：

~~~bash
printf "检查编译\n检查测试\n" | ridgecode --read-only
~~~

## Goal 长任务收敛

goal 是本地持久化的单目标生命周期；模型自述不会自动把目标标记为完成。状态文件默认写入 .ridge/goal.json，可用 RIDGE_GOAL_FILE 覆盖。

~~~bash
ridgecode goal create "ship stable release"
ridgecode goal start
ridgecode goal advance verify "workspace tests passed" --next "run PTY smoke"
ridgecode goal block "PTY harness unavailable" --next "install a PTY harness"
ridgecode goal resume
ridgecode goal complete "PTY smoke passed"
ridgecode goal status
~~~

状态包含 active、blocked、completed、cancelled、phase、evidence、failure、next、running、revision；每次更新先写临时文件并原子替换，重启后可直接执行 goal status 回读。TUI 内使用 /goal、/goal create ...、/goal status 等同一组命令；/goal help 查看完整语法。

外部调用有界：shell 默认 RIDGE_SHELL_TIMEOUT=180 秒，MCP 工具默认 RIDGE_TOOL_TIMEOUT=180 秒；超时会返回失败观测并显示 waiting/timeout，不再无限停留在调查阶段。

## Provider 与登录

### API Key 登录

最直接的方式是环境变量：

~~~bash
export RIDGE_API_KEY="your-key"
export RIDGE_PROVIDER="openai"
export RIDGE_MODEL="gpt-4o"
export RIDGE_BASE_URL="https://api.openai.com/v1"
ridgecode
~~~

也可用内置 provider 预设。省略 KEY 时从 stdin 读取，避免密钥进入 shell 历史；登录会先校验连通性，成功后把密钥写入独立的 ~/.ridge/auth.json，不写入 config.json：

~~~bash
ridgecode login --list
ridgecode login deepseek
ridgecode login deepseek "your-key"
ridgecode login kimi --model kimi-k2 --name moonshot --no-default
ridgecode login openai "your-key" --no-verify
~~~

可用预设 id：openai、anthropic、gemini、grok、glm、kimi、deepseek、qwen、hunyuan、minimax、openrouter、siliconflow、together、groq。完整端点、默认模型和密钥变量以 ridgecode login --list 输出为准。

参数：--model <model> 覆盖默认模型，--name <name> 指定档案名，--no-default 只登记档案，--default 设为默认，--no-verify 跳过联网校验。

### OAuth 订阅

~~~bash
ridgecode login --claude
ridgecode login --codex
# 无法监听 localhost:1455 时，使用无本地端口的设备授权
ridgecode login --codex --device-auth
~~~

ChatGPT/Codex 启动时会用 OAuth 账号目录校验当前模型；若配置中的 `gpt-5` 不在账号可用列表，自动切换到目录首个可用模型（例如 `gpt-5.6-sol`），避免把公共 API 模型名误发到订阅端点。`RIDGE_PROVIDER` 优先于配置中的 provider，可用它临时选择 `chatgpt-plus`；`/model` 目录加载后切换会持久化默认模型。

程序打开授权流程，用户在浏览器完成授权。凭据独立保存到 ~/.ridge/oauth.json；不把 access token 打进日志、配置或任务内容。OAuth 端点、账号权限和 provider wire 仍以实际账号与服务端结果为准。

`ridgecode login --codex` 使用 ChatGPT/Codex 订阅通道：授权成功后保存 `id_token` 与 `chatgpt_account_id`，补全请求发往 `https://chatgpt.com/backend-api/codex/responses`，并带 `ChatGPT-Account-Id`。已有旧版 `oauth.json` 若缺少账号标识，需重新执行 `ridgecode login --codex`；可用 `RIDGE_CHATGPT_BASE_URL` 覆盖后端地址。API Key 路径仍使用 `RIDGE_BASE_URL` 与 OpenAI 兼容的 Chat Completions。

`ridgecode login --codex --device-auth` 不占用本机回调端口：浏览器打开设备页，输入一次性设备码，程序自动轮询并保存凭据。

也可先启动 `ridgecode`，在 TUI 输入 `/login`，选择 `codex-oauth`；入口会自动打开浏览器。若 Windows 无法监听已登记的 localhost:1455/1457，Codex TUI 自动切设备授权，无需复制粘贴回调 URL。

## 交互式 TUI

直接运行 ridgecode 即进入 inline TUI。输出会落入终端历史，可用终端自己的滚轮、选择和复制；活动区域只占终端底部固定空间。

### Answer、Reasoning 与工具

- Answer 展示模型实际回答；收到的 reasoning_content 单独作为 Reasoning 展示，不生成或伪造隐藏思考。
- Answer 支持有界行级 Markdown 展示：标题、粗体、行内 code、代码围栏与 ANSI 16 色语义角色。
- Markdown 告警（`NOTE`/`TIP`/`IMPORTANT`/`WARNING`/`CAUTION`）及其后续引用行共享语义侧轨；正文仍按普通 Markdown 折行，不额外挤占输出槽。
- fenced code 的 Live 可见行与落入终端历史的 Answer 都按有限词法规则区分关键字、类型、字符串、数字、字面量和注释；未知文本保持普通 Muted 色，不猜测跨行语法。
- 工具调用默认显示摘要；Ctrl+O 展开当前工具详情。Alt+↑/↓ 选择旧工具并锁定焦点；详情展开且可滚动时，Alt+PageUp/PageDown 查看旧/新详情位置。
- Ctrl+I（部分终端用 Alt+I）或 `/inspect` 打开 Live Block Inspector：按当前流顺序聚合 Answer/Reasoning/Tool，并在同一面板底部显示 pending FIFO；↑↓/PgUp/PgDn 聚焦历史块，Enter 或 Space 展开详情，Delete 删除选中的待执行消息，Ctrl+Q 切到完整队列，筛选与检视均不暂停模型；大段文本仅保留头尾有界预览，避免拖慢重绘。
- Ctrl+R 展开或收起当前实际 Reasoning；当前回合结束后自动保留最近 8 段 reasoning，可再次按 Ctrl+R 或 `/reasoning` 检索、筛选、展开与滚动，不再因进入终端历史而失去入口。
- 顶部状态条显示当前阶段及该阶段已持续时间（`+ms`/`+s`）；Ctrl+T 或 `/activity` 打开最近 5 个真实 Agent 活动，最新阶段置顶，窄终端自动折行。
- 宽屏顶部以低饱和 `⟦SYS›THK›TLS›CHK›SUM›ANS⟧` breadcrumb 显示最近观测相位；`THK` 表示调查/思考，`ANS` 表示回答，`TLS` 表示工具，`CHK` 表示验证，`SUM` 表示结论收束，`WAIT` 表示等待；窄屏保留当前阶段与等待/工具目标，并以 `⏭N` 标出队首待执行数，Ctrl+T 可展开完整活动链。
- Live Answer/Reasoning 默认跟随最新尾部；`Alt+PageUp/PageDown` 暂停并检视较早/较新内容，`Alt+End` 回到最新尾部。检视状态会在顶栏显示。
- 长任务中可继续编辑输入；任务忙时按 Enter 会排队，输入框上方显示 `⏭ next` 与有界 FIFO 预览；Ctrl+Enter 将消息插入队首且不打断当前模型思考，当前任务结束后继续执行。

### 输入与快捷键

| 按键 | 作用 |
|---|---|
| Enter | 空闲时提交；忙时排队 |
| Ctrl+J | 插入换行 |
| Shift+Enter | 支持 CSI-u 的终端插入换行；Windows Terminal 可用 Alt+Enter 或 Ctrl+J |
| Ctrl+C | 首次中断当前任务并进入 takeover；2 秒内再次按下退出整个会话 |
| Ctrl+R | 切换当前 Reasoning；无 live reasoning 时打开 Reasoning History |
| Ctrl+O | 切换工具详情；无 live 工具时打开 Tool History |
| Ctrl+I / Alt+I | 打开/关闭 Live Block Inspector；可检视/删除 pending，不暂停当前任务 |
| Ctrl+T | 打开/关闭最近 Agent 活动 |
| Ctrl+Q | 打开/关闭待执行队列面板 |
| Ctrl+Space | 支持释放事件的终端：按住进入 HOLD、松开回到 FOLLOW；旧终端按键切换，不暂停模型任务 |
| Ctrl+Enter | 忙时将当前输入插入队首，不打断当前任务 |
| Alt+↑/↓ | 选择 live 工具焦点 |
| Alt+PageUp/PageDown | 优先滚动工具详情；否则检视 Live Answer/Reasoning 或 Tool History 详情 |
| Alt+End | Live Answer/Reasoning 回到最新尾部 |
| ↑/↓ | 空闲输入时浏览历史；面板中移动选项 |
| Tab | 只接受当前补全，不提交；可补全 /command 与 @path |
| Enter | 接受当前补全并提交；忙时进入队列 |
| Esc | 关闭浮窗/面板；审批中拒绝 |
| 审批 y / Enter | 批准工具调用 |
| 审批 n / Esc | 拒绝工具调用 |
| 审批 ↑↓ / PageUp/PageDown | 滚动 diff 或详情预览 |

面板通用操作：输入字符即时过滤 key/value，Backspace 删除过滤词，Home/End 跳到首尾，PageUp/PageDown 翻页，Enter 执行动作或展开详情，Esc 关闭。Tool History 展开详情后，Alt+PageUp/PageDown 在搜索命中位置附近滚动；配置面板的 Enter 进入编辑，再按 Enter 写回；Esc 取消编辑。

### 队列干预与接管

任务忙时，普通 `Enter` 将当前输入追加到 FIFO；`Ctrl+Enter` 直接插入队首，均不打断当前模型思考。输入框上方持续显示队首与有界预览。Live Inspector 也显示 pending 行：选中后 `Delete` 可直接移除；`Ctrl+Q` 或 `/queue` 切到完整队列，`Ctrl+I` 可从队列返回 Inspector。删除只作用于尚未执行的队列，不影响当前回合。面板可实时观察队列变化，模型仍继续输出。

实时状态位于顶部活动条与底部状态条：阶段、阶段耗时、工具/思考/回答通道、输入/输出 token、速率、上下文占用、effort 与队列深度均分开显示。长回答与工具输出按终端宽度换行；文件读取默认折叠为一个工具块，`Ctrl+O` 展开当前工具详情，详情保留首尾并折叠中段，`Alt+↑/↓` 切换工具，`Alt+PageUp/PageDown` 查看详情，`/history` 搜索已完成工具记录。`Ctrl+I`/`Alt+I` 或 `/inspect` 检视当前 Answer/Reasoning/Tool 混合块，Enter/Space 展开选中块而不打断模型。`Ctrl+R` 或 `/reasoning` 搜索最近 8 段已完成 reasoning，Enter 展开全文，Alt+PageUp/PageDown 滚动详情。支持释放事件的终端中，`Ctrl+Space` 按住将实时视口置为 `HOLD`，松开回到 `FOLLOW`；不支持释放事件的终端保留原有按键切换。任何情况下都不打断模型。`Ctrl+C` 第一次请求接管并保留输入，2 秒内第二次才退出。

启用 `RIDGE_TUI_SNAPSHOT` 时，诊断 JSON 还会记录当前面板、筛选词、选中项、详情展开/滚动位置、可见行数、`state.live_view`（`hold`/`follow`）、`state.reasoning_expanded`、`state.live_focus`（`answer`/`reasoning`/`tool:<id>`）、`state.activity_kind`、有界 `state.activity_history`、`state.live_blocks` 与 `state.reasoning_history` 数量，便于外部终端/测试工具实时判断用户正在查看什么。

### Windows Terminal 实机验收

发布包使用 inline TUI，不进入备用屏；已提交的 Answer、Reasoning 与工具记录应进入终端原生历史。可按以下顺序验收：

1. 在 Windows Terminal 或 PowerShell 解压发布包并运行 `ridgecode.exe`，输入 `/help` 后按 Enter，确认帮助文本进入历史。
2. 用终端自身的滚轮或历史查看、选择并复制一段 Answer 与一段工具摘要；再用当前终端绑定的原生搜索快捷键搜索其中的英文、CJK 与 emoji 文本。复制内容不应包含 ANSI 转义片段。
3. 任务进行时验证 `Alt+PageUp/PageDown` 能检视 Live Answer/Reasoning，`Alt+End` 能回到最新尾部；展开工具详情后，同组快捷键应优先滚动工具详情。
4. 将窗口缩至约 `48×12`，确认 Answer、输入框、状态提示与退出命令仍可见且无越界；恢复窗口后输入 `/exit` 退出。

不同终端可自定义搜索、复制和鼠标选择快捷键；记录终端名称、版本、窗口尺寸及失败文本，勿将终端原生绑定差异误判为模型或工具协议错误。

需要避开 Windows Terminal/UIA 焦点层、直接验收 ConPTY 字节链路时，可运行仓库内 harness：

~~~powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\windows-pty-e2e.ps1
~~~

脚本使用独立临时 `RIDGE_CONFIG`，以仅供诊断的 `RIDGE_FORCE_TUI=1` 进入 TUI，直接拉起 `target\debug\ridgecode.exe`；向 ConPTY 写入 `/help`、Enter、两次 Ctrl+C，并输出 JSON 验收摘要。它不读取或改写用户配置、Cookie、Chrome 状态。默认模式验证首帧、输入/输出管道与双 Ctrl+C 退出；忙态夹具可再验证真实队列行为：

~~~powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\windows-pty-e2e.ps1 -BusyFixture -KeepDiagnostics
~~~

`-BusyFixture` 使用无网络、延迟 30 秒的 `ScriptedProvider`，自动进入思考态，再发送 Enter 队列与 Ctrl+Enter 队首插入；夹具先写入 Kitty CSI-u `ESC[13;5u`，若 Windows ConPTY/`INPUT_RECORD` 不向 `crossterm` 暴露该序列，则回退写入物理 `LF`，应用层仍把 CR/LF/Ctrl-M 统一归一为 `Enter`。摘要中的 `snapshot_mid_queued=2`、`snapshot_mid_queue[0]="/front"`、`busy_fixture_front_observed=true` 与 `snapshot_has_next_queue=true` 表示内部队首和最终帧 `⏭ next` affordance 均已被应用接收/显示；`snapshot_live_focus`/`snapshot_inspector_live_focus` 会报告当前 Inspector 所选的 `answer`、`reasoning` 或 `tool:<id>`；忙态结束后的 `snapshot_has_reasoning_history=true` 表示已提交思考仍可检索；`busy_fixture_front_transport` 会记录采用 `csi-u` 或 `csi-u→legacy-crlf`，末尾仍验证双 Ctrl+C 接管退出。Windows `crossterm` 键事件未由原始字节管道复现时会明确报告 `status=partial`。

需验证无网络完成态的完整收束链路，可使用 `-CompletionFixture`（与 `-BusyFixture` 互斥）：

~~~powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\windows-pty-e2e.ps1 -CompletionFixture -TimeoutMs 10000 -KeepDiagnostics
~~~

该夹具使用即时 `ScriptedProvider`，自动提交一条任务，要求 Snapshot 同时出现 `busy=false`、`snapshot_has_reasoning_history=true`、`snapshot_has_answer_history=true`，并在 ConPTY 原始滚屏中出现 fixture reasoning 与最终 Answer 文本；随后仍验证两次 Ctrl+C 退出。`completion_evidence_satisfied=true` 才表示思考→回答→历史归档→滚屏完成态均已走通。`-KeepDiagnostics` 还会保留 `pty-output.bin`、`frame.json` 与 `tui-trace.log` 路径，便于复核原始字节与最终帧。

需验证流式块聚焦时追加 `-InspectLive`：夹具用 ConPTY 发送 Alt+I，再发送 Space，要求 `live_inspector_observed=true` 与 `live_inspector_expanded_observed=true`；Ctrl+I 仍是交互终端主快捷键。

需验证 Inspector 内的 pending 干预与面板互切时再追加 `-InspectQueue`（须同时带 `-BusyFixture -InspectLive`）：夹具选中末条 pending、发送 Delete，再用 Ctrl+Q 切到完整队列、Alt+I 返回 Inspector；要求 `live_inspector_queue_removed_observed=true`、`attention_queue_observed=true` 与 `attention_live_return_observed=true`。

需验证物理控制字节时，可追加 `-InspectReasoning` 或 `-InspectHold`（均须带 `-BusyFixture`）：前者发送真实 Ctrl+R 并要求 `reasoning_observed=true`；后者发送真实 Ctrl+Space 两次并要求 `hold_observed=true`、`follow_observed=true`。可与 `-InspectLive -InspectQueue` 组合。

需验证运行中动态重排时追加 `-ResizeProbe`（可与上述 BusyFixture 探针组合）：脚本调用 Windows `ResizePseudoConsole`，在运行中于 `96×24 ↔ 40×12` 间切换，并要求 `resize_observed=true`、Snapshot `rect.width/height` 更新。RidgeCode 内联视口高度有意封顶为 14 行，因此目标高度超过 14 时，`resize_expected_frame_rows=14` 属正常；宽度仍必须精确切换。

若终端宿主无法读出字符画面，可显式开启应用帧快照（默认关闭，不产生文件 I/O）：

~~~powershell
$env:RIDGE_TUI_SNAPSHOT = "$pwd\ridgecode-frame.json"
.\target\debug\ridgecode.exe
~~~

快照为最后一次已绘制的 JSON 帧，`version=2`，含 `format`、`rect`、`render_us`、`state`、`telemetry`、按行排列的 `rows` 文本与压缩后的 `styled_rows` 样式 runs；`state` 提供 `busy`、`waiting`、`phase`、`activity`、`activity_kind`、有界 `activity_history`、`live_view`、`reasoning_expanded`、`live_focus`、`queued`/`queue`、输入/输出/流式 token、`rate`、`effort` 等诊断字段；`telemetry` 提供 `phase_duration_ms`、`token_velocity` 与 `last_render_us`。`styled_rows` 保留每段的 cell 起点、宽度、文本、前景/背景色、修饰符及候选语义角色，便于外部渲染器或 harness 复现“虚拟视网膜”。仅用于诊断/自动验收，路径由用户指定，文件会被下一帧覆盖。

### 斜杠命令

| 命令 | 用法 |
|---|---|
| /help | 显示快捷键与命令提示 |
| /exit、/quit | 退出 TUI |
| /reset | 清空当前上下文并保存空会话 |
| /compact | 压缩历史消息，保留最近上下文 |
| /cost | 查看本会话 token 与任务数 |
| /tools | 查看当前内置/MCP 工具 |
| /activity | 查看最近 5 个 Agent 活动阶段 |
| /reasoning | 检索最近 8 段已完成 reasoning；↑↓ 选择、Enter 展开 |
| /inspect | 检视当前流式 Answer/Reasoning/Tool 与 pending；↑↓ 选择、Enter/Space 展开、Delete 删除待执行项 |
| /queue | 查看 FIFO 队列；↑↓ 选择，Delete/Ctrl+Backspace 删除待执行项 |
| /history | 搜索已完成工具调用；Enter 展开详情 |
| /model | 打开跨 provider 模型选择器 |
| /model <name> | 沿用当前 provider 热切换模型 |
| /effort | 查看当前 reasoning effort 与可选值 |
| /effort <none\|low\|medium\|high\|xhigh\|max> | 持久化并立即切换 reasoning effort |
| /find [query] | 在当前 Live Answer/Reasoning/Tool 块中打开非阻塞搜索 |
| /goal [status|create|start|advance|resume|complete|block|cancel] | 查看或推进持久化长任务目标 |
| /provider、/provider list | 查看 provider 档案 |
| /provider add <name> <kind> <model> <base_url> [key_env] | 新增档案并写回配置 |
| /provider use <name> | 热切换到指定档案 |
| /login | 打开掩码登录面板 |
| /login list | 列出预设与 OAuth 入口 |
| /login --claude、/login --codex | 启动对应 OAuth 登录；端口受限时可执行 `ridgecode login --codex --device-auth` |
| /login <id> <API_KEY> | 校验并接入指定预设 |
| /agent | 查看可派发的只读 sub-agent |
| /mcp | 查看已配置 MCP server |
| /skills | 查看本会话已加载 Skills |
| /commands | 查看自定义命令与 Skill 命令 |
| /init | 分析项目并生成/完善项目级 AGENTS.md |
| /config | 打开配置面板 |
| /config set <key> <value> | 持久化允许的标量配置键 |
| /jailbreak | 查看 cwd 外写入开关状态 |
| /jailbreak on/off | 当前会话开/关 cwd 外写入 |

/model、/provider、/agent、/mcp、/skills 面板均支持过滤和键盘导航。/login 的 API key 输入会掩码显示。/jailbreak 只放宽 cwd 子树限制，危险命令、受保护路径和 read-only 仍硬拦；如需持久化，使用 /config set allow_jailbreak true。

输入行中的 @path 会把存在的文件正文注入本次任务；单文件最多注入 20,000 字符，不存在的路径保持原文：

~~~text
请检查 @src/main.rs 与 @Cargo.toml 的启动流程
~~~

## 配置

默认配置路径是 ~/.ridge/config.json；RIDGE_CONFIG 可覆盖。安装器首次运行会生成配置骨架与 config.example.json。最小真实配置：

~~~json
{
  "provider": "openai",
  "model": "gpt-4o",
  "base_url": "https://api.openai.com/v1",
  "budget_tokens": 0,
  "skip_danger": false,
  "providers": [],
  "mcp": []
}
~~~

密钥推荐只放环境变量：RIDGE_API_KEY 或 provider 档案中的 key_env。顶层 api_key 和档案 api_key 也能工作，但会把明文留在配置文件；/provider add 不会序列化明文 key。

### 顶层字段

| 字段 | 说明 |
|---|---|
| provider | openai（含兼容端点）或 anthropic |
| model | 默认模型名 |
| base_url | 默认 provider 端点 |
| api_key | 顶层内联 key；不推荐 |
| key_env | 顶层 key 所在环境变量名或 auth 槽名 |
| budget_tokens | token 预算；0 或省略表示不限 |
| skills_dir | Skills 目录；默认 ~/.ridge/skills |
| commands_dir | 自定义命令目录；默认 ~/.ridge/commands |
| skip_danger | true 自动批准工具；灾难命令仍硬拦 |
| status_bar | 输入框下状态条模板 |
| allow_jailbreak | 是否允许 cwd 子树外写入；默认关 |
| notify | 每个任务完成时响终端铃 |
| sandbox_cmd | run_shell 的外置 sandbox 包裹模板，{cwd} 替换项目目录 |
| proxy | 出站代理；HTTP/Mixed 例如 http://127.0.0.1:51081，SOCKS5 例如 socks5h://127.0.0.1:51080 |
| providers | 命名 provider 档案数组 |
| mcp | MCP server 数组 |
| hooks | pre_tool、post_tool、session_start、stop hook 数组 |

/config set 允许持久化：provider、model、base_url、budget_tokens、skills_dir、skip_danger、status_bar、allow_jailbreak、proxy。结构化字段（如 mcp、providers、hooks）请直接编辑 JSON。

代理优先级为 `RIDGE_PROXY` > 配置项 `proxy` > 通用 `HTTP_PROXY`/`HTTPS_PROXY`；需临时覆盖配置时用 `RIDGE_PROXY`。

### Provider 档案

~~~json
{
  "providers": [
    {
      "name": "kimi",
      "kind": "openai",
      "model": "kimi-k2",
      "base_url": "https://api.moonshot.cn/v1",
      "key_env": "MOONSHOT_API_KEY"
    }
  ]
}
~~~

档案 key 解析顺序为内联 api_key、key_env 对应的进程环境变量、~/.ridge/auth.json 对应槽。/provider use kimi 会热切换，不重建 agent 图。

### MCP

~~~json
{
  "mcp": [
    {
      "name": "notebooklm",
      "cmd": "notebooklm-mcp",
      "args": []
    },
    {
      "name": "codegraph",
      "cmd": "codegraph",
      "args": ["serve", "--mcp"]
    }
  ]
}
~~~

每个 server 通过 stdio 启动、初始化并列出工具；工具暴露为 <server>__<tool>。单个 server 启动或握手失败只跳过该 server，不阻塞其余工具。兼容旧式单 server 环境变量：RIDGE_MCP_CMD 与可选的 RIDGE_MCP_NAME。

### Hooks

~~~json
{
  "hooks": [
    {
      "event": "post_tool",
      "matcher": "write_file",
      "command": "echo formatted $RIDGE_TOOL_ARG"
    },
    {
      "event": "pre_tool",
      "matcher": "run_shell",
      "command": "exit 0",
      "blocking": true
    }
  ],
  "notify": true
}
~~~

Hook 子进程可读取 RIDGE_TOOL 与 RIDGE_TOOL_ARG。pre_tool 的 blocking hook 返回非 0 会拦截工具。

## 扩展：Skills、Agents 与 Commands

### Skills

把目录放进 ~/.ridge/skills/<name>/SKILL.md，或用 RIDGE_SKILLS_DIR / skills_dir 指定目录。文件需含 name、description frontmatter，正文作为领域规则注入 system prompt。样例见 samples/skills。

### 只读 sub-agent

内置 fastcontext、explorer、reviewer；用户 agent 放在 ~/.ridge/agents/<name>.md，也可用 RIDGE_AGENTS_DIR 指定目录。主 agent 可调用 dispatch_agent，子 agent 独立上下文、只读、只返回结论。/agent 查看当前可用列表。

### Agent route：按任务选择 provider/model

`dispatch_agent` 会按任务难度、规模、类型和角色，在当前有凭据的 `providers[]` 档中确定性选择模型；选择结果会随工具结果显示，例如 `selected=provider::model`、角色、评分与是否 fallback。开启 `RUST_LOG=agent=debug` 可查看同一决策的审计日志。

可在 provider 档声明路由元数据；未声明的能力保持未知，RidgeCode 不从模型名猜能力：

~~~json
{
  "providers": [
    {
      "name": "fast",
      "kind": "openai",
      "model": "small-model",
      "base_url": "https://api.example.com/v1",
      "key_env": "FAST_KEY",
      "route": {
        "context_window": 32768,
        "cost_tier": 1,
        "latency_tier": 1,
        "supports_tools": true,
        "supports_reasoning": false,
        "tags": ["cheap", "fast"]
      }
    }
  ]
}
~~~

`cost_tier`/`latency_tier` 取 `1`（低）到 `3`（高）。大任务会过滤上下文不足的档；需要工具或深度推理而档案明确不支持时会过滤。`dispatch_agent` 可选传 `difficulty`、`size`、`kind`、`provider`、`model` 覆盖推断；偏好不可用时自动回退并明确标注，所有档案均不可用时沿用主 provider。

### 自定义斜杠命令

在 ~/.ridge/commands/<name>.md 写入 prompt 正文，$ARGS 会替换为命令参数；也可由 Skill 暴露同名命令。目录可由 RIDGE_COMMANDS_DIR 或 commands_dir 覆盖。启动后输入 /name args 执行。

## 内置工具与安全边界

内置工具：

- read_file：按路径读取，可给 offset、limit。
- search：按 glob 在目录树内搜索 pattern。
- web_search：联网搜索标题、链接与摘要。
- fetch_url：抓取并清洗网页正文。
- todo_write：维护 [ ]、[~]、[x] 任务清单。
- signal_write：写入或消解跨会话信号。
- run_shell：执行宿主 shell；可选 shell 为 cmd、powershell、pwsh、bash、sh。
- write_file：新建文件的整文件写入。
- edit_file：唯一匹配的 old/new 精准替换。
- apply_edits：多文件编辑汇总成一份 diff，原子应用。
- dispatch_agent：派发只读 sub-agent。

read_file、search、web_search、fetch_url、todo_write、signal_write 与 dispatch_agent 不走用户审批；写文件、编辑和 shell 默认显示 diff/权限门。危险命令、受保护路径始终硬拦。--yolo / skip_danger 只跳过普通审批，不能绕过灾难拦截；--read-only 从工具提供阶段和执行阶段双重拒绝副作用工具及 MCP。

## 运行时文件与环境变量

| 变量 | 作用 | 默认或备注 |
|---|---|---|
| RIDGE_CONFIG | 配置文件路径 | ~/.ridge/config.json |
| RIDGE_API_KEY | 顶层 API key | 优先于配置 key |
| RIDGE_PROVIDER / RIDGE_MODEL / RIDGE_BASE_URL | 覆盖顶层 provider 身份 | env 优先于 config |
| RIDGE_PROXY | 出站代理（http://... 或 socks5h://...） | 也可用 config proxy |
| RIDGE_AUTH | API key 密钥库路径 | ~/.ridge/auth.json |
| RIDGE_OAUTH | OAuth 密钥库路径 | ~/.ridge/oauth.json |
| RIDGE_SESSION | 会话恢复文件 | ~/.ridge/session.json |
| RIDGE_SKILLS_DIR | Skills 目录 | ~/.ridge/skills |
| RIDGE_COMMANDS_DIR | 自定义命令目录 | ~/.ridge/commands |
| RIDGE_AGENTS_DIR | sub-agent 目录 | ~/.ridge/agents |
| RIDGE_MCP_CMD / RIDGE_MCP_NAME | 兼容旧 MCP 单 server | 优先使用 config mcp 数组 |
| RIDGE_SKIP_PERMISSIONS | 默认跳过普通审批 | 1/true 开启 |
| RIDGE_READ_ONLY | 默认只读模式 | 1/true 开启 |
| RIDGE_EXTRACT_SIGNALS | 任务结束后额外抽取跨会话信号 | 默认关闭，避免额外 token |
| RIDGE_HTTP_TIMEOUT | HTTP 超时秒数 | 默认 180 |
| RIDGE_SHELL_TIMEOUT | shell/run_argv 超时秒数 | 默认 180；超时返回失败观测 |
| RIDGE_TOOL_TIMEOUT | MCP 工具调用超时秒数 | 默认 180；超时返回失败观测 |
| RIDGE_GOAL_FILE | goal 状态文件 | 默认 .ridge/goal.json |
| RUST_LOG | tracing 过滤器 | 默认只显示 warn |
| RIDGE_KEYLOG | TUI 按键诊断 | 输出到 ~/.ridge/keylog.txt |

Hook 子进程使用的 RIDGE_TOOL、RIDGE_TOOL_ARG 是运行时注入变量，不是启动配置。

## 本地打包与 Release

### 本机归档

Windows：

~~~powershell
pwsh -File scripts/dist.ps1
~~~

Linux / macOS：

~~~bash
sh scripts/dist.sh
~~~

脚本会执行 cargo build --release --locked --bin ridgecode -p agent，并把二进制、README.md 与平台安装脚本放入归档，再生成 SHA-256 文件。默认输出 dist/；可向脚本传入自定义输出目录。

### GitHub Release

维护者在稳定基线完成全量质量门后创建 v* 标签并推送；CI 会自动创建 GitHub Release、构建五个平台资产、生成 SHA256 并把 README/安装脚本放进归档：

~~~bash
git tag v0.5.16
git push origin main
git push origin v0.5.16
~~~

也可用 GitHub CLI 下载指定版本：

~~~bash
gh release download v0.5.16 --repo MySetsuna/ridge-code --pattern 'ridgecode-*'
~~~

`.github/workflows/release.yml` 会为 Linux x86_64/aarch64、macOS x86_64/aarch64、Windows x86_64 构建并上传归档；发布前先执行以下质量门：

~~~bash
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --locked
~~~

Full quality gate also runs line coverage (minimum 80%) and the local SonarQube
quality gate. The repository targets `http://localhost:9000`; start the local
SonarQube service, create a project token at
`http://localhost:9000/account/security`, then run
`pwsh -File scripts/configure-sonar-secret.ps1` and paste the token into the
hidden prompt. The token is stored only in the current user's environment.
Install the scanner with `npm install --global @sonar/scan@5.0.0`, then run
`pwsh -File scripts/quality-gate.ps1` or `sh scripts/quality-gate.sh` locally.
Missing sonar-scanner or SONAR_TOKEN is a hard failure; the scan cannot be
skipped. The release workflow's local Sonar job requires a self-hosted runner
on the same machine as the local service; GitHub-hosted runners cannot reach
localhost.

## 引擎 API（最小示例）

crates/langgraph 不依赖 LLM，只负责状态、reducer、节点、边与 Pregel/BSP 执行：

~~~rust
use langgraph::{GraphState, StateGraph, END};

#[derive(Clone)]
struct S { n: i64 }
impl GraphState for S {
    type Update = i64;
    fn apply(&mut self, update: i64) { self.n += update; }
}

let mut graph = StateGraph::<S>::new();
graph.add_node("inc", |_state: S| async {
    Ok::<_, std::convert::Infallible>(1)
});
graph.set_entry("inc");
graph.add_edge("inc", END);
let output = graph.compile()?.invoke(S { n: 0 }).await?;
assert_eq!(output.n, 1);
~~~

四个基本元素是 GraphState（显式 reducer）、异步 Node、静态/条件 Edge，以及 invoke / invoke_with Runtime。invoke_with 可接 checkpointer、streaming 与 max_supersteps。

## 架构与安全说明

- crates/langgraph：纯图引擎；不依赖 LLM。
- crates/provider：OpenAI 兼容、Anthropic、流式 SSE、OAuth 与 HTTP 接缝。
- crates/tools：文件、搜索、跨平台 shell 与危险命令拦截。
- crates/mcp：JSON-RPC MCP 客户端与 stdio 传输。
- crates/agent：ReAct 图、maker/checker、安全门、Skills、Agents、TUI 与 ridgecode 二进制。

安全边界：默认写入限于 cwd 子树；危险命令和受保护路径硬拦；普通副作用需审批；sandbox_cmd 只是用户配置的外置隔离入口，是否真正隔离取决于 Docker、WSL 或用户提供的 wrapper。当前没有重量级 OS sandbox。

## 相关文档

- [samples/README.md](samples/README.md)：Skills、provider、MCP 样例。
- [docs/DIRECTION.md](docs/DIRECTION.md)：产品方向与边界。
- [docs/WORKFLOW.md](docs/WORKFLOW.md)：迭代与验证工作流。
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)：架构细节。
- [docs/REPORT-langgraph-rust.md](docs/REPORT-langgraph-rust.md)：引擎设计来源。
