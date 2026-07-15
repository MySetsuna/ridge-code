# samples —— 开箱即用的官方样例

RidgeCode 的核心承诺:**加能力 = 加一个 `SKILL.md` 或一段 MCP 配置,不改源码。** 这里是几个能直接抄的样例。

## Skills(声明式领域技能)

把 `skills/<name>/` 拷到 `~/.ridge/skills/<name>/`(或用 `RIDGE_SKILLS_DIR` 指到别处),启动 `ridgecode` 就会自动加载并注入 system prompt。

- `skills/researcher/SKILL.md` —— 调研:web_search → fetch_url → 据原文作答、带来源。
- `skills/rust-fixer/SKILL.md` —— 修 Rust:信编译器不信自述、精准/批量编辑、改完必复核。
- `skills/triage/SKILL.md` —— 分诊:一堆待办/报错 → 归因、排优先级、列清单、交决策。
- `skills/summarize/SKILL.md` —— 摘要:长文/文件/网页压成 TL;DR + 要点,保真去水。
- `skills/translate/SKILL.md` —— 翻译:中英互译/润色,信达雅、术语统一、只给译文。

**非编程域也照做**(summarize/translate)—— 这正是「模块化跨领域框架」的意思:换个 `SKILL.md` 就换个领域,不改一行 Rust。

`SKILL.md` = YAML frontmatter(`name` + `description`)+ 正文。`description` 写清「什么时候用」,越具体越好。

## 配置

`config.json` —— 配置样例,拷到 `~/.ridge/config.json`(或用 `RIDGE_CONFIG` 指定路径)。env 覆盖此处;**密钥只走 `RIDGE_API_KEY` 环境变量,不进 config**。

键:

| 键 | 说明 |
|---|---|
| `provider` | `openai`(兼容端点)/ `anthropic` |
| `model` | 模型名 |
| `base_url` | OpenAI 兼容端点;anthropic 走 `https://api.anthropic.com/v1` |
| `budget_tokens` | 预算,`0` = 不限;超预算熔断停机 |
| `skills_dir` | 技能目录,不填默认 `~/.ridge/skills` |
| `skip_danger` | `true` = 工具自动放行不问 `[y/N]`(灾难命令仍拦) |
| `mcp` | 要并接的 MCP server 数组(见下) |

启动后也能在 REPL 里 **`/config`** 看配置、**`/config set <key> <value>`** 持久化上表标量键(改完重启生效);两种方式并存,直接编辑文件同样有效。

## MCP(接万物)

在 `config.json` 的 `mcp` 数组里加一段即多一批工具(见样例里的 AnySearch 例子)。零改源码。
