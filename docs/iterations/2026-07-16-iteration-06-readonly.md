# 迭代报告 · 2026-07-16 · iter-6:`--read-only` 只读模式(轻量护栏续)

> 承 iter-5(写 jail + denylist)。补齐用户所择「轻量内核护栏」套件的最后一项。

## 做了什么(纯内核,双保险)

`--read-only`(别名 `--readonly`,+ env `RIDGE_READ_ONLY`):agent 只能读/查/研究,一切副作用工具硬拒。

- **Offering 过滤(源头断写)**:`build_core` 加 `read_only` 参;只读时从工具 spec 里 `retain` 掉 mutating(run_shell/write_file/edit_file/apply_edits)、且**不 offer MCP**(副作用未知);`dispatch_agent` 保留(子 agent 恒只读,安全)。
- **深度防御(双保险)**:`read_only_block(read_only, name)` —— act 节点 obs 首臂判定,副作用工具即使被幻觉调到也回 `BLOCKED (read-only)`。
- **穿线**:`build_llm_agent_full` → `run_once`/`headless`/`tui::run`;`parse_args` 改返 `ParsedArgs` 结构体(避免裸元组膨胀)。**flag 走构建参数**,env 仅 main 层入口 —— 不引全局可变状态,测试并发安全。

## 测试状态(确定性信号)

```
cargo test --workspace          # 全绿(+2 测)
cargo clippy --workspace ... -D warnings   # 净
cargo fmt --all --check         # 净
```

- `read_only_filters_out_mutating_tools`:只读 offering 排除 4 个写工具、保留 read_file/search/web_search/fetch_url/todo_write。
- `read_only_block_rejects_mutating_only`:只拦副作用工具、读类放行、非只读一律不拦、拒串前缀 `BLOCKED (read-only)`。

## 轻量沙箱套件收束

用户所择「轻量内核护栏」全部落地(纯 std、跨平台、可离线单测):
| 项 | 轮次 |
|---|---|
| 写操作 cwd jail(挡绝对/`..` 逃逸) | iter-5 |
| 危险命令 denylist 补漏 | iter-5 |
| `--read-only` 只读模式(offering 过滤 + 深度防御) | iter-6 |

**残余(非轻量层职责)**:符号链接逃逸(词法不解析 symlink)、run_shell 内的真隔离 —— 皆待真 OS 沙箱(Docker/gVisor,需用户环境/技术选型)。

## 现有安全全景

危险命令硬拦截 + 权限门 + diff 预览 + **写操作 cwd jail** + **`--read-only` 模式** + `--yolo` skip-danger 逃生阀。真 OS 隔离另议。
