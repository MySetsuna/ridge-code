# M4 · rc-mcp(MCP 客户端)— 设计文档

> 状态:待实现(2026-07-05)。
> 配套:方向依据见 `PLAN.md §4`(rmcp 选型)、`§10`(M4 里程碑);项目现状见 `CLAUDE.md`。

---

## 1. 背景与目标

M0–M3 已经让「强/弱混合编排 + 客观验证 + eval 闭环」跑通,工具面仍只有 4 个内置工具(read_file / write_file / list_dir / run_shell)。M4 的第一件事(`PLAN §10`)是 **`rc-mcp`:用官方 `rmcp` SDK 接外部 MCP 服务器**,把生态里现成的工具/skills(git、filesystem、fetch、数据库、私有工具…)接进来给编排的 Worker 用——不必每种能力都手写内置工具。

本设计交付一个**最小可用闭环**:从配置声明若干 MCP 服务器(子进程 stdio)→ 启动时连上、列出它们的工具 → 归一化成内部 `ToolSpec` → 合并进 Worker 的工具集 → 模型调用时按名路由回对应服务器执行 → 结果回灌工具循环。

## 2. 范围

**做:**
- 新 crate `rc-mcp`:MCP **客户端**(基于 `rmcp` 2.x,`transport-child-process` 子进程 stdio)。
- 配置驱动:`~/.ridge/config.toml` 新增 `[[mcp]]` 数组,每项 = `name` + `command` + `args` + 可选 `env`。
- `McpHub`:连接多个服务器、`list_all_tools` 拉工具、归一化成 `rc_types::ToolSpec`、按名路由 `call_tool`、优雅关闭。
- **工具名命名空间**:`<server>__<tool>`,避免多服务器/与内置工具重名冲突;路由用哈希表(不靠拆名,健壮)。
- 接入 `rc-core`:`Orchestrator::with_mcp(hub)` 把 MCP 工具并入 Worker/修复/基线的工具集;`run_agent` 分派时先查 MCP、再落内置。
- 接入 `rc-cli`:读 `[[mcp]]` → 建 hub → 注入编排器 → 运行后 `shutdown`。
- **健壮性**:单个 MCP 服务器连不上/列不出工具 → 记警告并跳过,**不拖垮整轮**(缺一个工具不该让任务失败)。
- 纯函数(命名空间/工具转换/结果渲染/路由)带离线单测,零联网零子进程。

**不做(YAGNI,留待后续):**
- MCP 的 resources / prompts / sampling(先只接 tools——编码 Worker 只需要工具)。
- 非 stdio 传输(SSE / Streamable HTTP):v1 只做子进程 stdio,最常见。
- 把 MCP 工具喂给 Reviewer(评审是只读评估,仍只用内置 read_file/list_dir,避免副作用工具)。
- 工具级权限/风险门控(内置 run_shell 已是同等信任面;留待后续统一风险层)。
- 图片/二进制内容块的完整承载(先取文本内容;非文本注明省略)。

## 3. 关键决策

| # | 决策 | 取舍理由 |
|---|---|---|
| 1 | 用官方 `rmcp` 2.x(非自研 JSON-RPC) | `PLAN §4` 已核实:官方、成熟、client + 子进程 stdio 齐全 |
| 2 | 只做 **tools**,先不做 resources/prompts | 编码 Worker 只消费工具;最小闭环够用 |
| 3 | 传输只做 **子进程 stdio** | 覆盖绝大多数 MCP 服务器;SSE/HTTP 留后续 |
| 4 | 工具名 `<server>__<tool>` + **哈希表路由** | 防重名;路由不靠拆名(工具名可能含 `__`),用 map 权威 |
| 5 | 单服务器失败**降级跳过**、不中断 | 缺一个外部工具不该让编码任务整体失败 |
| 6 | MCP 工具只进 Worker/修复/基线,**不进 Reviewer** | 评审只读、避免触发副作用工具 |
| 7 | `McpServerConfig` 定义在 `rc-mcp`(serde) | 配置类型随实现走,rc-cli 直接反序列化 + 传入 |
| 8 | 路由/命名/渲染抽成**纯函数**,离线单测 | 对应 M3 的 StubProvider 思路:bug 在归一化里,不烧网络就能测 |
| 9 | rmcp 只启 `client`+`transport-child-process`(`default-features=false`) | 客户端不需要 server/macros;精简依赖与编译量 |

## 4. 架构与数据流

`rc-mcp` 位于依赖链 `{rc-providers,rc-tools,rc-verify} → rc-mcp → rc-core`(`PLAN §5`)。

```
启动(rc-cli):读 config 的 [[mcp]] 列表
        │
        ▼   McpHub::connect(configs):对每个服务器
   ┌─────────────────────────────────────────────┐
   │ 1. TokioChildProcess 起子进程(command+args+env)│
   │ 2. ().serve(transport) 初始化 MCP 会话         │  ← 失败:warn + 跳过该服务器
   │ 3. list_all_tools() 拉工具清单                 │
   │ 4. 每个工具:namespaced=<server>__<tool>        │
   │      · 归一化成 ToolSpec(name/description/schema)│
   │      · 路由表 route[namespaced] = (server_idx, 原名)│
   └─────────────────────────────────────────────┘
        │
        ▼
   Orchestrator::with_mcp(hub):self.tools ∪ hub.tool_specs()
        │
        ▼   run_agent 工具循环里,模型发起一次 tool_call:
   dispatch:hub.has_tool(name)? ── 是 ─▶ hub.call(call):
        │                                 route[name] → conn.call_tool(原名, args)
        │                                 → CallToolResult → 取文本 → 回灌
        └── 否 ─▶ rc_tools::dispatch(call)(内置 read/write/list/shell)
        │
        ▼
   运行结束:orch.shutdown() → hub.shutdown() → 逐个 client.cancel()(关子进程)
```

**关键不变量:**
- 上层(`rc-core`)只依赖 `LlmProvider` trait 与 `McpHub`,**不直接依赖 `rmcp`**——rmcp 的 wire 类型不外泄,归一化成 `rc-types::{ToolSpec, ToolCall}`(同 provider 边界原则,`HANDOFF §5`)。
- 路由用哈希表,不解析工具名;命名空间只用于生成暴露名 + 保证唯一。
- MCP 工具与内置工具在模型眼里同构(都是 `ToolSpec`),Worker 无感知一个工具来自内置还是外部。

## 5. 数据结构与各 crate 改动

### 新 crate `rc-mcp`

```rust
/// 一个 MCP 服务器的声明(来自 config 的 [[mcp]])。
#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)] pub args: Vec<String>,
    #[serde(default)] pub env: HashMap<String, String>,
}

/// 已连接的一批 MCP 服务器 + 归一化后的工具索引。
pub struct McpHub {
    conns: Vec<ServerConn>,   // 每个持有 rmcp RunningService<RoleClient,()>
    index: ToolIndex,         // 暴露名 → (server_idx, 原名) + 归一化 ToolSpec 列表
}

impl McpHub {
    pub async fn connect(configs: Vec<McpServerConfig>) -> Self;   // 单点失败跳过,永不 panic
    pub fn tool_specs(&self) -> &[ToolSpec];
    pub fn has_tool(&self, name: &str) -> bool;
    pub async fn call(&self, call: &ToolCall) -> Result<String>;
    pub async fn shutdown(self);                                    // 逐个 cancel
}
```

**纯函数(离线可测,不碰 rmcp 运行时):**
```rust
fn namespaced_name(server: &str, tool: &str) -> String;            // "git" + "status" → "git__status"
fn render_call_result(r: &rmcp::model::CallToolResult) -> String;  // 取文本块;is_error → "ERROR: .."

/// 路由/命名逻辑独立成结构,单测不需要真连接。
struct ToolIndex {
    specs: Vec<ToolSpec>,
    route: HashMap<String, (usize, String)>,
}
impl ToolIndex {
    fn add_tool(&mut self, server_idx, server_name, original, description, schema: Value);
    fn route(&self, exposed_name) -> Option<(usize, &str)>;
}
```

### `rc-core`:接入 MCP(不破坏现有签名)

- `Orchestrator` 加字段 `mcp: Option<McpHub>`(`new` 默认 `None`,现有调用方零改动)。
- 新增 builder:`pub fn with_mcp(mut self, hub: McpHub) -> Self`——把 `hub.tool_specs()` 追加进 `self.tools`(**不进** `read_tools`,评审隔离)。
- `run_agent` 加参数 `mcp: Option<&McpHub>`;分派处 `dispatch_tool(mcp, call)`:先 `hub.has_tool` 走 MCP,否则落 `rc_tools::dispatch`。所有调用点传 `self.mcp.as_ref()`(评审传 `None`)。
- 新增 `pub async fn shutdown(self)`:有 hub 则 `hub.shutdown().await`。

### `rc-cli`:装配

- `Config` 加 `#[serde(default)] mcp: Vec<McpServerConfig>`。
- main:非空则 `McpHub::connect(...)` → `info!` 打连上多少工具 → `orch.with_mcp(hub)`;运行报告打印后 `orch.shutdown().await`。

### 依赖

- 根 `Cargo.toml`:`rmcp = { version = "2", default-features = false, features = ["client", "transport-child-process"] }`。
- `rc-mcp`:rmcp + rc-types + tokio + anyhow + tracing + serde + serde_json。
- `rc-core`:加 `rc-mcp`。`rc-cli`:加 `rc-mcp`。

## 6. 命名空间与路由(防冲突的关键)

- 暴露给模型的工具名 = `namespaced_name(server, tool)` = `<server>__<tool>`。多个服务器即便都有 `search`,也变 `git__search`/`fs__search`,不撞;也不会撞内置(内置无 `__` 前缀语义)。
- **路由不靠拆名**:`route: HashMap<暴露名, (server_idx, 原名)>` 在 connect 时构建。调用时按暴露名查表拿到「哪个连接 + 原始工具名」,再 `conn.call_tool(原名, args)`。工具名里带 `__` 也不影响(不解析)。
- 若两个服务器配了相同 `name`(用户配置错误)→ 后者工具会覆盖同名暴露键;connect 时 `warn` 提示重名。

## 7. 结果渲染、错误处理、测试

**结果渲染(`render_call_result`):** 取 `CallToolResult.content` 里的文本块拼接;为空则退回 `structured_content` 的 JSON;仍空则「(工具无文本输出)」。`is_error == Some(true)` 前缀 `ERROR:`(与内置工具 `dispatch` 的错误文本风格一致,让模型能自我纠正)。

**错误处理:**
- 连接期:单服务器起进程/初始化/列工具失败 → `warn` + 跳过,`connect` 仍返回(其余可用)。全失败则 hub 工具集为空,编排照常只用内置工具。
- 调用期:`call` 内部 rmcp 报错(超时/协议错)→ 转成 `Err`,由 `dispatch_tool` 兜成 `"ERROR: .."` 文本回灌模型(不向上抛断整轮),与内置工具语义一致。

**测试策略(全离线,不起子进程、不联网):**
- `namespaced_name`:拼接正确。
- `ToolIndex`:两个服务器各加若干工具 → `specs` 命名带前缀、`route` 能定位到正确 (server_idx, 原名);同名工具跨服务器不冲突。
- `render_call_result`:文本块 / 多块拼接 / `is_error` 前缀 / 空内容退回——用 `serde_json::from_value` 造 `CallToolResult`(公有 `Deserialize`,`#[non_exhaustive]` 无法字面量构造)。
- rc-core:现有单测保持绿(`with_mcp` 不改默认路径;`run_agent` 新参数在无 MCP 时传 `None`)。

> 真连接(起真实 MCP 服务器、真 list/call)不进 CI——依赖外部命令(uvx/npx),且 Windows 下 bare `npx` 有 ENOENT 坑(见工程记忆)。留作手动冒烟:配一个 `[[mcp]]` 指向真实服务器跑一次。

## 8. 实施顺序(串行,接缝集中在 rc-core)

1. 根 `Cargo.toml` 加 rmcp 依赖;`rc-mcp/Cargo.toml` 补依赖。
2. `rc-mcp`:`McpServerConfig` + `namespaced_name` + `ToolIndex` + `render_call_result`(纯函数)+ 单测。
3. `rc-mcp`:`ServerConn` + `McpHub::{connect, tool_specs, has_tool, call, shutdown}`(rmcp 运行时)。
4. `rc-core`:`mcp` 字段 + `with_mcp` + `run_agent` 分派 + `shutdown`;跑现有单测。
5. `rc-cli`:config `[[mcp]]` + 装配 + shutdown。
6. `config.example.toml` + `CLAUDE.md`/`README.md` 文档;`cargo build`/`cargo test` 全绿。

## 9. 本设计的验收标准(DoD)

- `cargo build` 全 workspace 通过;`cargo test` 全绿(含 rc-mcp 新增离线单测)。
- 配置里声明一个 MCP 服务器时,`ridge-code` 启动日志显示「MCP 已连接 N 个工具」,且这些工具能被 Worker 调用(手动冒烟)。
- 未配置 `[[mcp]]` 时,行为与 M3 完全一致(零回归)。
- 单个 MCP 服务器不可用时,`ridge-code` 仍能只用内置工具完成任务(降级不崩)。

## 10. 后续(M4 剩余,不在本设计内)

- `ratatui` 实时 DAG/进度视图(`PLAN §4/§10`)。
- **`cargo-dist` 单二进制跨平台分发**(`PLAN §10` M4 收尾;本 goal 的第二阶段)。
- MCP resources/prompts、非 stdio 传输、工具级风险门控。
