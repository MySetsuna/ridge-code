---
id: L3-AGENT-KNOWLEDGE-001
level: L3
parent: L2-AGENT-001
title: Declarative skills and read-only sub-agents
status: VALID
code_targets:
  - crates/agent/src/brain.rs
  - crates/agent/src/knowledge.rs
  - crates/agent/src/orchestrate.rs
  - crates/agent/src/graph.rs
  - crates/agent/src/exec.rs
test_targets:
  - crates/agent/src/brain.rs
  - crates/agent/src/knowledge.rs
  - crates/agent/src/orchestrate.rs
  - crates/agent/src/graph.rs
public_interface:
  - agent::load_skills
  - agent::load_skill_catalog
  - agent::discover_skill_scopes
  - agent::load_commands_from_catalog
  - agent::merge_skills
  - agent::load_agents
  - agent::load_commands
  - agent::load_project_rules
  - ridgecode Skills system-prompt injection
---

# Declarative skills and read-only sub-agents

Skill discovery now uses deterministic high-to-low scopes: `RIDGE_SKILLS_DIR`
(env), configured `skills_dir`, cwd `.ridge/skills`/`.agents/skills`, repository
`.ridge/skills`/`.agents/skills`, user `~/.ridge/skills`, then built-ins. Missing
scope directories are harmless; cwd/repository duplicates are removed by
normalized path. All scopes share one 256-candidate cap. Only winning bodies
enter the system prompt; shadowed candidates remain available through
qualified slash aliases such as `/user:name` or `/repo:name`, and collisions
report source labels/paths without body text. Hot reload and progressive body
loading remain out of scope.

Skills 与 agent 定义来自带 frontmatter 的 Markdown；用户定义可覆盖同名内置定义，
`merge_skills` 按来源优先级去重，避免重复注入。Skill 发现最多保留 256 份、每份
128 KiB；flat commands/agents 目录亦只保留路径字典序前 256 份，每份 64 KiB，重复
agent name 由最早路径确定性胜出。全局及 cwd 项目规则每文件最多读取 128 KiB；超限
时仅取 UTF-8 安全头尾并写入显式 marker，而非先把任意大文件读进内存。注入阶段再以
24 Ki Unicode 字符和 6000 估算 token 双硬限封住总 prompt。
普通小集合逐字不变；超限节以 Unicode-safe 头尾摘要和显式 marker 收束，末尾的
`项目规则` 预留预算并仍置末尾，不能被前序巨型 Skill 静默挤掉。sub-agent 只授予
`read_file`/`search` 等只读工具，主 agent 通过 dispatch 获取精炼结论，避免复制完整上下文。
