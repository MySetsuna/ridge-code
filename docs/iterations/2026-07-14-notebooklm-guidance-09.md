# NotebookLM 指导归档 + 对抗评审 —— 针对 Iteration 09(即 Iteration 10 计划)

- **时间戳**: 2026-07-14
- **来源**: NotebookLM,基于《Ridge迭代报告 Iteration-09》(source `61ee9e52`)+ 全部来源(含 loop-engineering / MCP notes)
- **conversation_id**: `68791fb7-659a-4ad6-a86c-beb7ac694781`

## NotebookLM 给的 Iteration 10

- **P0** config.toml + 多 MCP(称「基础架构硬门槛」,多 API key 要统一持久化)
- **P1** API key 搜索后端(Brave/Serper/Tavily,结构化 JSON)
- **P2** `fetch_url` 网页正文抓取(RAG 闭环)
- **P3** 多文件批量编辑(顺延)
- **P4** 探测更稳(多探针 + TTL)
- **P5** 结果去重/排序/引用编号
- DoD 5 条:生产级配置管理、混合搜索(环境感知 + API 优先 + 无 key 回落 HTML)、完整 Research 闭环(search→fetch_url→RAG→写码)、批量工程效率、**确定性引用审计(每段解释带 [n] 编号 + 对应 trace 网页快照)**。

## 对抗评审(不全信 NotebookLM)

- ✅ **采纳**:`fetch_url`(RAG 闭环)、API key 搜索后端(env 配 key、无 key 回落 HTML)、探测更稳(多探针 + TTL)、结果去重/排序。都直击「工业级 web」。
- ⚠️ **重排 P0**:NotebookLM 把 config.toml 当 P0 硬门槛,理由是「API key 要持久化」。但 **API key 完全可走 env**(ridge 现有 `RIDGE_API_KEY` 等就是 env 模式),不阻塞。用户本轮目标是「**web 达到工业级**」,最高价值 + **可实测** 的是 `fetch_url`(搜到链接→取正文→RAG),而非配置 plumbing。→ **P0 改为 `fetch_url`**;config.toml 降为 P2 并行轨(key 先用 env,config 作为它的最终归宿一并做,但不卡交付)。
- ⚠️ **驳回「确定性引用审计」当 DoD 硬门槛**:无法**确定性**强制 LLM 每段都正确带 [n] 编号(引用正确性本身不可机检)—— 这是 LLM-as-judge 才能软评的东西,拿它当「确定性」验收信号自相矛盾。→ 引用编号列**可选打磨**,非硬门槛。
- ⚠️ **张冠李戴**:上条 DoD 的引用 `[10]` 是一份**无关的 awesome-MCP-servers 清单**(eleata-verify / bumpguard 等一堆第三方 server),被拿来「佐证」引用审计需求 —— 与 ridge 无关,忽略。
- 📝 **实测约束**:API key 后端**我手上只有智谱 GLM 的 key,没有 Brave/Serper/Tavily 的 key** → 只能用假抓取器单测其 JSON 解析 + 无 key 回落,**live 实测需用户提供搜索 key**。`fetch_url` 则可 live 实测(抓真实公开页)。

## 采纳后的 Iteration 10(见 CONTRACT-iteration-10)

P0 `fetch_url`(正文抓取 + 转纯文本,RAG 闭环,可 live 实测)→ P1 API key 搜索后端(env-first,无 key 回落 HTML,假抓取器单测)+ 探测更稳(多探针并发 + TTL)→ P2 config.toml + 多 MCP(key 的最终归宿)+ 结果去重排序。引用编号 = 可选打磨。多文件批量编辑仍在 backlog(顺延自 CONTRACT-09)。
