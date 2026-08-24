---
id: L3-PROVIDER-TRANSPORT-001
level: L3
parent: L2-PROVIDER-001
title: Provider normalization and transport
status: VALID
code_targets:
  - crates/provider/src/lib.rs
  - crates/provider/src/anthropic.rs
  - crates/provider/src/openai.rs
  - crates/provider/src/responses.rs
  - crates/provider/src/http.rs
  - crates/provider/src/providers.rs
  - crates/provider/src/chatgpt.rs
  - crates/provider/src/models.rs
  - crates/provider/src/oauth.rs
  - crates/provider/src/search.rs
test_targets:
  - crates/provider/src/tests.rs
public_interface:
  - provider::anthropic::parse_response
  - provider::openai::parse_response
  - provider::OpenAiProvider
  - provider::AnthropicProvider
---

# Provider normalization and transport

各厂商适配器只负责 wire 映射与请求发送；错误在 provider 边界归一化。工具调用参数最终为 JSON `Value`，保证 agent/tool 路由不感知厂商差异。
