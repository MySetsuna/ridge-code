# Open Vision 实现切片

`REQ-20260810-OPEN-VISION-01` 已获批准。下表将来源 Note 中七类方向绑定到有界代码接缝、确定性测试与质量证据；方向不改变既有权限门、只读子 agent、maker/checker、verify、BSP 或 MCP/provider 边界。

| 切片 | 落地接缝 | 关键测试 | 状态 |
| --- | --- | --- | --- |
| OV-01 agent 通信安全与隐私 | `AgentVerifier`、`SecurityAuditLog`、现有 `AgentEnvelope` HMAC/时钟/nonce；TEE 不作未经平台证据支持的伪实现 | `verifier_binds_key_and_keeps_secret_out_of_audit`、`signed_envelope_rejects_tampering_expiry_and_replay` | 已实现 |
| OV-02 自主权与治理元数据 | `GovernanceMetadata`、`AutonomyTier`、`authorize_governance` | `governed_envelope_round_trips_and_requires_explicit_approval`、`handshake_negotiates_capabilities_and_enforces_read_only` | 已实现 |
| OV-03 原生 grapheme/Unicode 探测与回退 | `UnicodeCapabilities`、原生宽度与安全占位回退 | `unicode_detection_has_native_and_safe_fallback_paths`、TUI CJK/emoji/换行回归集 | 已实现 |
| OV-04 图谱支撑推理接缝 | `GraphReasoningBridge` 投影 langgraph 节点/超步为有界树 | `graph_bridge_and_audit_tree_enforce_parent_and_bounds`、workspace graph tests | 已实现 |
| OV-05 离线 store-and-forward/federated A2A | `StoreForwardQueue`、`FederatedOutbox`，NDJSON、去重、原子替换、容量阀 | `store_forward_queue_persists_deduplicates_and_acks`、`federation_rejects_unknown_or_wrong_target_peer` | 已实现 |
| OV-06 有界 Web/PWA 事件流与接管 | `BoundedEventStream`，序号增量、容量/载荷阀、单租约接管 | `bounded_web_stream_evicts_old_events_and_serializes_takeover` | 已实现 |
| OV-07 推理树离线审计 | `ReasoningTree`、`ReasoningAuditLog`，摘要上限、哈希链、离线完整性核验 | `offline_reasoning_audit_is_hash_chained_and_detects_tampering`、`reasoning_tree_rejects_missing_parent_and_overflow` | 已实现 |

## 质量合同

统一阀由 `scripts/quality-gate.ps1`、`scripts/quality-gate.sh` 与 `.github/workflows/quality-gate.yml` 共同执行：格式、workspace 测试、`clippy -D warnings`、workspace build、`cargo llvm-cov` 的批准阈值、`git diff --check` 与本机 SonarQube quality gate 均为硬失败条件。质量阀失败时只能修复代码、测试或架构后重跑；不得降阈值、跳过扫描、扩大排除或伪造报告。

证据路径：`target/quality/` 覆盖率报告、SonarQube 项目 `ridge-code`、GitHub Actions release quality run、`docs/PROJECT-STATE.md` 与 `docs/archive/events-2026-08.jsonl`。本切片不上传密钥、cookie、原始会话或隐藏推理；推理审计只接收显式安全摘要。
