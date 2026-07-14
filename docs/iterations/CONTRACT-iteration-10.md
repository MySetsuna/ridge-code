# CONTRACT —— Iteration 10:工业级 web 能力(Research 闭环)

- **开工时间戳**: 2026-07-14
- **里程碑**: web 从「能搜」到「能读能引」—— 搜索 → 取正文 → RAG 总结的完整研究闭环
- **依据**: `docs/iterations/2026-07-14-notebooklm-guidance-09.md`(NotebookLM + 对抗评审)+ 用户 steer(web 达到工业级水准)

## 目标(End State)

在 Iter09 的 `web_search`(探网络 + 换引擎)之上,让 agent **不止拿到链接,还能读正文并据原文推理**;搜索从脆弱的 HTML 抓取升级到**可配 API key 的稳定后端**(无 key 自动回落 HTML);网络探测更鲁棒。

## 任务与验收信号

| 优先级 | 任务 | 确定性验收信号 | 状态 |
|---|---|---|---|
| **P0** | **`fetch_url(url)`**:抓网页 → 去脚本/样式/标签 → 返回正文纯文本(截断防爆),喂模型做 RAG。复用 `WebFetch` + `strip_tags` | 单测:fake html(含 `<script>`/`<style>`/标签)→ 返回干净正文、无标签;**live**:抓一个真实公开页(如 example.com / rust 文档)→ 正文非空且含预期关键词 | ⬜ |
| **P1** | **API key 搜索后端**(Brave 或 Tavily;env `RIDGE_SEARCH_BACKEND` + `RIDGE_SEARCH_KEY`;**无 key 回落**现有 HTML 引擎) | 单测:fake JSON 响应 → 结构化 `SearchResult`(含 score/排序);无 key → 走 `web_search` HTML 路径。**live 待用户提供搜索 key** | ⬜ |
| **P1** | **网络探测更稳**:多探针并发取最快成功者 + 结果 TTL 缓存(而非永久缓存) | 单测:3 个 fake 探针(1 快 Ok / 2 慢或 Err)→ 判 International;同一 `NetProbe` 在 TTL 内不重探(计数验证) | ⬜ |
| **P2** | `~/.ridge/config.toml`(provider/model/预算/**搜索与 LLM key**/多 `[[mcp]]`/skills;env 覆盖)+ 多 MCP 并接(现有 `StdioTransport`) | 单测:解析含 2 个 `[[mcp]]` + 搜索 key 的 config → 对应 spec/设置;起 2 假 server → `list_tools` 并集、命名空间不撞 | ⬜ |
| **P2** | 搜索结果**去重 + 排序**(按引擎给的相关性;同 URL 物理合并) | 单测:含重复 URL 的结果集 → 去重后唯一、按 score 降序 | ⬜ |

## 「工业级 web + 媲美 Claude Code」Definition of Done

- [x] 联网搜索 + 网络环境感知 + 引擎切换 —— Iter09
- [ ] **完整 Research 闭环**:`web_search` → `fetch_url`(正文)→ RAG 总结 → 写码;**信原文不信摘要**(本轮 P0)
- [ ] **混合搜索**:环境感知 + API 后端优先 + 无 key 回落 HTML(本轮 P1)
- [ ] **探测鲁棒**:多探针 + TTL,不因单探针误判失效(本轮 P1)
- [ ] **生产级配置**:LLM/搜索 key 在 config.toml/env 统一管理(本轮 P2)
- [ ] 插件式扩展:config 加 MCP 不改源码即见新工具(本轮 P2)
- [ ] 引用编号(可选打磨,**非硬门槛** —— 引用正确性不可确定性机检)
- [ ] 批量工程:多文件 EditBuffer + 汇总 diff 一次确认(顺延自 CONTRACT-09,backlog)

## 已知限制(Beta 可先发,对抗评审后明确)

- **搜索 API key 的 live 实测**:手上只有智谱 GLM 的 key,无 Brave/Serper/Tavily key → API 后端只能假抓取器单测,live 待用户提供。
- **重量级沙箱**(Docker/gVisor;WASM 不适合 shell)、**rmcp 替换自写 stdio**(自写已连真实 server,可选升级)、**自动化触发器**、kill-9 REPL 恢复、子任务并行 = backlog / 已知限制。

## 边界

不破坏现有 55 测试 + clippy/fmt 干净;**key/敏感不写 trace、不写日志**(搜索 key 同 LLM key 待遇);`fetch_url` 正文截断防爆上下文;config 解析失败降级到 env;不新增重依赖(HTML/JSON 解析优先复用现有 reqwest/serde,HTML 仍纯 std)。
