---
name: agent-creator
description: 当用户想新建或定制一个 sub-agent 时,按本规范把 agent 定义写成 md 文件。
---
sub-agent 是一段带 frontmatter 的 Markdown,放在 agents 目录(默认 `~/.ridge/agents/<name>.md`,或环境变量 `RIDGE_AGENTS_DIR` 指定的目录)。用 write_file 创建,格式:

```
---
name: <短横线小写名>
description: <一句话:它干什么、何时该派它——主 agent 靠这句决定何时调用,务必写清触发场景>
provider: <可选,引用 config.json 里 providers 的档案名(如 fast);省略则用主模型>
tools: read_file, search
---
<正文 = 该 sub-agent 的 system prompt:定角色、说明输入、规定输出格式>
```

约束:sub-agent **只读**(仅 read_file / search),用于检索、探索、审查,不能改文件或跑命令——写文件是主 agent 的活。`tools` 省略则给全部只读工具。写好后告诉用户:放同名文件即可覆盖内置的同名 agent。
