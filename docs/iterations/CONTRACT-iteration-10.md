# CONTRACT —— Iteration 10:工业级 web 能力(Research 闭环)

- **开工时间戳**: 2026-07-14
- **里程碑**: web 从「能搜」到「能读能引」—— 搜索 → 取正文 → RAG 总结的完整研究闭环
- **依据**: `docs/iterations/2026-07-14-notebooklm-guidance-09.md`(NotebookLM + 对抗评审)+ 用户 steer(web 达到工业级水准)

## 目标(End State)

在 Iter09 的 `web_search`(探网络 + 换引擎)之上,让 agent **不止拿到链接,还能读正文并据原文推理**;搜索从脆弱的 HTML 抓取升级到**可配 API key 的稳定后端**(无 key 自动回落 HTML);网络探测更鲁棒。

## 任务与验收信号

| 优先级 | 任务 | 确定性验收信号 | 状态 |
|---|---|---|---|
| **P0** | **`fetch_url(url)`**:抓网页 → 去脚本/样式/标签 → 返回正文纯文本(截断防爆),喂模型做 RAG。复用 `WebFetch` + `strip_tags` | fake html → 干净正文;**live** example.com 正文非空含关键词 | ✅ `0df7a09` |
| ~~P1~~ | ~~**API key 搜索后端**(Brave/Tavily)~~ → **用户驳回付费 API**。改为**无 key 多引擎 fallback 链**:某引擎报错/空 → 自动落下一个(International=[duckduckgo, bing]、Restricted=[bing-cn]) | 单测:DDG 返回空 → 落到 Bing 拿到结果;GLM live DDG 正常 | ✅ `a7b632d` |
| **P1** | **网络探测更稳**:多探针并发 | 单测:一探针 Err 另一 Ok → International(3s 超时上限) | ✅ `c91640b`(TTL 缓存 = YAGNI,会话级永久缓存够用) |
| **P2** | `~/.ridge/config.toml`(provider/model/预算/多 `[[mcp]]`/skills;env 覆盖)+ 多 MCP 并接(现有 `StdioTransport`) | 单测:解析含 2 个 `[[mcp]]` 的 config → 对应 spec/设置;起 2 假 server → `list_tools` 并集、命名空间不撞 | ⬜ 下一步 |
| **P2** | 搜索结果**去重**(同 URL 物理合并) | 单测:含重复 URL → 去重后唯一 | ✅ `ec41364` |

## 「工业级 web + 媲美 Claude Code」Definition of Done

- [x] 联网搜索 + 网络环境感知 + 引擎切换 —— Iter09
- [x] **完整 Research 闭环**:`web_search` → `fetch_url`(正文)→ 据原文作答 —— `0df7a09`
- [x] **无 key 稳健搜索**:环境感知 + **多引擎 fallback**(不接付费 API)+ 去重 —— `a7b632d`/`ec41364`
- [x] **探测鲁棒**:多探针并发(TTL = YAGNI) —— `c91640b`
- [ ] **生产级配置**:LLM key/多 MCP 在 config.toml/env 统一管理(下一步 P2)
- [ ] 插件式扩展:config 加 MCP 不改源码即见新工具(下一步 P2)
- [ ] 批量工程:多文件 EditBuffer + 汇总 diff 一次确认(顺延自 CONTRACT-09,backlog)

## 已知限制(Beta 可先发,对抗评审后明确)

- **付费搜索 API(Brave/Tavily)**:用户明确不接。搜索靠**无 key 多引擎 fallback** 抗单点失效;若未来所有引擎同时改版,再往 `engines()` 加无 key 引擎(Mojeek/SearXNG),仍不接付费 API。
- **重量级沙箱**(Docker/gVisor;WASM 不适合 shell)、**rmcp 替换自写 stdio**(自写已连真实 server,可选升级)、**自动化触发器**、kill-9 REPL 恢复、子任务并行 = backlog / 已知限制。

## 边界

不破坏现有 55 测试 + clippy/fmt 干净;**key/敏感不写 trace、不写日志**(搜索 key 同 LLM key 待遇);`fetch_url` 正文截断防爆上下文;config 解析失败降级到 env;不新增重依赖(HTML/JSON 解析优先复用现有 reqwest/serde,HTML 仍纯 std)。
