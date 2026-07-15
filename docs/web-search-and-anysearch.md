# Web 搜索:内置方案 vs AnySearch(调研结论)

- **时间戳**: 2026-07-14
- **问题**: AnySearch 能否作为 RidgeCode 网页工具的**更优选择**(它输出结构化网页搜索结果)?

## TL;DR — 两个都留,各司其职

| | 内置 `web_search`/`fetch_url` | AnySearch(MCP server) |
|---|---|---|
| 依赖 | **零**(纯 Rust + reqwest,不装任何东西) | Node/npx + 联网到 anysearch.com |
| key | 不需要 | 匿名可用 / 可选免费 key(1000 次/天) |
| 输出 | 抓 HTML 解析(脆,引擎改版/广告位会脏) | **结构化**(title/url/context + JSON) |
| 能力 | search + fetch 正文 | search / **batch_search** / **extract** / **vertical 垂直域**(金融/学术/代码/法律/安全) |
| 接入 RidgeCode | 已内置 | **config.json 加一段 mcp,零改源码** |

**结论**:AnySearch 的**结构化输出确实更优**(内置抓 HTML 实测会混进 DuckDuckGo 广告位 `y.js` 脏链,已加过滤但结构化天生免疫),且有 batch/extract/垂直域等内置没有的能力。但它是**第三方服务 + 需 npx**;内置是**零依赖默认**。所以:**内置当默认(离线无 node 也能用)、AnySearch 当可选升级**,按 RidgeCode 的插件式扩展方式接入 —— **不写一行 Rust**。

## 关键验证:AnySearch = 一个 MCP server,config.json 即插即用(已实测)

AnySearch 提供官方 MCP server。RidgeCode 的 `~/.ridge/config.json` 多 MCP 支持(Iteration 10 P2)直接吃它,**零改源码**就多出 4 个工具。**实测通过**:

```jsonc
// ~/.ridge/config.json 的 mcp 数组里加一段:
{
  "mcp": [
    {
      "name": "anysearch",
      // ⚠ Windows 必须用 cmd /c 包一层 —— Rust 的 Command 不解析 npx.cmd,直接写 "npx" 会 "program not found"
      "cmd": "cmd",
      "args": ["/c", "npx", "-y", "mcp-remote", "https://api.anysearch.com/mcp"]
      // 类 Unix 直接:"cmd": "npx", "args": ["-y", "mcp-remote", "https://api.anysearch.com/mcp"]
      // 要更高额度:再加 --header "Authorization: Bearer <ANYSEARCH_API_KEY>"
    }
  ]
}
```

跑 `ridgecode` 后 stderr 打印 `[ridgecode] 已接入 1 个 MCP server`,模型即可用:
`anysearch__search` / `anysearch__extract` / `anysearch__batch_search` / `anysearch__get_sub_domains`(命名空间不撞内置工具)。

> 这也正好**印证了北极星**:加新能力 = 加一个 MCP 配置,不改 Rust 源码。RidgeCode 的降级也生效了 —— npx 缺失/连不上时,自动回落内置 `web_search`,不崩。

## 已知坑

- **Windows npx**:`cmd="npx"` 会 `spawn npx: program not found`(Rust `Command` 不走 shell、不补 `.cmd`)。用 `cmd="cmd", args=["/c","npx",…]` 包一层。
- **第三方依赖**:query 会发到 anysearch.com;服务可能限流/下线。内置直连 DuckDuckGo/Bing 不经第三方。
- 若要 RidgeCode **原生**接 Streamable-HTTP MCP(免 npx 代理),需给 mcp crate 加 HTTP 传输 —— 列为 backlog(当前 stdio + mcp-remote 已够用)。

## 来源
- AnySearch 官网 / MCP server:https://www.anysearch.com/ · https://github.com/anysearch-ai/anysearch-mcp-server
