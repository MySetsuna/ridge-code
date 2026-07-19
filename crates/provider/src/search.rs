use super::ProviderError;
use std::time::Duration;

/// 抓文本(GET)的最小接缝:网页/JSON 都走它,便于测试替身注入。
#[async_trait::async_trait]
pub trait WebFetch: Send + Sync {
    async fn get_text(&self, url: &str) -> Result<String, ProviderError>;
}

/// 生产用真实抓取器(reqwest,带浏览器 UA + 超时,躲最基础的反爬、防卡死)。
pub struct ReqwestFetch {
    client: reqwest::Client,
}
impl ReqwestFetch {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .user_agent(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                     (KHTML, like Gecko) Chrome/122.0 Safari/537.36",
            )
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_default();
        Self { client }
    }
}
impl Default for ReqwestFetch {
    fn default() -> Self {
        Self::new()
    }
}
#[async_trait::async_trait]
impl WebFetch for ReqwestFetch {
    async fn get_text(&self, url: &str) -> Result<String, ProviderError> {
        let resp = self.client.get(url).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(format!("http {status}").into());
        }
        Ok(text)
    }
}

/// 网络环境。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetEnv {
    /// 能直连国际网络(裸连或带 VPN/代理)。
    International,
    /// 受限(GFW 内,连不到国际端点)。
    Restricted,
}

/// 探测能否直连国际网络:**并发**探两个国际端点(Google / gstatic 的 `generate_204`),
/// 每个 3s 超时,**任一通** → International;都不通 → Restricted。比单探针更抗抖动、收敛更快
/// (单个端点被限速/抽风不至于误判),且 3s 上限避免卡住 15s 的 HTTP 超时。
/// ponytail: 2 探针够用;captive-portal(返 200 假页)仍可能误判,升级路径 = 校验响应体特征。
pub async fn detect_net(fetch: &dyn WebFetch) -> NetEnv {
    async fn reachable(fetch: &dyn WebFetch, url: &str) -> bool {
        let timed =
            tokio::time::timeout(std::time::Duration::from_secs(3), fetch.get_text(url)).await;
        matches!(timed, Ok(Ok(_)))
    }
    let (a, b) = tokio::join!(
        reachable(fetch, "https://www.google.com/generate_204"),
        reachable(fetch, "https://www.gstatic.com/generate_204"),
    );
    if a || b {
        NetEnv::International
    } else {
        NetEnv::Restricted
    }
}

/// 该环境的**首选**搜索引擎名(展示/审计用;实际是引擎链的第一个)。
pub fn engine_for(env: NetEnv) -> &'static str {
    engines(env).first().map(|e| e.name).unwrap_or("none")
}

/// 一个**无 key** 搜索引擎:query(已 urlencode)→ 请求 URL,HTML → 结果解析。
struct Engine {
    name: &'static str,
    url: fn(&str) -> String,
    parse: fn(&str) -> Vec<SearchResult>,
}

fn ddg_url(q: &str) -> String {
    format!("https://html.duckduckgo.com/html/?q={q}")
}
fn bing_url(q: &str) -> String {
    format!("https://www.bing.com/search?q={q}")
}
fn bing_cn_url(q: &str) -> String {
    format!("https://cn.bing.com/search?q={q}")
}

/// 按网络环境给出**有序引擎链**:首选打头,其余作 fallback(某引擎改版/抽风/被限流 →
/// 自动换下一个)。**全是无 key 直抓 HTML,不依赖任何第三方付费 API**(Brave/Tavily 等)。
fn engines(env: NetEnv) -> &'static [Engine] {
    const INTL: &[Engine] = &[
        Engine {
            name: "duckduckgo",
            url: ddg_url,
            parse: parse_duckduckgo,
        },
        Engine {
            name: "bing",
            url: bing_url,
            parse: parse_bing,
        },
    ];
    const CN: &[Engine] = &[Engine {
        name: "bing-cn",
        url: bing_cn_url,
        parse: parse_bing,
    }];
    match env {
        NetEnv::International => INTL,
        NetEnv::Restricted => CN,
    }
}

/// 一条搜索结果。
#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// 搜索:按 env 取**引擎链**,**依次**尝试直到拿到非空结果(每条去重、裁 top 8)。
/// 某引擎报错/返回空 → 自动落到下一个;全失败才返回错(全空则返回空列表)。
/// ponytail: 全程无 key 直抓 HTML,靠多引擎 fallback 抗单点失效;仍怕全体同时改版,
/// 升级路径 = 往 [`engines`] 里加更多无 key 引擎(Mojeek / SearXNG 公共实例),**不接付费 API**。
pub async fn web_search(
    fetch: &dyn WebFetch,
    query: &str,
    env: NetEnv,
) -> Result<Vec<SearchResult>, ProviderError> {
    let q = urlencode(query);
    let mut last_err = None;
    for eng in engines(env) {
        match fetch.get_text(&(eng.url)(&q)).await {
            Ok(html) => {
                let mut results = (eng.parse)(&html);
                // 按 URL 去重(保序):同一结果被引擎重复列出时只留一条。
                let mut seen = std::collections::HashSet::new();
                results.retain(|r| seen.insert(r.url.clone()));
                results.truncate(8);
                if !results.is_empty() {
                    return Ok(results);
                }
            }
            Err(e) => last_err = Some(e),
        }
    }
    last_err.map_or(Ok(Vec::new()), Err)
}

/// 抓一个网页并抽成**可读正文**(供 RAG:web_search 拿链接 → fetch_url 读正文 → 据原文作答)。
pub async fn fetch_url(fetch: &dyn WebFetch, url: &str) -> Result<String, ProviderError> {
    let html = fetch.get_text(url).await?;
    Ok(html_to_text(&html))
}

/// HTML → 正文:先删 script/style 等整块,块级结束标签转换行,再去标签、压缩空白、截断防爆。
/// ponytail: 非 readability 级正文抽取,够喂模型;升级路径 = 接 readability/dom 解析。
fn html_to_text(html: &str) -> String {
    let mut buf = strip_blocks(html, &["script", "style", "noscript", "head", "svg"]);
    for tag in [
        "</p>", "</div>", "</li>", "</h1>", "</h2>", "</h3>", "</h4>", "</tr>", "<br>", "<br/>",
        "<br />",
    ] {
        buf = buf.replace(tag, "\n");
    }
    let text = strip_tags(&buf); // 去剩余标签 + 解实体(保留我插入的 \n)
    let mut out = String::new();
    let mut blank = false;
    for line in text.lines() {
        let t = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if t.is_empty() {
            if !blank {
                out.push('\n');
            }
            blank = true;
        } else {
            out.push_str(&t);
            out.push('\n');
            blank = false;
        }
    }
    let out = out.trim();
    if out.chars().count() > 4000 {
        let head: String = out.chars().take(4000).collect();
        format!("{head}\n…(正文截断)")
    } else {
        out.to_string()
    }
}

/// 删掉 `<tag ...>…</tag>` 整块(ASCII 大小写不敏感,字节布局不变→索引安全)。无闭合则删到结尾。
fn strip_blocks(html: &str, tags: &[&str]) -> String {
    let mut s = html.to_string();
    for tag in tags {
        let open = format!("<{tag}");
        let close = format!("</{tag}>");
        loop {
            let lower = s.to_ascii_lowercase(); // ASCII fold 不改字节长度,索引对 s 有效
            let Some(a) = lower.find(&open) else { break };
            let end = match lower[a..].find(&close) {
                Some(rel) => a + rel + close.len(),
                None => s.len(),
            };
            s.replace_range(a..end, " ");
        }
    }
    s
}

/// 解析 DuckDuckGo html 端点:结果锚点 `class="result__a"` + 摘要 `class="result__snippet"`。
fn parse_duckduckgo(html: &str) -> Vec<SearchResult> {
    let mut out = Vec::new();
    for chunk in html.split("class=\"result__a\"").skip(1) {
        // chunk 形如: ` href="//duckduckgo.com/l/?uddg=...">Title</a> ... result__snippet ...>Snippet</a>`
        let href = attr(chunk, "href=\"").unwrap_or_default();
        // 跳过广告位:DuckDuckGo 的赞助结果走 `/y.js?ad_domain=…` 跳转,不是自然结果。
        if href.contains("/y.js") || href.contains("ad_domain=") {
            continue;
        }
        let title = between(chunk, ">", "</a>")
            .map(strip_tags)
            .unwrap_or_default();
        let url = clean_ddg_url(&href);
        let snippet = chunk
            .find("result__snippet")
            .and_then(|p| between(&chunk[p..], ">", "</a>"))
            .map(strip_tags)
            .unwrap_or_default();
        if !title.is_empty() && !url.is_empty() {
            out.push(SearchResult {
                title,
                url,
                snippet,
            });
        }
    }
    out
}

/// 解析 Bing 静态 HTML:每条结果 `<li class="b_algo">`,标题在 `<h2><a href>`,摘要在 `<p>`。
fn parse_bing(html: &str) -> Vec<SearchResult> {
    let mut out = Vec::new();
    for chunk in html.split("class=\"b_algo\"").skip(1) {
        let Some(h2) = chunk.find("<h2") else {
            continue;
        };
        let url = attr(&chunk[h2..], "href=\"").unwrap_or_default();
        let title = between(&chunk[h2..], ">", "</a>")
            .map(strip_tags)
            .unwrap_or_default();
        let snippet = chunk
            .find("<p")
            .and_then(|p| between(&chunk[p..], ">", "</p>"))
            .map(strip_tags)
            .unwrap_or_default();
        if !title.is_empty() && !url.is_empty() {
            out.push(SearchResult {
                title,
                url,
                snippet,
            });
        }
    }
    out
}

/// 取 `hay` 中 `start` 之后到 `end` 之前的片段(找不到 → None)。
fn between<'a>(hay: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let i = hay.find(start)? + start.len();
    let rest = &hay[i..];
    let j = rest.find(end)?;
    Some(&rest[..j])
}

/// 取 `key`(如 `href="`)后到下一个 `"` 之前的属性值。
fn attr(hay: &str, key: &str) -> Option<String> {
    between(hay, key, "\"").map(|s| s.to_string())
}

/// DuckDuckGo 的 href 是跳转链接 `//duckduckgo.com/l/?uddg=<编码真实 URL>&rut=...` —— 解出真实 URL。
fn clean_ddg_url(href: &str) -> String {
    let h = href.replace("&amp;", "&");
    if let Some(v) = h.split("uddg=").nth(1) {
        return percent_decode(v.split('&').next().unwrap_or(v));
    }
    if let Some(rest) = h.strip_prefix("//") {
        return format!("https://{rest}");
    }
    h
}

/// 去掉 `<...>` 标签 + 解一点常见 HTML 实体。ponytail: 非完整 HTML 解析,够抽标题/摘要用。
fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&#39;", "'")
        .replace("&quot;", "\"")
        .replace("&nbsp;", " ")
        .trim()
        .to_string()
}

/// 极简 percent-encode(RFC3986 unreserved 之外全转义)。
fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 极简 percent-decode(`%XX` → 字节,`+` → 空格)。
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Some(b) = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 记录被请求的 URL、按 URL 回不同 HTML 的假抓取器(不联网)。
    struct FakeFetch {
        duck: String,
        bing: String,
        probe_ok: bool,
    }
    #[async_trait::async_trait]
    impl WebFetch for FakeFetch {
        async fn get_text(&self, url: &str) -> Result<String, ProviderError> {
            if url.contains("generate_204") {
                return if self.probe_ok {
                    Ok(String::new())
                } else {
                    Err("blocked".into())
                };
            }
            if url.contains("duckduckgo") {
                Ok(self.duck.clone())
            } else if url.contains("bing") {
                Ok(self.bing.clone())
            } else {
                Err("unexpected url".into())
            }
        }
    }

    fn fake() -> FakeFetch {
        FakeFetch {
                duck: r#"<a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.rust-lang.org%2F&amp;rut=x">Rust 语言</a>
                    <a class="result__snippet" href="//x">A language empowering everyone.</a>"#.to_string(),
                bing: r#"<li class="b_algo"><h2><a href="https://doc.rust-lang.org/book/">The Rust Book</a></h2><p class="b_lineclamp2">学 Rust 的书。</p></li>"#.to_string(),
                probe_ok: true,
            }
    }

    #[tokio::test]
    async fn detect_net_reads_probe() {
        let mut f = fake();
        f.probe_ok = true;
        assert_eq!(detect_net(&f).await, NetEnv::International);
        f.probe_ok = false;
        assert_eq!(detect_net(&f).await, NetEnv::Restricted);
    }

    #[tokio::test]
    async fn detect_net_international_if_any_probe_ok() {
        // google 探针失败、gstatic 成功 → 仍判直连(多探针 OR 语义)。
        struct MixedProbe;
        #[async_trait::async_trait]
        impl WebFetch for MixedProbe {
            async fn get_text(&self, url: &str) -> Result<String, ProviderError> {
                if url.contains("gstatic") {
                    Ok(String::new())
                } else {
                    Err("blocked".into())
                }
            }
        }
        assert_eq!(detect_net(&MixedProbe).await, NetEnv::International);
    }

    #[tokio::test]
    async fn international_uses_duckduckgo_and_decodes_url() {
        let r = web_search(&fake(), "rust 语言", NetEnv::International)
            .await
            .unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].title, "Rust 语言");
        assert_eq!(r[0].url, "https://www.rust-lang.org/"); // uddg 解码
        assert!(r[0].snippet.contains("empowering"));
    }

    #[tokio::test]
    async fn restricted_uses_bing() {
        let r = web_search(&fake(), "rust", NetEnv::Restricted)
            .await
            .unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].title, "The Rust Book");
        assert_eq!(r[0].url, "https://doc.rust-lang.org/book/");
        assert!(r[0].snippet.contains("Rust"));
    }

    #[tokio::test]
    async fn fetch_url_extracts_readable_text() {
        struct PageFetch;
        #[async_trait::async_trait]
        impl WebFetch for PageFetch {
            async fn get_text(&self, _url: &str) -> Result<String, ProviderError> {
                Ok(
                    r#"<html><head><title>T</title><style>.x{color:red}</style></head>
                        <body><script>alert('nope')</script>
                        <h1>标题</h1><p>第一段正文。</p><p>第二段正文。</p></body></html>"#
                        .to_string(),
                )
            }
        }
        let text = fetch_url(&PageFetch, "https://x.com").await.unwrap();
        assert!(
            text.contains("标题") && text.contains("第一段正文"),
            "{text}"
        );
        assert!(
            !text.contains("alert") && !text.contains("color:red"),
            "脚本/样式应被剔除: {text}"
        );
        // 块级标签转了换行 → 段落分行,不会糊成一坨。
        assert!(text.lines().count() >= 3, "应按段分行: {text}");
    }

    #[test]
    fn parse_duckduckgo_skips_ads() {
        let html = r#"
                <a class="result__a" href="//duckduckgo.com/y.js?ad_domain=spam.com&ad_provider=bing">广告</a>
                <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Freal.com%2F">真实结果</a>"#;
        let r = parse_duckduckgo(html);
        assert_eq!(r.len(), 1, "广告位应被过滤: {r:?}");
        assert_eq!(r[0].url, "https://real.com/");
    }

    #[tokio::test]
    async fn web_search_falls_back_when_primary_empty() {
        // 首选 DuckDuckGo 返回无结果 → 自动落到 Bing(无 key fallback,不依赖付费 API)。
        struct FallbackFetch;
        #[async_trait::async_trait]
        impl WebFetch for FallbackFetch {
            async fn get_text(&self, url: &str) -> Result<String, ProviderError> {
                if url.contains("duckduckgo") {
                    Ok("<html>no results here</html>".to_string())
                } else if url.contains("bing") {
                    Ok(r#"<li class="b_algo"><h2><a href="https://b.com/">命中</a></h2><p>摘要</p></li>"#.to_string())
                } else {
                    Err("unexpected".into())
                }
            }
        }
        let r = web_search(&FallbackFetch, "q", NetEnv::International)
            .await
            .unwrap();
        assert_eq!(r.len(), 1, "DDG 空 → 应落到 Bing: {r:?}");
        assert_eq!(r[0].url, "https://b.com/");
    }

    #[tokio::test]
    async fn web_search_dedupes_by_url() {
        struct DupFetch;
        #[async_trait::async_trait]
        impl WebFetch for DupFetch {
            async fn get_text(&self, _url: &str) -> Result<String, ProviderError> {
                // 同一 URL 出现两次的 DDG 结果。
                Ok(r#"<a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fa.com%2F">A1</a>
                        <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fa.com%2F">A2</a>"#.to_string())
            }
        }
        let r = web_search(&DupFetch, "q", NetEnv::International)
            .await
            .unwrap();
        assert_eq!(r.len(), 1, "重复 URL 应去重: {r:?}");
        assert_eq!(r[0].url, "https://a.com/");
    }

    #[test]
    fn strip_blocks_removes_script_case_insensitive() {
        let out = strip_blocks("<BODY>keep<SCRIPT>drop</SCRIPT>keep2</BODY>", &["script"]);
        assert!(out.contains("keep") && out.contains("keep2") && !out.contains("drop"));
    }

    #[test]
    fn urlencode_and_decode_roundtrip() {
        assert_eq!(urlencode("a b&c"), "a%20b%26c");
        assert_eq!(percent_decode("https%3A%2F%2Fx.com%2F"), "https://x.com/");
        assert_eq!(engine_for(NetEnv::International), "duckduckgo");
        assert_eq!(engine_for(NetEnv::Restricted), "bing-cn");
    }
}
