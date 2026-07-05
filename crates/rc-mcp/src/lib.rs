//! rc-mcp — MCP 客户端(基于官方 rmcp),接外部工具/skills。M4,详见 PLAN.md §4。
//!
//! 只做「子进程 stdio + tools」最小闭环:配置声明 MCP 服务器 → 连接 → 列举并归一化工具 →
//! `<server>__<tool>` 命名空间 + 哈希表路由 → 供 rc-core 的 Worker 调用。
//! rmcp 的 wire 类型不外泄(同 provider 边界原则,见 HANDOFF.md §5)。

use serde::Deserialize;
use std::collections::HashMap;

mod hub;
mod index;
mod result;

pub use hub::McpHub;

/// 一个 MCP 服务器的声明(来自 ~/.ridge/config.toml 的 `[[mcp]]`)。
#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    /// 服务器名 —— 用作工具命名空间前缀,应唯一。
    pub name: String,
    /// 启动命令(可执行文件名或路径)。
    pub command: String,
    /// 命令参数。
    #[serde(default)]
    pub args: Vec<String>,
    /// 附加环境变量。
    #[serde(default)]
    pub env: HashMap<String, String>,
}
