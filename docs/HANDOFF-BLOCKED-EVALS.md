# 外部验收阻塞项交接

更新时间：2026-09-08  
基线：`main@73942b3`（`feat(tui): unify commands keybindings and performance gates`）

本文只交接必须依赖另一台设备、凭据或长期 runner 的验收工作。不要在本文、提交、CI 日志或诊断附件中写入 API key、OAuth token、Cookie、用户会话正文与本机绝对临时路径。

## 已完成且无需重跑的基线

- GitHub Actions run `34195062762` 已通过 Ubuntu、macOS、Windows workspace build/test、fmt/clippy、确定性覆盖率，以及 Windows ConPTY completion+resize、raw-input 与 recovery 闸门；自托管 Sonar job 按设计跳过。
- provider 测试 60/60 通过，覆盖 OpenAI-compatible/Anthropic/ChatGPT OAuth、流式、tool calls 与 GLM 响应修复。
- ChatGPT OAuth 真实 smoke 已以可用订阅模型完成一次 `verify PASS`，结果为 `approved=true`、`steps=1`。
- 本地有界 eval soak 连续 100 轮通过（300 cases）；recovery soak 连续 5 轮通过，每轮均验证首进程终止后恢复且 `manifest=3`、`unique=3`、`resumed=2`、`passed=3`。
- Windows ConPTY 的 Completion+Resize、InputFixture 与基础 busy cancellation 已通过。`BusyFixture + InspectLive` 尚不能稳定观测 active live block：fixture 的 reasoning 会先提交进 history，当前证据为 `live_blocks=0`；这是 fixture/active-tail 语义缺口，不能表述成已确认的产品故障。
- 2026-09-08 在本机临时直连环境（不读取保存的代理配置）以 ChatGPT OAuth / `gpt-5.6-sol` 跑通一次真实端到端写入：agent 独立生成并验证 `ridgecode-demo.svg`。该 SVG 是纯向量元素（无嵌入/外链图片）；本地验证产物不提交。Ridge desktop `ridge-mcp` 的 custom pane 当前会返回 `identity has no session_id`，因此可抓屏但不能投递任务；这是宿主 pane 会话绑定问题，不能以此替代 provider E2E 或归咎于 agent 图。

## 1. Ark OpenAI-compatible 真实 E2E

状态：**BLOCKED——目标设备需在启动 Codex/RidgeCode 前预注入凭据。**

使用 README 中的 Ark provider 档案，目标端点为 `https://ark.cn-beijing.volces.com/api/plan/v3`，模型为 `glm-5.3`，凭据变量名为 `RIDGECODE_API_KEY`。不要把实际值写进 shell history、配置样例、测试夹具或本文。

验收顺序：

1. 在目标设备的安全环境中预注入 `RIDGECODE_API_KEY`，新开终端并确认变量存在；只检查是否非空，不打印值。
2. 设置或选择 README 所示的 OpenAI-compatible Ark provider，运行一次最小无工具补全，确认返回真实 assistant 文本。
3. 运行一次需要内置只读工具和确定性 verify 的任务，确认 provider 返回的 tool call 被归一化、工具执行成功、最终 `approved=true`。
4. 保存脱敏证据：provider 名、model、HTTP 状态类别、工具名、approved、steps、耗时；不得保存请求头或原始密钥。

若失败，按 401/403（凭据或权限）、404（base URL/model）、429（额度/限流）、协议解析或网络错误分别记录，不得用 scripted provider 冒充真实 E2E。

## 2. 本地 Sonar quality gate

状态：**BLOCKED——当前设备缺少 `SONAR_TOKEN`。** scanner、Cargo、npm 与 `cargo-llvm-cov` 已可用。

目标设备执行：

```powershell
pwsh -File scripts/quality-preflight.ps1
pwsh -File scripts/configure-sonar-secret.ps1
pwsh -File scripts/quality-gate.ps1
```

前置条件：`http://localhost:9000` 的 SonarQube 已启动、项目已创建、token 仅存于当前用户环境。验收要求 line coverage 不低于 80%，scanner 成功上传并等待 quality gate 返回通过；不得降低阈值、扩大排除目录或跳过 Sonar。

## 3. Harbor 标准评测

状态：**BLOCKED——当前设备只有约 49.5 GiB 空闲，低于 120 GiB；Python 命中不可执行的 Store shim；Docker engine 不可达；`harbor` CLI 不在 PATH。** `eval/harbor/ridgecode_agent.py` 已存在且 CI 会做语法检查。

目标设备先执行：

```powershell
pwsh -File scripts/harbor-preflight.ps1
```

必须同时满足：评测存储卷可用空间至少 120 GiB、真实 Python 可执行、Docker engine 正常、Harbor CLI 可执行、adapter 文件存在。preflight 未全绿前不要下载大数据或启动正式 run。正式评测须保存版本、任务集、模型/provider、pass-rate、token/成本和失败分类，并保持原始结果在未跟踪的输出目录中。

## 4. 24 小时 endurance / restart

状态：**BLOCKED——需要可连续运行的专用 runner。**

以固定 commit、固定模型档案和脱敏临时配置运行 24 小时；周期性记录 completed/failed/recovered、RSS、checkpoint 数量与磁盘增长。期间至少触发一次受控进程终止并从 durable state 恢复。通过条件：无任务丢失或重复、内存/快照/日志保持有界、恢复后确定性结果一致、凭据不出现在输出中。短时 soak 只能作为前置检查，不能替代 24 小时结果。

## 双设备协作约定

- 两台设备都直接使用 `main`；每个独立结果提交前执行 `git pull --rebase origin main`，测试通过后立即 push，禁止 force-push。
- 外部验收设备只向本文追加脱敏结果，并只修改为接通验收所必需的脚本/文档；UI 设备负责 TUI 代码及 `specs/agent-ui.md`、`specs/agent-input.md`。
- 若 README 或规范冲突，保留双方事实，手工合并后重跑对应 gate。
- 每个外部任务单独提交；commit message 指明 `eval:` 或 `ci:`，不要把生成报告、coverage、Harbor 数据集或诊断快照纳入 Git。
