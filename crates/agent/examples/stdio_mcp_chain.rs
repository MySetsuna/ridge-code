//! Exercise the real stdio MCP transport through the RidgeCode agent graph.
//
// Usage on Windows:
// cargo run -p agent --example stdio_mcp_chain -- cmd.exe /d /s /c "codegraph serve --mcp"
//
// The first completion emits one namespaced tool call and the second emits a
// final answer. No provider credentials or model network are used.

use agent::{build_llm_agent_with, resolve_mcp, AgentState};
use mcp::{McpClient, StdioTransport};
use provider::{Completion, ScriptedProvider, ToolCall};
use serde_json::Value;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: stdio_mcp_chain <command> [args...]"))?;
    let command_args: Vec<String> = args.collect();
    let namespace = std::env::var("RIDGE_MCP_NAMESPACE").unwrap_or_else(|_| "stdio".into());
    let raw_tool = std::env::var("RIDGE_MCP_TOOL").unwrap_or_else(|_| "codegraph_explore".into());
    let arguments = std::env::var("RIDGE_MCP_ARGS")
        .ok()
        .map(|raw| serde_json::from_str::<Value>(&raw))
        .transpose()?
        .unwrap_or_else(|| {
            serde_json::json!({
                "query": "MCP chain smoke: locate AgentState",
                "projectPath": std::env::current_dir()
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            })
        });

    let transport = StdioTransport::spawn(&command, &command_args)?;
    let client = Arc::new(McpClient::new(&namespace, Box::new(transport)));
    let mcp = resolve_mcp(vec![client]).await;
    let qualified_tool = format!("{namespace}__{raw_tool}");
    anyhow::ensure!(
        mcp.tool_names().iter().any(|name| name == &qualified_tool),
        "MCP tool not discovered: {qualified_tool}; available: {:?}",
        mcp.tool_names()
    );

    let provider = ScriptedProvider::new(vec![
        Completion {
            tool_calls: vec![ToolCall {
                id: "stdio-smoke-call".into(),
                name: qualified_tool.clone(),
                arguments,
            }],
            ..Default::default()
        },
        Completion {
            text: "stdio MCP result received".into(),
            ..Default::default()
        },
    ]);
    let app = build_llm_agent_with(Arc::new(provider), mcp)?;
    let output = app
        .invoke(AgentState::new("run one real stdio MCP query"))
        .await?;
    let tool_result = output
        .tool_output
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("agent produced no tool result"))?;
    anyhow::ensure!(!tool_result.trim().is_empty(), "MCP tool result was empty");
    anyhow::ensure!(
        output.history.iter().any(|message| {
            message.role == provider::Role::Tool
                && message.tool_call_id.as_deref() == Some("stdio-smoke-call")
        }),
        "tool result missing from agent history"
    );
    println!(
        "stdio_mcp_chain: passed; tool={qualified_tool}; result_chars={}; history={}",
        tool_result.chars().count(),
        output.history.len()
    );
    Ok(())
}
