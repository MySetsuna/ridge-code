---
description: 分析本仓库,生成或完善项目级 AGENTS.md(每会话自动注入的项目规则)
---
分析当前仓库,生成或完善项目级 AGENTS.md(RidgeCode/Codex 等 agent 每会话自动注入的项目规则)。

步骤:
1. 侦察:read_file 读 README 与构建清单(Cargo.toml / package.json / pyproject.toml / go.mod 等,存在哪个读哪个)、CI 配置;search 探目录结构与测试布局。已有 AGENTS.md 或 CLAUDE.md 先读。
2. 提炼——只写对 agent 干活有用、且从单个文件看不出来的信息:
   - 项目一句话定位(是什么、给谁用)
   - 常用命令:构建 / 测试 / lint(取自清单或 CI,勿编造未验证的命令)
   - 架构要点:模块边界、关键不变量、改动时易踩的坑
   - 仓库约定:语言、代码风格、分支模型
3. 用 write_file(新建)或 edit_file(已存在,保留正确内容只补缺纠错)写 AGENTS.md。全文 ≤60 行,用仓库主要语言书写,勿罗列文件树,勿复述代码。

写毕一句话报告:新建还是更新、包含哪几节。
