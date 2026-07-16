# CONTRACT · iter-6:`--read-only` 只读模式(轻量护栏续)

- **开工时间戳**: 2026-07-16
- **上一轮**: iter-5(写操作 jail + denylist 补漏)
- **依据**: 用户择「轻量内核护栏」,含 `--read-only` 开关(iter-5 未做,本轮补)

## 目标(End State)

`--read-only` 模式:agent 只能读/查/研究,**一切副作用工具硬拒**(write_file/edit_file/apply_edits/run_shell)。适合「让它先看看、别动手」的信任场景。纯内核、可离线单测。

## 任务与验收信号(可离线判定)

| 优先级 | 任务 | 确定性验收 |
|---|---|---|
| **P0** | `--read-only` CLI flag(+ env `RIDGE_READ_ONLY`)解析,穿线到图构建 | 单测:`parse_args` 认 `--read-only` |
| **P0** | 只读时 reason 节点**只 offer 只读工具**(复用 `readonly_tool_specs`) | 单测:read_only 装配下工具 spec 不含 run_shell/write_file/edit_file/apply_edits |
| **P0** | 深度防御:act 节点/`execute_tool_call` 即使被调也拒副作用工具 | 单测:read_only 下调 write_file → BLOCKED 且不落盘 |

## 明确不做

- 不引 OS 隔离(Docker/gVisor)——需用户环境/技术选型,重量级另议。
- 不动 iter-5 的 jail/denylist(已稳)。

## 边界

- 不破坏现有测试 + clippy/fmt 干净;纯 std 无新依赖。
- 穿线避免全局可变 env 作唯一真相(测试并发安全):flag 走构建参数,env 仅作 main 层入口。

## 停机条件

`cargo test --workspace` 全绿 + clippy `-D warnings` + fmt `--check`,且新增单测覆盖「只读装配不 offer 写工具」+「深度防御拒写」两信号。
