# RidgeCode

**模块化、跨领域可扩展的通用 agent 框架**(单二进制 `ridgecode`)—— 既能像 Claude Code 写代码,也能做编程以外的事(调研 / 摘要 / 翻译 / 分诊 …)。核心赌注:**加新能力 = 加一个 `SKILL.md` 或一段 MCP 配置,而不是改 Rust 源码。**

底座是手搓的 **Rust 版 LangGraph** 引擎(有状态图状态机),agent 分层跑在其上。用 NotebookLM 驱动持续迭代(见 [`docs/WORKFLOW.md`](docs/WORKFLOW.md)),关键决策过对抗评审。方向见 [`docs/DIRECTION.md`](docs/DIRECTION.md)。

## 能力(对标 Claude Code —— 核心用户体验已全套达成)

- **交互式 TUI**(ratatui):彩色实时输出 + 等待 spinner、答案 **token 逐字流式**(SSE)、灰色**状态行**(provider·model · 会话 tokens · 目录)、斜杠命令 `/help /cost /model /provider /agent /config /reset /compact /tools /exit`;非 TTY(管道/CI/重定向)回落 **headless** —— 逐行 stdin 当任务串行跑,无斜杠命令。
- **多 provider**:`/provider add|list|use` **交互式加/列/热切换** provider 档案;`/model <name>` 热切换模型(均不重建图);`/cost` 看会话累计 tokens。**密钥不落 config**,档案只存要读的 env 变量名。
- **驾驭工程**:精准 `edit_file`(唯一匹配替换)、**多文件原子批量编辑** `apply_edits`(汇总一份 diff 一次确认)、可移植 `search`、分段 `read_file`。
- **安全人机**:副作用工具**权限门 + `-/+` diff 预览**、危险命令硬拦截、`--yolo` **skip-danger** 模式。
- **web 研究闭环**:`web_search`(**探测 GFW 自动换引擎**、无 key 多引擎 fallback)→ `fetch_url`(抓正文)→ 据原文作答。
- **会话韧性**:`@file` 上下文引用、`--resume` **kill-9 崩溃恢复**、**Ctrl-C 中断**当前任务、`todo_write` **任务清单** `[x]/[~]/[ ]` 实时渲染。
- **插件式扩展**:`~/.ridge/config.json`(provider/model/预算/多 `mcp`/skills;env 覆盖;**密钥只走 env**;TUI 内 `/config set` 可持久化)、多 MCP 并接(实测零改源码接入 [AnySearch](docs/web-search-and-anysearch.md))、`SKILL.md` 声明式技能。
- **子智能体 & 自扩展**:内置只读 sub-agent(fastcontext/explorer/reviewer),`dispatch_agent` 主 agent **自动派** / `/agent` **手动派**(独立上下文、只回结论、省 token、恒**只读**);FastContext 走 config 的廉价档省钱;内置 `agent-creator`/`skill-creator` 教主 agent **自建 agent/skill**;cwd 的 `CLAUDE.md`/`AGENTS.md` 自动注入 system prompt。
- **可信闭环**:`maker≠checker`(确定性验证 + 可选独立模型 reviewer)、多层停机护栏、`trace.json` 审计、`tracing` 全链路。

`cargo test --workspace` = **81 全绿**,clippy `-D warnings` / fmt 干净。

## 两层架构

- **`crates/langgraph`** —— 纯图引擎(零 LLM):强类型 `StateGraph` + Pregel 超步(BSP)+ checkpoint(内存 / `FileCheckpointer` 落盘 / `resume`)+ `StreamEvent`。
- **`crates/provider`** —— `LlmProvider`(Anthropic/OpenAI 归一化 + **流式 `complete_streaming` SSE**)+ `HttpClient` 传输接缝 + `web_search`/`fetch_url`(`WebFetch` 接缝,可离线测)。
- **`crates/tools`** —— std-only 真实文件读写 / `edit_file` / `apply_edits` / `search` / 跨平台 shell / 危险命令拦截。
- **`crates/mcp`** —— MCP 客户端(JSON-RPC:initialize/tools/list/tools/call + `server__tool` 命名空间)+ 可插拔传输。
- **`crates/agent`** —— ReAct 图(reason → act → verify)+ 全部上面的装配;二进制 `ridgecode`。

## 安装(独立二进制,无需 cargo)

`ridgecode` 是**单个静态二进制**(纯 Rust TLS,零系统依赖),下载即用。全平台一键装到 PATH:

```bash
# Linux / macOS —— 装最新 Release 到 ~/.local/bin
curl -fsSL https://raw.githubusercontent.com/MySetsuna/ridge-code/main/scripts/install.sh | sh
```
```powershell
# Windows —— 装到 %LOCALAPPDATA%\Programs\ridgecode 并写入用户 PATH(新终端生效)
irm https://raw.githubusercontent.com/MySetsuna/ridge-code/main/scripts/install.ps1 | iex
```

- 手动:去 [Releases](https://github.com/MySetsuna/ridge-code/releases) 下对应平台归档(`ridgecode-<target>.tar.gz`/`.zip`,附 `.sha256`),解压把 `ridgecode` 放进任一 PATH 目录即可。
- 已在本机构建:`./scripts/install.sh --local target/release/ridgecode`(或 PS `install.ps1 -Local target\release\ridgecode.exe`)—— 不联网、不碰 cargo。
- 自己出跨平台产物:打标签 `git tag v0.2.1 && git push origin v0.2.1` 触发 [`release.yml`](.github/workflows/release.yml),为 5 个目标(linux x86_64/aarch64、macOS x86_64/aarch64、windows x86_64)构建归档并传上 Release。本机单平台包:`scripts/dist.ps1` / `scripts/dist.sh` → `dist/`。

## 快速开始

```bash
cargo build --workspace
cargo run -p agent --bin ridgecode -- --help   # 用法
cargo test --workspace                          # 81 单测,全绿

# 接真实 LLM(OpenAI 兼容端点示例;密钥只走 env):
export RIDGE_API_KEY=sk-...        # 或用 ~/.ridge/config.json(见 samples/config.json)
ridgecode                          # 交互式 TUI(管道/非 TTY 则 headless 逐行任务)
ridgecode "修复编译错误" --cwd /path/to/proj   # 一次性任务
ridgecode --resume                 # 恢复上次会话(崩溃/关掉重开)
```

**加能力不改源码**:把 [`samples/skills/`](samples/) 里的 `SKILL.md` 拷进 `~/.ridge/skills/`(含 researcher/rust-fixer/triage/summarize/translate),或在 `config.json` 的 `mcp` 加一段。

## 引擎用法

```rust
use langgraph::{GraphState, StateGraph, END};

#[derive(Clone)]
struct S { n: i64 }
impl GraphState for S {
    type Update = i64;
    fn apply(&mut self, u: i64) { self.n += u; } // reducer:累加而非覆盖
}

let mut g = StateGraph::<S>::new();
g.add_node("inc", |_s: S| async { Ok::<_, std::convert::Infallible>(1) });
g.set_entry("inc");
g.add_edge("inc", END);
let out = g.compile()?.invoke(S { n: 0 }).await?; // out.n == 1
```

四要素:**State**(`GraphState` + reducer)、**Node**(异步函数)、**Edge**(`add_edge` / `add_conditional_edge`)、**Runtime**(`invoke` / `invoke_with`,后者可挂 checkpointer 与 streaming)。

## 已知限制(需环境/决策,标为后续)

- **重量级沙箱**(Docker/gVisor;gVisor 仅 Linux)—— 现靠危险命令拦截 + 权限门 + diff 确认;真 OS 隔离待技术选型。
- **官方 `rmcp` 替换自写 stdio** —— 自写传输已连真实 server(notebooklm-mcp / AnySearch),rmcp 为可选鲁棒性升级。
- 子智能体**并行**编排(现有 `dispatch_agent`/sub-agent 为**串行只读**)、`bincode` checkpoint。

来源与设计理由见 [`docs/REPORT-langgraph-rust.md`](docs/REPORT-langgraph-rust.md);迭代归档见 [`docs/iterations/`](docs/iterations/) 与 [`docs/LOG.md`](docs/LOG.md)。

```bash
cargo run -p eval --bin ridgecode-eval       # 离线 eval demo:每 case PASS/FAIL + 成功率
RUST_LOG=langgraph=debug,agent=debug ridgecode …  # 全链路结构化日志
```
