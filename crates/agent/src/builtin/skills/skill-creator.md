---
name: skill-creator
description: 当用户想新建一个 skill(领域知识或操作规范)时,按本规范把 SKILL.md 写到技能目录。
---
skill 是注入 system prompt 的领域知识,放在 `<skills_dir>/<name>/SKILL.md`(默认 `~/.ridge/skills/`,或 config.json 的 skills_dir / 环境变量 `RIDGE_SKILLS_DIR`)。用 write_file 创建,格式:

```
---
name: <名>
description: <一句话:这块知识/规范是什么、何时适用>
---
<正文 = 要注入的知识、步骤、约定,写清"怎么做">
```

skill 与 agent 之别:skill 是**被动注入**给主 agent 的知识(改变它怎么想、怎么做);agent 是**独立上下文的只读子任务执行者**(被派去搜集或审查,回结论)。要"教主 agent 一套做法"用 skill;要"派人只读地查一件事"用 agent。
