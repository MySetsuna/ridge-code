# ridge-code

成本优化的编码 agent CLI(Rust)。方向与架构见 [PLAN.md](./PLAN.md),动手指南见 [HANDOFF.md](./HANDOFF.md)。

## M0 现状(walking skeleton)

单模型 agent loop:读 `~/.ridge/config.toml` 里的 OpenAI 兼容 provider,带工具(`read_file` / `write_file` / `list_dir` / `run_shell`)循环执行一个编码任务,直到模型给出最终答复。**还没有编排、验证器、修复循环**——那些是 M1/M2(见 PLAN.md §10)。

## 跑起来

```bash
cp config.example.toml ~/.ridge/config.toml   # 填 base_url / model / key
export RIDGE_API_KEY=sk-...                    # 或直接在 config 里填 api_key

cargo run -p ridge-code -- "在 src/lib.rs 里加一个返回 42 的 fn answer()"
# 看更详细日志:
RUST_LOG=debug cargo run -p ridge-code -- "..."
```

## workspace 布局

| crate | 角色 | 里程碑 |
|---|---|---|
| `rc-types` | 纯数据类型(serde) | M0 ✅ |
| `rc-providers` | provider 抽象 + OpenAI 兼容实现 | M0 ✅ |
| `rc-tools` | 内置工具(fs / shell) | M0 ✅ |
| `rc-cli` | 二进制入口 + 单模型 agent loop | M0 ✅ |
| `rc-verify` | 验证器(编译/测试/lint) | M1(占位) |
| `rc-core` | 编排大脑 | M2(占位) |
| `rc-mcp` | MCP 客户端(rmcp) | M4(占位) |
| `rc-eval` | eval harness | M3(占位) |
