# CONTRACT —— Iteration 02:物理闭环(M1)

- **开工时间戳**: 2026-07-13
- **里程碑**: M1 物理闭环
- **依据**: `docs/iterations/2026-07-13-notebooklm-guidance-01.md`(NotebookLM 指导)

## 目标(End State)

让 agent 从「真空运行」走向能触碰真实世界:先补齐 **真实工具链(FS + Shell)**,再接 **真实 LLM**,使 ReAct 循环能真正读写代码、调编译器修错。

## 任务与验收信号(可验证,别写「改进代码」)

| 优先级 | 任务 | 确定性验收信号 | 状态 |
|---|---|---|---|
| **P0** | `crates/tools`:真实 `read_file` / `write_file` / `run_shell`,跨平台 | `cargo test -p tools` 全绿:write→read 往返内容一致(哈希变化符合预期);`run_shell("exit 0")` 返回 code 0、`exit 3` 返回 3 | ✅ 本轮完成 |
| **P0** | 把 `run_shell` 包成 agent 的 `Tool`(`shell_tool()`),打通「act 节点执行真实命令」的接缝 | `cargo test -p agent`:`shell_tool()("exit 0")` 输出反映真实退出码 | ✅ 本轮完成 |
| **P1** | `crates/provider`:`LlmProvider` trait + 一个真实实现(Anthropic 或 OpenAI 兼容),工具调用归一化 | 离线录制响应单测通过;`Brain` 换成真实 provider 后 demo 能修一个 `rustc` 报错到 `approved=true` | ⬜ 下轮 |
| **P1** | 结构化 tool_call:`Brain::decide` 升级为返回 `{tool, args}`,`act` 据此调真实工具 | 单测:给定一段带 tool_call 的模型输出,解析出正确的工具名与参数并执行 | ⬜ 下轮 |

## 边界(Constraints)

- 不破坏现有 9 项测试与 `clippy -D warnings` / `fmt` 干净。
- provider 边界:第三方 SDK 包在自己的 trait 后,不让 `langgraph`/`agent` 直接依赖具体实现。
- `run_shell` 暂不做沙箱(M1 先跑通);沙箱(Docker/gVisor/WASM)留到 harness 阶段。**⚠ 因此本轮工具默认只在受控命令上测试,不接受不可信输入执行。**
- 密钥永不写日志。

## 停机 / 预算

- 硬回合上限沿用 agent 的 `MAX_STEPS`;真实 LLM 接入后加 token 预算熔断(P3,下轮)。
- 无进展检测:连续 N 轮 `cargo check` 报错内容不变 → 停机并在报告记录熔断。

## 授权阶梯

保持 **Level 2 (Draft)**:改动在工作区/分支,人做物理信号验证(`cargo test`)后再合并,不 auto-merge。
