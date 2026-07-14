# samples —— 开箱即用的官方样例

RidgeCode 的核心承诺:**加能力 = 加一个 `SKILL.md` 或一段 MCP 配置,不改源码。** 这里是几个能直接抄的样例。

## Skills(声明式领域技能)

把 `skills/<name>/` 拷到 `~/.ridge/skills/<name>/`(或用 `RIDGE_SKILLS_DIR` 指到别处),启动 `ridgecode` 就会自动加载并注入 system prompt。

- `skills/researcher/SKILL.md` —— 调研:web_search → fetch_url → 据原文作答、带来源。
- `skills/rust-fixer/SKILL.md` —— 修 Rust:信编译器不信自述、精准/批量编辑、改完必复核。

`SKILL.md` = YAML frontmatter(`name` + `description`)+ 正文。`description` 写清「什么时候用」,越具体越好。

## 配置

`config.toml` —— 带注释的配置样例,拷到 `~/.ridge/config.toml`。provider/model/预算/多 `[[mcp]]`/skills 一处配;env 覆盖;**密钥只走 `RIDGE_API_KEY` 环境变量,不进 config**。

## MCP(接万物)

在 `config.toml` 里加 `[[mcp]]` 段即多一批工具(见 `config.toml` 里的 AnySearch 例子)。零改源码。
