# RidgeCode

RidgeCode 是一个模块化、跨领域可扩展的通用 agent 框架，发布为单一命令 ridgecode。它既能做代码任务，也能做调研、摘要、翻译、分诊等工作；新增能力主要靠 SKILL.md、MCP 配置或自定义命令，不必改 Rust 内核。

底层是纯 Rust 的 LangGraph 风格有状态图引擎，外层接 provider、工具、MCP、Skills 与 TUI。TUI 重点呈现实际收到的 Answer 与 reasoning_content：Reasoning 单独显示，工具默认收起，长回答可在终端原生 scrollback 中保留。

## 先跑起来

### 下载 Release

Release 归档包含：平台二进制、本文档 README.md，以及对应平台安装脚本。归档旁附 .sha256 校验文件。

~~~bash
# Linux / macOS：安装最新 Release 到 ~/.local/bin
curl -fsSL https://raw.githubusercontent.com/MySetsuna/ridge-code/main/scripts/install.sh | sh
~~~

~~~powershell
# Windows：安装到 %LOCALAPPDATA%/Programs/ridgecode，并写入用户 PATH
irm https://raw.githubusercontent.com/MySetsuna/ridge-code/main/scripts/install.ps1 | iex
~~~

手动下载地址：[GitHub Releases](https://github.com/MySetsuna/ridge-code/releases)。按平台选择 ridgecode-<target>.tar.gz 或 ridgecode-<target>.zip，解压后把二进制放进 PATH 即可。

安装器参数：

- Unix：--version <tag>、--local <binary>、--dir <dir>、--help。
- Windows：-Version <tag>、-Local <path>、-Dir <dir>。
- 当前平台本地安装不联网、不需要 Cargo：scripts/install.sh --local target/release/ridgecode 或 scripts/install.ps1 -Local target/release/ridgecode.exe。

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
~~~

程序打开授权流程，用户在浏览器完成授权。凭据独立保存到 ~/.ridge/oauth.json；不把 access token 打进日志、配置或任务内容。OAuth 端点、账号权限和 provider wire 仍以实际账号与服务端结果为准。

## 交互式 TUI

直接运行 ridgecode 即进入 inline TUI。输出会落入终端历史，可用终端自己的滚轮、选择和复制；活动区域只占终端底部固定空间。

### Answer、Reasoning 与工具

- Answer 展示模型实际回答；收到的 reasoning_content 单独作为 Reasoning 展示，不生成或伪造隐藏思考。
- Answer 支持有界行级 Markdown 展示：标题、粗体、行内 code、代码围栏与 ANSI 16 色语义角色。
- fenced code 的可见行按有限词法规则区分关键字、类型、字符串、数字、字面量和注释；未知文本保持普通 Muted 色，不猜测跨行语法。
- 工具调用默认显示摘要；Ctrl+O 展开当前工具详情。Alt+↑/↓ 选择旧工具并锁定焦点；详情展开且可滚动时，Alt+PageUp/PageDown 查看旧/新详情位置。
- Ctrl+R 展开或收起实际 Reasoning；Answer 到达时默认回到 Answer 优先视图。
- 长任务中可继续编辑输入；任务忙时按 Enter 会排队，当前任务结束后继续执行。

### 输入与快捷键

| 按键 | 作用 |
|---|---|
| Enter | 空闲时提交；忙时排队 |
| Ctrl+J | 插入换行 |
| Shift+Enter | 支持 CSI-u 的终端插入换行；Windows Terminal 可用 Alt+Enter 或 Ctrl+J |
| Ctrl+C | 中断当前任务；不退出整个会话 |
| Ctrl+R | 切换 Reasoning 视图 |
| Ctrl+O | 切换工具详情；无 live 工具时打开 Tool History |
| Alt+↑/↓ | 选择 live 工具焦点 |
| Alt+PageUp/PageDown | 滚动当前展开工具详情 |
| ↑/↓ | 空闲输入时浏览历史；面板中移动选项 |
| Tab | 打开或选择补全；可补全 /command 与 @path |
| Esc | 关闭浮窗/面板；审批中拒绝 |
| 审批 y / Enter | 批准工具调用 |
| 审批 n / Esc | 拒绝工具调用 |
| 审批 ↑↓ / PageUp/PageDown | 滚动 diff 或详情预览 |

面板通用操作：输入字符即时过滤 key/value，Backspace 删除过滤词，Home/End 跳到首尾，PageUp/PageDown 翻页，Enter 执行动作或展开详情，Esc 关闭。配置面板的 Enter 进入编辑，再按 Enter 写回；Esc 取消编辑。

### 斜杠命令

| 命令 | 用法 |
|---|---|
| /help | 显示快捷键与命令提示 |
| /exit、/quit | 退出 TUI |
| /reset | 清空当前上下文并保存空会话 |
| /compact | 压缩历史消息，保留最近上下文 |
| /cost | 查看本会话 token 与任务数 |
| /tools | 查看当前内置/MCP 工具 |
| /history | 搜索已完成工具调用；Enter 展开详情 |
| /model | 打开跨 provider 模型选择器 |
| /model <name> | 沿用当前 provider 热切换模型 |
| /provider、/provider list | 查看 provider 档案 |
| /provider add <name> <kind> <model> <base_url> [key_env] | 新增档案并写回配置 |
| /provider use <name> | 热切换到指定档案 |
| /login | 打开掩码登录面板 |
| /login list | 列出预设与 OAuth 入口 |
| /login --claude、/login --codex | 启动对应 OAuth 登录 |
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
| proxy | 出站 HTTP 代理，例如 http://127.0.0.1:7890 |
| providers | 命名 provider 档案数组 |
| mcp | MCP server 数组 |
| hooks | pre_tool、post_tool、session_start、stop hook 数组 |

/config set 允许持久化：provider、model、base_url、budget_tokens、skills_dir、skip_danger、status_bar、allow_jailbreak、proxy。结构化字段（如 mcp、providers、hooks）请直接编辑 JSON。

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
| RIDGE_PROXY | 出站代理 | 也可用 config proxy |
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

维护者在稳定基线完成全量质量门后创建 v* 标签并推送：

~~~bash
git tag v<version>
git push origin v<version>
~~~

.github/workflows/release.yml 会为 Linux x86_64/aarch64、macOS x86_64/aarch64、Windows x86_64 构建并上传归档。CI 同时验证 fmt、clippy、workspace build 与 workspace test。发布前至少执行：

~~~bash
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --locked
~~~

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
