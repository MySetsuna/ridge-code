---
id: L3-MCP-TRANSPORT-001
level: L3
parent: L2-MCP-001
title: MCP protocol and stdio transport
status: VALID
code_targets:
  - crates/mcp/src/lib.rs
  - crates/mcp/examples/connect.rs
  - crates/agent/examples/stdio_mcp_chain.rs
test_targets:
  - crates/mcp/src/lib.rs
public_interface:
  - mcp::McpClient::initialize
  - mcp::McpClient::list_tools
  - mcp::McpClient::call_tool
  - mcp::StdioTransport
  - mcp::MAX_MCP_FRAME_BYTES
  - mcp::MAX_MCP_TOOLS
---

# MCP protocol and stdio transport

`McpTransport` 抽象协议请求/通知，使 JSON-RPC 核心可离线测试；`StdioTransport` 负责真实子进程通信。客户端发起 MCP `2025-06-18` 初始化并接受服务端返回的非空协商版本（旧测试 transport 可省略该字段），随后发送 `notifications/initialized`。stdio 逐段读取 JSON-RPC 行并在分配继续增长前执行 1 MiB 硬限；transport drop 会终止仍存活的子进程。`tools/list` 最多接受 256 项，拒绝缺名工具并为缺失 schema 使用 object 默认值；`tools/call` 将 MCP `isError: true` 映射为显式 `McpError::Tool`，文本/structuredContent 诊断有 64 KiB 上限。协议层不把具体 server 或模型 SDK 写死。
