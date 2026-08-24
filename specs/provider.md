---
id: L2-PROVIDER-001
level: L2
parent: L1-PROJECT-001
title: LLM provider boundary
status: VALID
code_targets:
  - crates/provider/src/lib.rs
  - crates/provider/src/anthropic.rs
  - crates/provider/src/openai.rs
  - crates/provider/src/responses.rs
  - crates/provider/src/http.rs
test_targets:
  - crates/provider/src/tests.rs
public_interface:
  - provider::LlmProvider
  - provider::Completion
  - provider::ToolCall
  - provider::Message
  - provider::ScriptedProvider
  - provider::ScriptedStream
  - provider::ScriptedStreamChunk
---

# LLM provider boundary

provider 将 Anthropic `tool_use`、OpenAI `tool_calls` 与 Responses wire 格式归一化为 `Completion`、`Message`、`ToolCall`。HTTP 客户端是薄传输层；协议解析与请求构造保持纯函数可测，agent 只依赖 `LlmProvider` trait。

`ScriptedProvider` 亦是确定性流式 harness：按请求顺序消费脚本，每段可即时发送或由 oneshot gate 精确放行，故可在无网络、无 sleep 下复现 SSE 分片边界、并发交错与恢复时机。gate 关闭须 fail-closed，未放行尾段不得泄出；请求仅记录有界形状，不保存正文。
