//! `McpHub`:连接多个 MCP 服务器(子进程 stdio)、拉取并归一化工具、按名路由调用、优雅关闭。
//! rmcp 的 wire 类型只在本文件内使用,对上层归一化成 `rc_types::{ToolSpec, ToolCall}`。

use crate::index::ToolIndex;
use crate::result::render_call_result;
use crate::McpServerConfig;
use anyhow::{Context, Result};
use rc_types::{ToolCall, ToolSpec};
use rmcp::model::{CallToolRequestParams, Tool};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::ServiceExt;
use serde_json::Value;
use tokio::process::Command;

type ClientService = RunningService<RoleClient, ()>;

/// 一个已连接的 MCP 服务器会话。
struct ServerConn {
    name: String,
    peer: ClientService,
}

/// 已连接的一批 MCP 服务器 + 归一化后的工具索引。
pub struct McpHub {
    conns: Vec<ServerConn>,
    index: ToolIndex,
}

impl McpHub {
    /// 连接一批 MCP 服务器。单个失败仅告警跳过、不 panic;全失败则工具集为空(编排照常只用内置工具)。
    pub async fn connect(configs: Vec<McpServerConfig>) -> Self {
        let mut conns: Vec<ServerConn> = Vec::new();
        let mut index = ToolIndex::default();
        for cfg in configs {
            match connect_one(&cfg).await {
                Ok((peer, tools)) => {
                    let server_idx = conns.len();
                    let count = tools.len();
                    for tool in &tools {
                        let desc = tool.description.as_deref().unwrap_or("");
                        let schema = Value::Object((*tool.input_schema).clone());
                        index.add_tool(server_idx, &cfg.name, &tool.name, desc, schema);
                    }
                    tracing::info!(server = %cfg.name, tools = count, "MCP 服务器已连接");
                    conns.push(ServerConn {
                        name: cfg.name,
                        peer,
                    });
                }
                Err(e) => {
                    tracing::warn!(server = %cfg.name, error = %format!("{e:#}"), "MCP 服务器连接失败,跳过");
                }
            }
        }
        Self { conns, index }
    }

    /// 归一化后的工具规格(供上层合并进模型工具集)。
    pub fn tool_specs(&self) -> &[ToolSpec] {
        self.index.specs()
    }

    /// 该暴露名是否是本 hub 管理的 MCP 工具。
    pub fn has_tool(&self, name: &str) -> bool {
        self.index.has(name)
    }

    /// 是否没有任何已连接的服务器。
    pub fn is_empty(&self) -> bool {
        self.conns.is_empty()
    }

    /// 执行一次 MCP 工具调用:按暴露名路由到对应服务器 + 原始工具名,返回渲染后的文本。
    pub async fn call(&self, call: &ToolCall) -> Result<String> {
        let (server_idx, original) = self
            .index
            .route(&call.name)
            .with_context(|| format!("未知 MCP 工具: {}", call.name))?;
        let conn = self
            .conns
            .get(server_idx)
            .context("路由到的 MCP 服务器索引越界")?;

        let mut params = CallToolRequestParams::new(original.to_string());
        // 参数是 JSON 字符串;非 object(如空/null)则不带 arguments。
        if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(&call.arguments) {
            params = params.with_arguments(obj);
        }

        let result = conn
            .peer
            .call_tool(params)
            .await
            .with_context(|| format!("调用 MCP 工具 {original}(服务器 {})失败", conn.name))?;
        Ok(render_call_result(&result))
    }

    /// 优雅关闭:逐个 cancel(关闭子进程会话)。
    pub async fn shutdown(self) {
        for conn in self.conns {
            let name = conn.name;
            if let Err(e) = conn.peer.cancel().await {
                tracing::warn!(server = %name, error = %format!("{e:#}"), "关闭 MCP 服务器出错");
            }
        }
    }
}

/// 起一个子进程 MCP 服务器、初始化会话、列举其全部工具。
async fn connect_one(cfg: &McpServerConfig) -> Result<(ClientService, Vec<Tool>)> {
    let args = cfg.args.clone();
    let env = cfg.env.clone();
    let transport = TokioChildProcess::new(Command::new(&cfg.command).configure(|cmd| {
        cmd.args(&args);
        for (k, v) in &env {
            cmd.env(k, v);
        }
    }))
    .with_context(|| format!("启动 MCP 子进程失败: {}", cfg.command))?;

    let peer = ()
        .serve(transport)
        .await
        .with_context(|| format!("初始化 MCP 会话失败: {}", cfg.command))?;

    let tools = peer
        .list_all_tools()
        .await
        .with_context(|| format!("列举 MCP 工具失败: {}", cfg.command))?;

    Ok((peer, tools))
}
