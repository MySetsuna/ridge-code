//! # mcp —— 最小 MCP 客户端(M2)
//!
//! MCP 本质是 **JSON-RPC 2.0**。这里只做客户端最核心的三件事:握手 `initialize`、
//! 发现工具 `tools/list`、调用工具 `tools/call`,并给工具名加 `<server>__<tool>` 命名空间
//! (防多服务器/与内置工具重名)。
//!
//! **传输与协议解耦**(同 provider 的 HTTP 分层):协议逻辑([`McpClient`])是纯的、离线可测;
//! 真实 stdio 子进程传输([`StdioTransport`])是薄薄一层,靠 [`McpTransport`] trait 插进来。
//!
//! ⚠ 对抗评审留痕:官方 `rmcp` SDK 是生产级选择,但它的 stdio 传输离线无法单测、且是重依赖。
//! 本迭代先落**可离线测的协议核心** + 一个最小 stdio 传输;要上生产,把 `StdioTransport`
//! 换成 rmcp 实现即可(`McpTransport` 不变)。

use serde_json::{json, Value};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum McpError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("rpc error {code}: {message}")]
    Rpc { code: i64, message: String },
    #[error("bad response: {0}")]
    BadResponse(String),
}

/// 一个 MCP 服务器暴露的工具(已从 wire 归一化)。
#[derive(Clone, Debug, PartialEq)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    /// 入参 JSON Schema(对应 provider 的 `ToolSpec.schema`)。
    pub input_schema: Value,
}

/// 传输抽象:发一个 JSON-RPC 请求(method + params),拿回 `result`(错误映射成 [`McpError`])。
/// JSON-RPC 信封(jsonrpc/id 关联)由实现内部处理。
#[async_trait::async_trait]
pub trait McpTransport: Send + Sync {
    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError>;

    /// 发一个**通知**(无 id、无响应)。MCP 握手要求 initialize 后发 `notifications/initialized`。
    /// 默认空实现(离线假传输不需要)。
    async fn notify(&self, _method: &str, _params: Value) -> Result<(), McpError> {
        Ok(())
    }
}

/// MCP 客户端:协议逻辑,纯、离线可测。
pub struct McpClient {
    namespace: String,
    transport: Box<dyn McpTransport>,
}

impl McpClient {
    pub fn new(namespace: impl Into<String>, transport: Box<dyn McpTransport>) -> Self {
        Self {
            namespace: namespace.into(),
            transport,
        }
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// `<server>__<tool>` 命名空间(暴露给 LLM / 路由用)。
    pub fn namespaced(&self, tool: &str) -> String {
        format!("{}__{}", self.namespace, tool)
    }

    /// 握手:initialize 请求 + `notifications/initialized` 通知(MCP 规范要求,真实 server 常校验)。
    pub async fn initialize(&self) -> Result<(), McpError> {
        self.transport
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "ridge", "version": "0.1.0"}
                }),
            )
            .await?;
        self.transport
            .notify("notifications/initialized", json!({}))
            .await?;
        Ok(())
    }

    /// 列出服务器工具。
    pub async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        let res = self.transport.request("tools/list", json!({})).await?;
        let arr = res["tools"]
            .as_array()
            .ok_or_else(|| McpError::BadResponse("tools/list 缺 tools 数组".to_string()))?;
        Ok(arr
            .iter()
            .map(|t| McpTool {
                name: t["name"].as_str().unwrap_or("").to_string(),
                description: t["description"].as_str().unwrap_or("").to_string(),
                input_schema: t["inputSchema"].clone(),
            })
            .collect())
    }

    /// 调用一个工具(传**未加命名空间**的原始工具名),返回文本结果。
    pub async fn call_tool(&self, tool: &str, arguments: Value) -> Result<String, McpError> {
        let res = self
            .transport
            .request("tools/call", json!({"name": tool, "arguments": arguments}))
            .await?;
        // content 是块数组,拼接其中的 text 块。
        let text = res["content"]
            .as_array()
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|b| b["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();
        Ok(text)
    }
}

/// 用闭包充当传输(stub / 测试):`Fn(method, &params) -> Result<result, McpError>`。
/// 让上层无需 async-trait 就能造一个假 MCP 服务器。
pub struct FnTransport<F>(pub F);

#[async_trait::async_trait]
impl<F> McpTransport for FnTransport<F>
where
    F: Fn(&str, &Value) -> Result<Value, McpError> + Send + Sync,
{
    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        (self.0)(method, &params)
    }
}

/// 真实 stdio 子进程传输:把 JSON-RPC 一行一条写进子进程 stdin,从 stdout 读回。
///
/// ⚠ Windows 坑:bare `npx`/`uvx` 可能 ENOENT,用绝对路径或 `cmd /c` 包裹。
/// 只做请求-响应(按 id 关联,跳过通知),不接 resources/prompts、不接 SSE/HTTP。
pub struct StdioTransport {
    io: tokio::sync::Mutex<Io>,
    next_id: std::sync::atomic::AtomicU64,
    _child: tokio::process::Child,
}

struct Io {
    stdin: tokio::process::ChildStdin,
    stdout: tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
}

impl StdioTransport {
    pub fn spawn(command: &str, args: &[String]) -> Result<Self, McpError> {
        use std::process::Stdio;
        let mut child = tokio::process::Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| McpError::Transport(format!("spawn {command}: {e}")))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Transport("no stdin".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Transport("no stdout".to_string()))?;
        use tokio::io::AsyncBufReadExt;
        let stdout = tokio::io::BufReader::new(stdout).lines();
        Ok(Self {
            io: tokio::sync::Mutex::new(Io { stdin, stdout }),
            next_id: std::sync::atomic::AtomicU64::new(1),
            _child: child,
        })
    }
}

#[async_trait::async_trait]
impl McpTransport for StdioTransport {
    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        use std::sync::atomic::Ordering;
        use tokio::io::AsyncWriteExt;

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let envelope = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let mut line =
            serde_json::to_string(&envelope).map_err(|e| McpError::Transport(e.to_string()))?;
        line.push('\n');

        let mut guard = self.io.lock().await;
        guard
            .stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| McpError::Transport(e.to_string()))?;
        guard
            .stdin
            .flush()
            .await
            .map_err(|e| McpError::Transport(e.to_string()))?;

        // 读到 id 匹配的那条响应(跳过通知 / 无关行)。
        loop {
            let next = guard
                .stdout
                .next_line()
                .await
                .map_err(|e| McpError::Transport(e.to_string()))?;
            let Some(l) = next else {
                return Err(McpError::Transport("stdout EOF".to_string()));
            };
            let Ok(v) = serde_json::from_str::<Value>(&l) else {
                continue; // 非 JSON 行(日志噪声)跳过
            };
            if v["id"] == json!(id) {
                if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
                    return Err(McpError::Rpc {
                        code: err["code"].as_i64().unwrap_or(0),
                        message: err["message"].as_str().unwrap_or("").to_string(),
                    });
                }
                return Ok(v["result"].clone());
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        use tokio::io::AsyncWriteExt;
        let mut line = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "method": method, "params": params
        }))
        .map_err(|e| McpError::Transport(e.to_string()))?;
        line.push('\n');
        let mut guard = self.io.lock().await;
        guard
            .stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| McpError::Transport(e.to_string()))?;
        guard
            .stdin
            .flush()
            .await
            .map_err(|e| McpError::Transport(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 离线假传输:按 method 回 canned JSON-RPC result。
    struct FakeTransport;

    #[async_trait::async_trait]
    impl McpTransport for FakeTransport {
        async fn request(&self, method: &str, _params: Value) -> Result<Value, McpError> {
            match method {
                "initialize" => Ok(json!({"protocolVersion": "2024-11-05", "capabilities": {}})),
                "tools/list" => Ok(json!({"tools": [
                    {"name": "search", "description": "web search", "inputSchema": {"type": "object"}}
                ]})),
                "tools/call" => {
                    Ok(json!({"content": [{"type": "text", "text": "result from mcp"}]}))
                }
                other => Err(McpError::BadResponse(format!("unexpected method {other}"))),
            }
        }
    }

    #[tokio::test]
    async fn client_handshake_list_and_call() {
        let c = McpClient::new("brave", Box::new(FakeTransport));
        c.initialize().await.unwrap();

        let tools = c.list_tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "search");
        assert_eq!(c.namespaced("search"), "brave__search");

        let out = c
            .call_tool("search", json!({"q": "rust langgraph"}))
            .await
            .unwrap();
        assert_eq!(out, "result from mcp");
    }

    /// RPC 错误要如实映射成 McpError::Rpc。
    #[tokio::test]
    async fn rpc_error_maps_through() {
        struct ErrTransport;
        #[async_trait::async_trait]
        impl McpTransport for ErrTransport {
            async fn request(&self, _m: &str, _p: Value) -> Result<Value, McpError> {
                Err(McpError::Rpc {
                    code: -32601,
                    message: "method not found".to_string(),
                })
            }
        }
        let c = McpClient::new("x", Box::new(ErrTransport));
        let err = c.list_tools().await.unwrap_err();
        assert!(matches!(err, McpError::Rpc { code: -32601, .. }));
    }
}
