//! 连一个真实 stdio MCP server,握手 + 列举工具(验证 ridge 的 MCP 客户端对真实 server 可用)。
//!
//! 用法:`cargo run -p mcp --example connect -- <command> [args...]`
//! 例:  `cargo run -p mcp --example connect -- C:/Users/you/.local/bin/notebooklm-mcp.exe`

use mcp::{McpClient, StdioTransport};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let cmd = args.next().ok_or("usage: connect <command> [args...]")?;
    let rest: Vec<String> = args.collect();

    eprintln!("[connect] spawning: {cmd} {rest:?}");
    let transport = StdioTransport::spawn(&cmd, &rest)?;
    let client = McpClient::new("srv", Box::new(transport));

    client.initialize().await?;
    let tools = client.list_tools().await?;

    println!("✅ connected. {} tools discovered:", tools.len());
    for t in &tools {
        let desc = t.description.chars().take(70).collect::<String>();
        println!("  · {:<28} {}", t.name, desc);
    }
    Ok(())
}
