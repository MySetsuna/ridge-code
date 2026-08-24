---
id: L2-MCP-001
level: L2
parent: L1-PROJECT-001
title: MCP protocol integration
status: VALID
code_targets:
  - crates/mcp/src/lib.rs
  - crates/agent/src/mcp_tools.rs
test_targets:
  - crates/mcp/src/lib.rs
  - crates/agent/src/mcp_tools.rs
public_interface:
  - mcp::McpClient
  - mcp::McpTransport
  - mcp::StdioTransport
  - agent::McpTools
---

# MCP protocol integration

MCP 客户端实现 JSON-RPC 2.0 的 `initialize`、`tools/list`、`tools/call`；工具以 `<server>__<tool>` 命名空间接入 agent。初始化发送 `2025-06-18` 并完成版本协商，工具执行错误保留 `isError` 语义而非伪装为空成功，列表坏形态与 schema 缺失有确定性降级。启动阶段并发解析多个 server，单个失败超时只降级该 server，不阻塞其余工具。
