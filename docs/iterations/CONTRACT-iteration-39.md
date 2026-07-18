# CONTRACT · iteration-39 —— Prompt 模板自定义命令 + Skills 显示为命令

> maker = 用户需求「支持用户自定义安装命令(澄清=Prompt 模板命令),显示 Skills 作为命令」。checker = 本文正确性门禁。价值门禁不适用(用户明确需求)。

## 目标
1. `~/.ridge/commands/*.md` 各成一个 `/名字` 命令:name=文件名,可选 frontmatter `description`/`desc`,body=Prompt 模板(`$ARGS`=调用参数)。
2. 每个已加载 Skill 也暴露为 `/skill-name` 命令(调用即把 skill body 注入为任务)。
3. `/name [args]` 调用 → 展开 body(`$ARGS`→args;无占位且有参→追加)→ **以任务身份**喂给 agent。
4. 命令名进斜杠补全;`/commands` 列出全部。

## 设计(最小面)
- **lib.rs 纯核**:`SlashCommand{name,description,body}`、`parse_command_md(text,name)`(复用 frontmatter 切分)、`expand_command(body,args)`、`load_commands(dir,skills)`(文件优先于同名 skill)、`resolve_command`。Config 加 `commands_dir`。
- **main.rs**:`load_configured_commands(cfg,skills)`(env `RIDGE_COMMANDS_DIR`>config>默认),TTY 分支从同一 `&skills` 派生后移交 skills 给图;传入 `tui::run`。
- **tui.rs**:`run` 加 `commands` 参 + `set_dynamic_commands`(`DYNAMIC_COMMANDS: OnceLock`,并入 `build_popup` 补全);`run_command` 加 `commands` 参,末尾 `/` 分支改「解析 `/name args`→`resolve_command`→`expand_command`→置 `ui.run_task`」,内置命令仍先匹配不被 shadow;`ui.run_task: Option<String>` 由主环唯一提交点取走起任务(普通输入照旧);`/commands` 列表。
- **不改**:引擎、登录、Panel、排队;headless 无斜杠命令不涉及。

## 边界(不做)
- Shell 运行命令(`/deploy`→shell)—— 用户本轮选 Prompt 模板,不做。
- 命令参数占位除 `$ARGS` 外的 `$1/$2` —— YAGNI。
- Hook 系统 —— iter-40。

## 确定性验收信号
门禁全 exit 0。新增:
- `command_parse_and_expand`:frontmatter desc/desc简写 + body;`$ARGS` 替换;无占位有参追加、无参原样;无 frontmatter 全文即 body。
- `load_commands_merges_files_and_skills`:文件命令 + skill 命令合并,**文件优先于同名 skill**,`resolve_command` 命中/未命中。
- 既有 `slash_popup_lists_all_and_filters` / `filter_prefix`「/co」断言同步(新增 `/commands`)。

## 停机
单轮;连续 2 轮验收不过 → 报告。收尾:回写 ARCHITECTURE、报告、提交带 `iter-39`。NLM 源与本地安装在三迭代收尾统一做。
