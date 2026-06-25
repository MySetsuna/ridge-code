# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

> 与本仓库的既有文档保持一致,本文件用中文。架构与方向的「为什么」见 `PLAN.md`,从零动手的步骤见 `HANDOFF.md`,使用说明见 `README.md`——本文件只补充它们没讲清的「怎么干活」。

## 这是什么

成本优化的**编码 agent CLI**(Rust workspace,单二进制交付)。核心赌注:编码领域有免费的客观验证器(编译/测试/lint),所以可以「弱模型扛执行量 + 强模型管规划/修复/评审 + 客观验证做免费质量闸 + 级联路由压成本」。省钱主要来自级联(弱先做,验证不过才升强),而非多开 agent。详见 `PLAN.md §1-2`。

里程碑现状:**M0/M1/M2 已完成**(walking skeleton → 验证器+修复循环 → 强/弱编排大脑);`rc-eval`(M3 eval 闭环)与 `rc-mcp`(M4 MCP 客户端)仍是占位 lib。

## 常用命令

```bash
cargo build                        # 全 workspace 构建
cargo test                         # 全 workspace 测试(目前实测仅 rc-verify 有单测)
cargo test -p rc-verify            # 跑单个 crate 的测试
cargo test -p rc-verify passing_check   # 跑单个测试函数
cargo clippy                       # lint

# 运行 agent(注意:二进制叫 ridge-code,但它住在 crates/rc-cli;package 名就是 ridge-code)
cargo run -p ridge-code -- --cwd /path/to/target/project "实现 add/mul 并各写一个单元测试"
RUST_LOG=debug cargo run -p ridge-code -- "..."   # 详细日志(看清 DAG/工具每一步)
```

**易踩的坑:** `-p ridge-code`(不是 `-p rc-cli`)——目录是 `rc-cli`,但 `crates/rc-cli/Cargo.toml` 里的 package 名是 `ridge-code`。

## 运行前置(配置 + 密钥)

1. **全局配置** `~/.ridge/config.toml`(模板见仓库根 `config.example.toml`)。两种写法:
   - 混合(推荐):`[strong]` + `[weak]` 两段,可指向不同 provider/模型;
   - 兼容:单 `[provider]` 段(强=弱都用它)。
   解析逻辑:`strong` 缺省回退到 `provider`;`weak` 缺省回退到 `provider`,再缺省复用 `strong`(见 `rc-cli/src/main.rs`)。
2. **密钥**按此顺序解析(`resolve_api_key`):provider 段里的 `api_key` → `api_key_env` 指定的环境变量 → 默认环境变量 `RIDGE_API_KEY`。
3. **`.env.local` / `.env`** 在启动时自动加载(`dotenvy`),且**在 `--cwd` 切换目录之前**加载——所以这两个文件按「你敲命令的目录」查找,不是目标项目目录。已导出的环境变量优先于文件。
4. **模型必须支持 tool calling**,别用纯推理型(如 `*-r1`),否则工具循环跑不起来。

目标项目可放 `ridge.toml` 声明验证命令;否则按 `Cargo.toml`/`package.json` 自动探测(`rc-verify::resolve_plan`):
```toml
[verify]
build = "cargo build"
test  = "cargo test"
# lint = "cargo clippy"
```

## 架构:编排流水线

入口 `rc-cli` 是**薄壳**(读配置 → 造两个 provider → 构造 `Orchestrator` → 跑 → 打印成本账单),全部编排逻辑在 `rc-core::Orchestrator::run`:

```
① Planner(强)   把任务分解成 2-5 个有序子任务(要 JSON 数组);解析失败降级为单个 Hard 子任务
② Router+Worker  逐个子任务按难度路由:Difficulty::Hard → 强,其余 → 弱;Worker 跑工具循环改代码
③ Verify+Repair  跑 ridge.toml/探测出的检查 → Pass/Uncertain 收工;Fail 则【强模型】带诊断修复,最多 max_repairs 轮
④ Reviewer(强)  只读工具(read_file/list_dir)评审是否满足任务,要 JSON {approved, issues};未通过则强模型据评审修一轮再复验
            ↓
        输出报告 + 成本账单(关键指标:强模型 token 占比,越低越省钱)
```

`run_agent` 是通用工具循环:调模型 → 若无 tool_calls 则返回最终文本,否则执行每个工具调用、把结果回灌、循环到 `max_steps`。每步按档位累加 `Cost`。

**贯穿设计的成本杠杆(改编排时别破坏):** 级联而非平铺并发;执行者(弱)≠ 验证者(强);难任务直接给强,跳过「弱做→打回」;修复/规划/评审一律走强模型;review 选择性、不逐字深读(否则吃掉省的钱)。

### M2 的已知简化(改动时心里有数)

- 子任务**按规划顺序串行执行**;`Subtask.deps` 已记录但尚未用于并行调度(并行 + git worktree 隔离留待后续)。
- `write_file` 是**整文件覆盖**(无结构化 patch 工具),所以 Worker 的 system prompt 强调「先读出再保留」。
- build/test 闸盖不住「能编译但偏离规格」;Reviewer 自身也可能误判——语义正确性靠 M3 eval 度量(尚未做)。
- 模型输出的 JSON 用 `extract_between`(取首个 `[`/`{` 到末个 `]`/`}`)抠出来,容忍模型包裹的解释文字。

## Crate 布局与依赖方向

依赖自底向上(改下层留意上层影响):

```
rc-types  →  { rc-providers, rc-tools, rc-verify }  →  rc-core  →  { rc-cli, (rc-eval) }
```

| crate | 角色 | 关键约束 |
|---|---|---|
| `rc-types` | 纯数据 + serde,**零业务逻辑** | 所有 crate 依赖它;保持无业务依赖 |
| `rc-providers` | `LlmProvider` trait + OpenAI 兼容实现 | 见下方「provider 边界」 |
| `rc-tools` | 内置工具 read_file/write_file/list_dir/run_shell | 工具错误转成给模型看的文本(让它自我纠正),不向上抛 |
| `rc-verify` | 验证 runner:跑命令、解析输出成 `Diagnostic`、产出 `Verdict` | 失败输出截断保留**末尾** `MAX_DETAIL` 字符(编译错误结论在尾部) |
| `rc-core` | 编排大脑(上面的流水线) | 只依赖 `LlmProvider` trait,不碰具体 provider |
| `rc-cli` | 二进制入口(薄壳) | package 名 = `ridge-code` |
| `rc-mcp` / `rc-eval` | M4 / M3 占位 | 暂空 |

## 工程约定(来自 HANDOFF.md §5,落地时遵守)

- **provider 边界(最重要):** 第三方多 provider crate 必须包在自己的 `LlmProvider` trait 后面,**绝不让 `rc-core` 直接依赖**——保证可替换、可对单个 provider 降级到裸 reqwest 自己拼。工具调用的归一化(Anthropic `tool_use` vs OpenAI `tool_calls`)统一成 `rc-types` 的内部表示,wire 格式类型在 `rc-providers` 内部私有。
- **依赖版本统一在根 `Cargo.toml` 的 `[workspace.dependencies]`** 管;子 crate 用 `dep.workspace = true`。
- **可观测性走 `tracing`**,别用 `println!` 调试编排(`println!` 只用于最终报告)。关键编排步骤已打 info/debug 日志。
- **错误:** 约定库层用 `thiserror`、应用层用 `anyhow`(当前实现普遍用 `anyhow`,新增库级错误类型时按约定走 thiserror)。
- **密钥永不写日志。**
- **provider 层测试**用录制的固定响应做离线测试,避免每跑一次就烧 key。
