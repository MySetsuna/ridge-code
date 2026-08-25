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
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

#[derive(Debug, Error)]
pub enum McpError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("rpc error {code}: {message}")]
    Rpc { code: i64, message: String },
    #[error("tool error: {0}")]
    Tool(String),
    #[error("bad response: {0}")]
    BadResponse(String),
}

impl McpError {
    /// Return a bounded diagnostic suitable for status trails and user-facing
    /// observations.  Transport/RPC payloads can contain command arguments,
    /// response bodies, or credentials, so callers should not display the
    /// `Display` text at trust boundaries.
    pub fn redacted_summary(&self) -> String {
        match self {
            Self::Transport(_) => "transport error".to_string(),
            Self::Rpc { code, .. } => format!("RPC error {code}"),
            Self::Tool(_) => "MCP tool error".to_string(),
            Self::BadResponse(_) => "invalid MCP response".to_string(),
        }
    }
}

const CLIENT_PROTOCOL_VERSION: &str = "2025-06-18";
const MAX_TOOL_TEXT_CHARS: usize = 64 * 1024;
pub const MAX_MCP_FRAME_BYTES: usize = 1024 * 1024;
pub const MAX_MCP_TOOLS: usize = 256;

async fn read_bounded_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> Result<Option<Vec<u8>>, McpError> {
    let mut line = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .await
            .map_err(|error| McpError::Transport(error.to_string()))?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Err(McpError::Transport("truncated MCP frame".to_string()))
            };
        }
        if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
            if line.len() + newline + 1 > MAX_MCP_FRAME_BYTES {
                return Err(McpError::BadResponse(
                    "MCP frame exceeds size limit".to_string(),
                ));
            }
            line.extend_from_slice(&available[..newline]);
            reader.consume(newline + 1);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Some(line));
        }
        if line.len() + available.len() > MAX_MCP_FRAME_BYTES {
            return Err(McpError::BadResponse(
                "MCP frame exceeds size limit".to_string(),
            ));
        }
        line.extend_from_slice(available);
        let consumed = available.len();
        reader.consume(consumed);
    }
}

fn bounded_text(text: &str) -> String {
    let mut output = text.chars().take(MAX_TOOL_TEXT_CHARS).collect::<String>();
    if text.chars().nth(MAX_TOOL_TEXT_CHARS).is_some() {
        output.push_str("…[truncated]");
    }
    output
}

fn content_text(response: &Value) -> String {
    let mut parts = response["content"]
        .as_array()
        .into_iter()
        .flat_map(|blocks| blocks.iter())
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        if let Some(structured) = response
            .get("structuredContent")
            .filter(|value| !value.is_null())
        {
            return bounded_text(&structured.to_string());
        }
        return String::new();
    }
    parts.retain(|part| !part.is_empty());
    bounded_text(&parts.join(""))
}

/// 一个 MCP 服务器暴露的工具(已从 wire 归一化)。
#[derive(Clone, Debug, PartialEq)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    /// 入参 JSON Schema(对应 provider 的 `ToolSpec.schema`)。
    pub input_schema: Value,
    pub effect: provider::ToolEffect,
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
        let response = self
            .transport
            .request(
                "initialize",
                json!({
                    "protocolVersion": CLIENT_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": "ridge", "version": "0.1.0"}
                }),
            )
            .await?;
        // Older local fixtures omit the field; when present, it must be a
        // non-empty negotiated version rather than silently accepting a
        // malformed result.
        if let Some(version) = response.get("protocolVersion") {
            if version.as_str().is_none_or(str::is_empty) {
                return Err(McpError::BadResponse(
                    "initialize 缺有效 protocolVersion".to_string(),
                ));
            }
        }
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
        if arr.len() > MAX_MCP_TOOLS {
            return Err(McpError::BadResponse(format!(
                "tools/list exceeds {MAX_MCP_TOOLS} tools"
            )));
        }
        let mut tools = Vec::with_capacity(arr.len());
        for (index, tool) in arr.iter().enumerate() {
            let name = tool["name"]
                .as_str()
                .filter(|name| !name.is_empty())
                .ok_or_else(|| McpError::BadResponse(format!("tools/list 工具 {index} 缺 name")))?;
            let input_schema = tool
                .get("inputSchema")
                .filter(|schema| !schema.is_null())
                .cloned()
                .unwrap_or_else(|| json!({"type": "object"}));
            let description = tool["description"].as_str().unwrap_or("");
            tools.push(McpTool {
                name: name.to_string(),
                description: description.to_string(),
                input_schema,
                effect: provider::ToolEffect::from_metadata(name, description, Some(tool)),
            });
        }
        Ok(tools)
    }

    /// 调用一个工具(传**未加命名空间**的原始工具名),返回文本结果。
    pub async fn call_tool(&self, tool: &str, arguments: Value) -> Result<String, McpError> {
        let res = self
            .transport
            .request("tools/call", json!({"name": tool, "arguments": arguments}))
            .await?;
        // content 是块数组,拼接其中的 text 块。
        // MCP execution failures are successful JSON-RPC responses carrying
        // `isError: true`; do not turn them into an empty successful
        // observation. Preserve only a bounded diagnostic at this boundary.
        if res["isError"].as_bool() == Some(true) {
            let message = content_text(&res);
            return Err(McpError::Tool(if message.is_empty() {
                "MCP tool reported an error".to_string()
            } else {
                message
            }));
        }
        Ok(content_text(&res))
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
    stdout: tokio::io::BufReader<tokio::process::ChildStdout>,
}

impl StdioTransport {
    pub fn spawn(command: &str, args: &[String]) -> Result<Self, McpError> {
        Self::spawn_with_env(command, args, &[])
    }

    /// Spawn with a deliberately small inherited environment. Provider keys,
    /// `RIDGE_*`, proxy credentials, and arbitrary shell state stay out of MCP
    /// children unless explicitly declared for that server.
    pub fn spawn_with_env(
        command: &str,
        args: &[String],
        env: &[(String, String)],
    ) -> Result<Self, McpError> {
        use std::process::Stdio;
        let mut process = tokio::process::Command::new(command);
        process
            .args(args)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true);
        for (key, value) in filtered_environment(env) {
            process.env(key, value);
        }
        let mut child = process
            .spawn()
            .map_err(|e| McpError::Transport(format!("MCP process spawn failed: {e}")))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Transport("no stdin".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Transport("no stdout".to_string()))?;
        let stdout = tokio::io::BufReader::new(stdout);
        Ok(Self {
            io: tokio::sync::Mutex::new(Io { stdin, stdout }),
            next_id: std::sync::atomic::AtomicU64::new(1),
            _child: child,
        })
    }
}

const SAFE_ENV_KEYS: &[&str] = &[
    "PATH",
    "PATHEXT",
    "SYSTEMROOT",
    "WINDIR",
    "COMSPEC",
    "SYSTEMDRIVE",
    "TEMP",
    "TMP",
    "HOME",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "LANG",
    "LC_ALL",
    "TERM",
    "COLORTERM",
];

fn safe_env_key(key: &std::ffi::OsStr) -> bool {
    let Some(key) = key.to_str() else {
        return false;
    };
    SAFE_ENV_KEYS.iter().any(|allowed| {
        if cfg!(windows) {
            key.eq_ignore_ascii_case(allowed)
        } else {
            key == *allowed
        }
    })
}

fn filtered_environment(
    overrides: &[(String, String)],
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (key, value) in std::env::vars_os() {
        if safe_env_key(&key) {
            let normalized = key.to_string_lossy().to_ascii_lowercase();
            if seen.insert(normalized) {
                out.push((key, value));
            }
        }
    }
    for (key, value) in overrides {
        if key.is_empty() {
            continue;
        }
        out.retain(|(existing, _)| !existing.to_string_lossy().eq_ignore_ascii_case(key));
        out.push((key.clone().into(), value.clone().into()));
    }
    out
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
            let Some(line) = read_bounded_line(&mut guard.stdout).await? else {
                return Err(McpError::Transport("stdout EOF".to_string()));
            };
            let Ok(v) = serde_json::from_slice::<Value>(&line) else {
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
        assert_eq!(tools[0].effect, provider::ToolEffect::Explore);
        assert_eq!(c.namespaced("search"), "brave__search");

        let out = c
            .call_tool("search", json!({"q": "rust langgraph"}))
            .await
            .unwrap();
        assert_eq!(out, "result from mcp");
    }

    #[tokio::test]
    async fn tool_execution_error_is_not_reported_as_empty_success() {
        let c = McpClient::new(
            "server",
            Box::new(FnTransport(|method: &str, _params: &Value| match method {
                "tools/call" => Ok(json!({
                    "isError": true,
                    "content": [{"type": "text", "text": "secret backend detail"}]
                })),
                _ => Ok(json!({})),
            })),
        );
        let err = c.call_tool("search", json!({})).await.unwrap_err();
        assert!(matches!(err, McpError::Tool(_)));
        assert_eq!(err.redacted_summary(), "MCP tool error");
        assert!(!err.redacted_summary().contains("secret"));
    }

    #[tokio::test]
    async fn list_tools_rejects_missing_name_and_defaults_schema() {
        let missing = McpClient::new(
            "server",
            Box::new(FnTransport(|method: &str, _params: &Value| match method {
                "tools/list" => Ok(json!({"tools": [{"description": "broken"}]})),
                _ => Ok(json!({})),
            })),
        );
        assert!(matches!(
            missing.list_tools().await,
            Err(McpError::BadResponse(_))
        ));

        let schema = McpClient::new(
            "server",
            Box::new(FnTransport(|method: &str, _params: &Value| match method {
                "tools/list" => Ok(json!({"tools": [{"name": "search"}]})),
                _ => Ok(json!({})),
            })),
        );
        let tools = schema.list_tools().await.unwrap();
        assert_eq!(tools[0].input_schema, json!({"type": "object"}));
    }

    #[tokio::test]
    async fn list_tools_preserves_mcp_effect_annotations_and_unknown_default() {
        let client = McpClient::new(
            "server",
            Box::new(FnTransport(|method: &str, _params: &Value| match method {
                "tools/list" => Ok(json!({
                    "tools": [
                        {
                            "name": "opaque_action",
                            "description": "opaque operation",
                            "annotations": {"readOnlyHint": false}
                        },
                        {"name": "opaque_write_action", "description": "opaque operation"}
                    ]
                })),
                _ => Ok(json!({})),
            })),
        );
        let tools = client.list_tools().await.unwrap();
        assert_eq!(tools[0].effect, provider::ToolEffect::Edit);
        assert_eq!(tools[1].name, "opaque_write_action");
        assert_eq!(tools[1].effect, provider::ToolEffect::Unknown);
    }

    #[tokio::test]
    async fn list_tools_rejects_registry_above_hard_limit() {
        let client = McpClient::new(
            "server",
            Box::new(FnTransport(|method: &str, _params: &Value| match method {
                "tools/list" => Ok(json!({
                    "tools": (0..=MAX_MCP_TOOLS)
                        .map(|index| json!({"name": format!("tool-{index}")}))
                        .collect::<Vec<_>>()
                })),
                _ => Ok(json!({})),
            })),
        );
        assert!(matches!(
            client.list_tools().await,
            Err(McpError::BadResponse(message)) if message.contains("exceeds")
        ));
    }

    #[test]
    fn stdio_child_environment_keeps_runtime_path_but_drops_secrets() {
        let inherited = filtered_environment(&[]);
        let names = inherited
            .iter()
            .map(|(key, _)| key.to_string_lossy().to_ascii_lowercase())
            .collect::<std::collections::BTreeSet<_>>();
        if std::env::var_os("PATH").is_some() {
            assert!(names.contains("path"));
        }
        assert!(!names.contains("ridge_api_key"));
        assert!(!names.iter().any(|name| {
            name.starts_with("ridge_")
                || name.contains("api_key")
                || name.contains("token")
                || name.contains("secret")
        }));

        let explicit = filtered_environment(&[("MCP_TEST_TOKEN".into(), "allowed".into())]);
        assert!(explicit
            .iter()
            .any(|(key, value)| key == "MCP_TEST_TOKEN" && value == "allowed"));
    }

    #[tokio::test]
    async fn stdio_child_receives_allowlisted_path_without_parent_secret() {
        let prior = std::env::var_os("RIDGE_TEST_SECRET");
        std::env::set_var("RIDGE_TEST_SECRET", "must-not-reach-child");
        let path = std::env::temp_dir().join(format!(
            "ridge_mcp_env_probe_{}_{}{}",
            std::process::id(),
            line!(),
            if cfg!(windows) { ".cmd" } else { ".sh" }
        ));
        #[cfg(windows)]
        let (command, args, script) = (
            "cmd.exe",
            vec![
                "/d".to_string(),
                "/s".to_string(),
                "/c".to_string(),
                path.to_string_lossy().into_owned(),
            ],
            "@echo off\nset \"line=\"\nset /p line=\nif defined RIDGE_TEST_SECRET (set \"secret=true\") else (set \"secret=false\")\nif defined PATH (set \"path=true\") else (set \"path=false\")\necho {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"secret\":%secret%,\"path\":%path%}}\n",
        );
        #[cfg(not(windows))]
        let (command, args, script) = (
            "sh",
            vec![path.to_string_lossy().into_owned()],
            "read line\nif [ -n \"${RIDGE_TEST_SECRET+x}\" ]; then secret=true; else secret=false; fi\nif [ -n \"${PATH+x}\" ]; then path=true; else path=false; fi\nprintf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"secret\":%s,\"path\":%s}}\\n' \"$secret\" \"$path\"\n",
        );
        std::fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&path, permissions).unwrap();
        }
        let transport = StdioTransport::spawn_with_env(command, &args, &[]).unwrap();
        let response = transport
            .request("probe", json!({}))
            .await
            .expect("stdio probe response");
        drop(transport);
        let _ = std::fs::remove_file(&path);
        match prior {
            Some(value) => std::env::set_var("RIDGE_TEST_SECRET", value),
            None => std::env::remove_var("RIDGE_TEST_SECRET"),
        }
        assert_eq!(response["secret"], false);
        assert_eq!(response["path"], true);
    }

    #[tokio::test]
    async fn stdio_reader_rejects_oversized_line_before_allocating_unboundedly() {
        use tokio::io::AsyncWriteExt;

        let (mut writer, reader) = tokio::io::duplex(MAX_MCP_FRAME_BYTES + 2);
        let mut payload = vec![b'x'; MAX_MCP_FRAME_BYTES];
        payload.push(b'x');
        payload.push(b'\n');
        writer.write_all(&payload).await.unwrap();
        drop(writer);

        let mut reader = tokio::io::BufReader::new(reader);
        assert!(matches!(
            read_bounded_line(&mut reader).await,
            Err(McpError::BadResponse(message)) if message.contains("size limit")
        ));
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
