use crate::exec::parse_edits;
use provider::ToolCall;

/// web_search 的观察结果:懒探测一次网络环境(缓存)→ 选引擎 → 搜 → 排版给模型看。
/// `fetch`/`net` 从 [`build_core`] 注入,测试可用假抓取器。
pub(crate) async fn web_search_obs(
    fetch: &dyn provider::search::WebFetch,
    net: &std::sync::OnceLock<provider::search::NetEnv>,
    call: &ToolCall,
) -> String {
    use provider::search::{detect_net, engine_for, web_search, NetEnv};
    let query = call
        .arguments
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if query.is_empty() {
        return "web_search error: 缺少 query".to_string();
    }
    // 网络环境只探一次,整个会话复用(ponytail: 进程级缓存;网络切换需重启)。
    let env = match net.get() {
        Some(e) => *e,
        None => {
            let e = detect_net(fetch).await;
            let _ = net.set(e);
            e
        }
    };
    let label = match env {
        NetEnv::International => "直连国际",
        NetEnv::Restricted => "受限(GFW 内)",
    };
    match web_search(fetch, query, env).await {
        Ok(rs) if rs.is_empty() => format!("网络:{label} · 引擎:{} · (无结果)", engine_for(env)),
        Ok(rs) => {
            let mut s = format!("网络:{label} · 引擎:{}\n", engine_for(env));
            for (i, r) in rs.iter().enumerate() {
                s.push_str(&format!(
                    "{}. {} — {}\n   {}\n",
                    i + 1,
                    r.title,
                    r.url,
                    r.snippet
                ));
            }
            s
        }
        Err(e) => format!("web_search error: {e}"),
    }
}

/// fetch_url 的观察结果:抓网页正文喂给模型(RAG 闭环的「读」)。`fetch` 从 [`build_core`] 注入。
pub(crate) async fn fetch_url_obs(
    fetch: &dyn provider::search::WebFetch,
    call: &ToolCall,
) -> String {
    let url = call
        .arguments
        .get("url")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if url.is_empty() {
        return "fetch_url error: 缺少 url".to_string();
    }
    match provider::search::fetch_url(fetch, url).await {
        Ok(text) if text.is_empty() => format!("(空正文) {url}"),
        Ok(text) => format!("正文 {url}:\n{text}"),
        Err(e) => format!("fetch_url error: {e}"),
    }
}

/// 给权限门一个**人类可读的预览**,而非生糊 JSON:让用户「看着 diff 批准」而非盲批。
/// edit_file → `-/+` diff;write_file → 路径+规模;run_shell → 命令原文;其余回落到参数。
pub fn preview_call(call: &ToolCall) -> String {
    let arg = |k: &str| call.arguments.get(k).and_then(|v| v.as_str()).unwrap_or("");
    match call.name.as_str() {
        "edit_file" => {
            let minus: String = arg("old_string")
                .lines()
                .map(|l| format!("\n    - {l}"))
                .collect();
            let plus: String = arg("new_string")
                .lines()
                .map(|l| format!("\n    + {l}"))
                .collect();
            format!("{}{}{}", arg("path"), minus, plus)
        }
        "write_file" => {
            let c = arg("contents");
            format!(
                "{} ({} 行, {} 字节)",
                arg("path"),
                c.lines().count(),
                c.len()
            )
        }
        "apply_edits" => {
            let edits = parse_edits(call);
            format!(
                "批量编辑 {} 处:\n{}",
                edits.len(),
                tools::edits_diff(&edits)
            )
        }
        "run_shell" => arg("cmd").to_string(),
        _ => call.arguments.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::*;

    /// fetch_url:抓网页 → 抽正文喂模型(RAG 的「读」),走假抓取器不联网。
    #[tokio::test]
    async fn fetch_url_obs_returns_page_text() {
        use provider::search::WebFetch;
        struct Page;
        #[async_trait::async_trait]
        impl WebFetch for Page {
            async fn get_text(&self, _url: &str) -> Result<String, provider::ProviderError> {
                Ok("<body><script>x()</script><p>正文内容在此。</p></body>".to_string())
            }
        }
        let call = ToolCall {
            id: "f".to_string(),
            name: "fetch_url".to_string(),
            arguments: serde_json::json!({"url": "https://ex.com"}),
        };
        let obs = fetch_url_obs(&Page, &call).await;
        assert!(
            obs.contains("正文内容在此") && !obs.contains("x()"),
            "{obs}"
        );

        let bad = ToolCall {
            id: "f2".to_string(),
            name: "fetch_url".to_string(),
            arguments: serde_json::json!({}),
        };
        assert!(fetch_url_obs(&Page, &bad).await.contains("缺少 url"));
    }

    /// web_search:探测网络环境 → 选引擎 → 排版结果,全程走假抓取器(不联网)。
    #[tokio::test]
    async fn web_search_obs_detects_env_and_picks_engine() {
        use provider::search::{NetEnv, WebFetch};

        // 探针失败 → Restricted → 该用 bing-cn;返回一条结果。
        struct RestrictedFetch;
        #[async_trait::async_trait]
        impl WebFetch for RestrictedFetch {
            async fn get_text(&self, url: &str) -> Result<String, provider::ProviderError> {
                if url.contains("generate_204") {
                    return Err("blocked".into());
                }
                assert!(url.contains("bing"), "受限环境应打 bing,实际:{url}");
                Ok(r#"<li class="b_algo"><h2><a href="https://ex.com/">标题</a></h2><p>摘要文本</p></li>"#.to_string())
            }
        }
        let net = std::sync::OnceLock::new();
        let call = ToolCall {
            id: "w".to_string(),
            name: "web_search".to_string(),
            arguments: serde_json::json!({"query": "rust 教程"}),
        };
        let obs = web_search_obs(&RestrictedFetch, &net, &call).await;
        assert!(obs.contains("受限(GFW 内)"), "{obs}");
        assert!(obs.contains("bing-cn"), "{obs}");
        assert!(
            obs.contains("标题") && obs.contains("https://ex.com/"),
            "{obs}"
        );
        assert_eq!(net.get(), Some(&NetEnv::Restricted)); // 探测结果被缓存

        // 缺 query → 明确报错,不打网络。
        struct NeverFetch;
        #[async_trait::async_trait]
        impl WebFetch for NeverFetch {
            async fn get_text(&self, _url: &str) -> Result<String, provider::ProviderError> {
                panic!("不该联网");
            }
        }
        let bad = ToolCall {
            id: "w2".to_string(),
            name: "web_search".to_string(),
            arguments: serde_json::json!({}),
        };
        let obs = web_search_obs(&NeverFetch, &std::sync::OnceLock::new(), &bad).await;
        assert!(obs.contains("缺少 query"), "{obs}");
    }
}
