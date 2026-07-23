use super::*;
use crate::http::HttpClient;
use serde_json::json;
use std::sync::Arc;

#[test]
fn openai_and_anthropic_normalize_to_same_tool_call() {
    // 两种厂商的 wire 格式,归一化后应得到等价的 ToolCall。
    let openai_wire = json!({
        "choices": [{"message": {
            "content": "let me run it",
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {"name": "run_shell", "arguments": "{\"cmd\":\"cargo build\"}"}
            }]
        }}]
    });
    let anthropic_wire = json!({
        "content": [
            {"type": "text", "text": "let me run it"},
            {"type": "tool_use", "id": "toolu_1", "name": "run_shell", "input": {"cmd": "cargo build"}}
        ]
    });

    let a = openai::parse_response(&openai_wire).unwrap();
    let b = anthropic::parse_response(&anthropic_wire).unwrap();

    assert_eq!(a.text, "let me run it");
    assert_eq!(b.text, "let me run it");
    assert_eq!(a.tool_calls.len(), 1);
    assert_eq!(a.tool_calls[0].name, "run_shell");
    assert_eq!(a.tool_calls[0].arguments, json!({"cmd": "cargo build"}));
    // 归一化到位:除了 id,两边的工具调用等价。
    assert_eq!(a.tool_calls[0].name, b.tool_calls[0].name);
    assert_eq!(a.tool_calls[0].arguments, b.tool_calls[0].arguments);
}

#[tokio::test]
async fn swap_provider_hot_switches_inner() {
    let comp = |t: &str| Completion {
        text: t.into(),
        ..Default::default()
    };
    let a: Arc<dyn LlmProvider> = Arc::new(ScriptedProvider::new(vec![comp("from-A")]));
    let b: Arc<dyn LlmProvider> = Arc::new(ScriptedProvider::new(vec![comp("from-B")]));
    let sw = SwapProvider::new(a);
    let req = CompletionRequest::default();
    // 起始走 A。
    assert_eq!(sw.complete(&req).await.unwrap().text, "from-A");
    // 热切换到 B → 下一次补全即走 B(图无需重建)。
    sw.swap(b);
    assert_eq!(sw.complete(&req).await.unwrap().text, "from-B");
}

#[test]
fn plain_text_response_has_no_tool_calls() {
    let wire = json!({"choices": [{"message": {"content": "all done"}}]});
    let c = openai::parse_response(&wire).unwrap();
    assert_eq!(c.text, "all done");
    assert!(c.tool_calls.is_empty());
}

#[test]
fn splits_thinking_from_answer() {
    // 成对块:块内为思考,块外为回答。
    let (a, t) = extract_inline_think("<think>reasoning here</think>pong");
    assert_eq!((a.as_str(), t.as_str()), ("pong", "reasoning here"));
    // GLM 实测:**裸 `</think>`**(无起始标签)漏进正文 → 其前为思考、其后为回答。
    // (旧代码只删标签、把「思考」并进回答显示,正是用户所诉「回答/思考错位」根因,此处订正。)
    let (a, t) = extract_inline_think("\nresult\n</think>\nThe answer");
    assert_eq!((a.as_str(), t.as_str()), ("The answer", "result"));
    // 孤立未闭合 `<think>`(流式截断)→ 其后全为思考。
    let (a, t) = extract_inline_think("ans<think>tail thinking");
    assert_eq!((a.as_str(), t.as_str()), ("ans", "tail thinking"));
    // 无标签原样(去首尾空白),全为回答。
    assert_eq!(strip_thinking("  clean  "), "clean");

    // 非流式:独立 `reasoning_content` 字段 → 进 reasoning,`content` 进 text(GLM 主路径)。
    let wire =
        json!({"choices": [{"message": {"content": "done", "reasoning_content": "let me think"}}]});
    let c = openai::parse_response(&wire).unwrap();
    assert_eq!(
        (c.text.as_str(), c.reasoning.as_str()),
        ("done", "let me think")
    );
    // 非流式:inline `<think>` 仍被剥净并归入 reasoning。
    let wire2 = json!({"choices": [{"message": {"content": "<think>x</think>done"}}]});
    let c2 = openai::parse_response(&wire2).unwrap();
    assert_eq!((c2.text.as_str(), c2.reasoning.as_str()), ("done", "x"));
}

/// 流式:`reasoning_content` 增量走 [`StreamChunk::Reasoning`],`content` 走 `Answer`,两路互不污染。
#[tokio::test]
async fn streaming_splits_reasoning_and_answer() {
    let frames: Vec<String> = vec![
        r#"{"choices":[{"delta":{"reasoning_content":"想一下"}}]}"#.into(),
        r#"{"choices":[{"delta":{"content":"你好"}}]}"#.into(),
        r#"{"choices":[{"delta":{"content":",世界"}}]}"#.into(),
        r#"{"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":3}}"#.into(),
    ];
    let p = OpenAiProvider::new("http://unused", "gpt-x", "key")
        .with_http(Arc::new(FakeStream(frames)));
    let req = CompletionRequest {
        messages: vec![Message::user("hi")],
        tools: vec![],
    };
    let answer = std::sync::Mutex::new(String::new());
    let reasoning = std::sync::Mutex::new(String::new());
    let on_token = |c: StreamChunk| match c {
        StreamChunk::Answer(t) => answer.lock().unwrap().push_str(&t),
        StreamChunk::Reasoning(t) => reasoning.lock().unwrap().push_str(&t),
    };
    let c = p.complete_streaming(&req, &on_token).await.unwrap();
    assert_eq!(c.text, "你好,世界");
    assert_eq!(c.reasoning, "想一下");
    assert_eq!(*answer.lock().unwrap(), "你好,世界");
    assert_eq!(*reasoning.lock().unwrap(), "想一下");
}

/// 多轮工具历史 → OpenAI wire:assistant 带 tool_calls,工具结果是 role=tool + tool_call_id。
#[test]
fn openai_build_request_multiturn_tool_loop() {
    let req = CompletionRequest {
        messages: vec![
            Message::system("sys"),
            Message::user("do it"),
            Message::assistant("").with_tool_calls(vec![ToolCall {
                id: "c1".to_string(),
                name: "run_shell".to_string(),
                arguments: json!({"cmd": "cargo build"}),
            }]),
            Message::tool_result("c1", "exit 0"),
        ],
        tools: vec![ToolSpec {
            name: "run_shell".to_string(),
            description: "run".to_string(),
            schema: json!({"type": "object"}),
        }],
    };
    let body = openai::build_request("gpt-x", &req);
    let msgs = body["messages"].as_array().unwrap();
    assert_eq!(msgs[0]["role"], "system");
    assert_eq!(msgs[1]["role"], "user");
    assert_eq!(msgs[2]["role"], "assistant");
    assert_eq!(msgs[2]["tool_calls"][0]["id"], "c1");
    assert_eq!(msgs[2]["tool_calls"][0]["function"]["name"], "run_shell");
    assert_eq!(msgs[3]["role"], "tool");
    assert_eq!(msgs[3]["tool_call_id"], "c1");
    assert_eq!(msgs[3]["content"], "exit 0");
    assert!(body["tools"].is_array());
}

/// 多轮工具历史 → Anthropic wire:system 顶层;tool_use / tool_result 块;相邻 user 合并。
#[test]
fn anthropic_build_request_merges_adjacent_and_uses_blocks() {
    let req = CompletionRequest {
        messages: vec![
            Message::system("sys"),
            Message::user("do it"),
            Message::assistant("").with_tool_calls(vec![ToolCall {
                id: "c1".to_string(),
                name: "run_shell".to_string(),
                arguments: json!({"cmd": "cargo build"}),
            }]),
            Message::tool_result("c1", "exit 0"),
            Message::user("and now?"),
        ],
        tools: vec![],
    };
    let body = anthropic::build_request("claude-x", 1024, &req);
    assert_eq!(body["system"], "sys");
    assert_eq!(body["max_tokens"], 1024);
    let msgs = body["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3, "user / assistant / merged-user");
    assert_eq!(msgs[0]["role"], "user");
    assert_eq!(msgs[0]["content"][0]["text"], "do it");
    assert_eq!(msgs[1]["role"], "assistant");
    assert_eq!(msgs[1]["content"][0]["type"], "tool_use");
    assert_eq!(msgs[1]["content"][0]["id"], "c1");
    // tool_result 与其后的 user 文本被合并进同一条 user 消息(Anthropic 要求角色交替)。
    assert_eq!(msgs[2]["role"], "user");
    assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
    assert_eq!(msgs[2]["content"][0]["tool_use_id"], "c1");
    assert_eq!(msgs[2]["content"][1]["text"], "and now?");
}

/// 注入 stub 传输:完整走 build_request → (假)HTTP → parse_response,零网络。
struct StubHttp(Value);

#[async_trait::async_trait]
impl HttpClient for StubHttp {
    async fn post_json(
        &self,
        _url: &str,
        _headers: &[(String, String)],
        _body: &Value,
    ) -> Result<Value, ProviderError> {
        Ok(self.0.clone())
    }
}

#[tokio::test]
async fn openai_provider_full_path_with_stub_http() {
    let canned = json!({"choices":[{"message":{"content":"","tool_calls":[{
        "id":"c1","type":"function",
        "function":{"name":"run_shell","arguments":"{\"cmd\":\"cargo build\"}"}
    }]}}]});
    let p =
        OpenAiProvider::new("http://unused", "gpt-x", "key").with_http(Arc::new(StubHttp(canned)));
    let req = CompletionRequest {
        messages: vec![Message::user("build it")],
        tools: vec![],
    };
    let c = p.complete(&req).await.unwrap();
    assert_eq!(c.tool_calls[0].name, "run_shell");
    assert_eq!(c.tool_calls[0].arguments, json!({"cmd": "cargo build"}));
}

// ── iter-29:实时模型列表解析 + fetch_models 全路径(零网络)──

/// GET 替身:get_json 恒返回预设 JSON,验 fetch_models 不联网走全路径。
struct StubGet(Value);
#[async_trait::async_trait]
impl HttpClient for StubGet {
    async fn post_json(
        &self,
        _u: &str,
        _h: &[(String, String)],
        _b: &Value,
    ) -> Result<Value, ProviderError> {
        Err("only GET".into())
    }
    async fn get_json(
        &self,
        _url: &str,
        _headers: &[(String, String)],
    ) -> Result<Value, ProviderError> {
        Ok(self.0.clone())
    }
}

#[test]
fn parse_model_list_openai() {
    let v = json!({"object":"list","data":[
        {"id":"gpt-4o","object":"model"},
        {"id":"gpt-4o-mini","object":"model"}
    ]});
    let ms = models::parse_model_list(&v);
    assert_eq!(ms.len(), 2);
    assert_eq!(ms[0].id, "gpt-4o");
    assert_eq!(ms[0].context, None);
}

#[test]
fn parse_model_list_openrouter_context() {
    let v = json!({"data":[
        {"id":"anthropic/claude-3.5","context_length":200000},
        {"id":"nested/x","top_provider":{"context_length":128000}}
    ]});
    let ms = models::parse_model_list(&v);
    assert_eq!(ms[0].context, Some(200000));
    assert_eq!(ms[1].context, Some(128000)); // 嵌套 top_provider 也捞到
}

#[test]
fn parse_model_list_malformed_is_empty() {
    assert!(models::parse_model_list(&json!("not-an-object")).is_empty());
    assert!(models::parse_model_list(&json!({})).is_empty());
    assert!(models::parse_model_list(&json!({"data":"nope"})).is_empty());
}

#[tokio::test]
async fn fetch_models_via_stub_http() {
    let canned = json!({"data":[{"id":"m1","context_window":32768}]});
    let got = models::fetch_models(&StubGet(canned), "openai", "http://unused/v1", "key")
        .await
        .unwrap();
    assert_eq!(
        got,
        vec![models::ModelInfo {
            id: "m1".into(),
            context: Some(32768)
        }]
    );
}

/// 假流式传输:把预设的 SSE `data:` 帧逐条喂给 on_line(不用 mockito、零网络)。
struct FakeStream(Vec<String>);
#[async_trait::async_trait]
impl HttpClient for FakeStream {
    async fn post_json(
        &self,
        _u: &str,
        _h: &[(String, String)],
        _b: &Value,
    ) -> Result<Value, ProviderError> {
        Err("only streaming".into())
    }
    async fn post_json_stream(
        &self,
        _u: &str,
        _h: &[(String, String)],
        _b: &Value,
        on_line: &(dyn Fn(String) + Send + Sync),
    ) -> Result<(), ProviderError> {
        for frame in &self.0 {
            on_line(frame.clone());
        }
        Ok(())
    }
}

#[tokio::test]
async fn openai_streaming_assembles_text_tokens_toolcalls_usage() {
    // 文本分 3 帧 + 一个分片工具调用(name 首帧、arguments 两帧)+ 末帧 usage。
    let frames: Vec<String> = vec![
            r#"{"choices":[{"delta":{"content":"你好"}}]}"#.into(),
            r#"{"choices":[{"delta":{"content":",世界"}}]}"#.into(),
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"run_shell","arguments":"{\"cmd\":"}}]}}]}"#.into(),
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"ls\"}"}}]}}]}"#.into(),
            r#"{"choices":[],"usage":{"prompt_tokens":11,"completion_tokens":7}}"#.into(),
        ];
    let p = OpenAiProvider::new("http://unused", "gpt-x", "key")
        .with_http(Arc::new(FakeStream(frames)));
    let req = CompletionRequest {
        messages: vec![Message::user("hi")],
        tools: vec![],
    };
    let streamed = std::sync::Mutex::new(String::new());
    let on_token = |c: StreamChunk| {
        if let StreamChunk::Answer(t) = c {
            streamed.lock().unwrap().push_str(&t);
        }
    };
    let c = p.complete_streaming(&req, &on_token).await.unwrap();
    // 逐字回调拼起来 == 完整文本。
    assert_eq!(*streamed.lock().unwrap(), "你好,世界");
    assert_eq!(c.text, "你好,世界");
    // 分片工具调用被拼装回完整对象。
    assert_eq!(c.tool_calls.len(), 1);
    assert_eq!(c.tool_calls[0].name, "run_shell");
    assert_eq!(c.tool_calls[0].arguments, json!({"cmd": "ls"}));
    assert_eq!(c.usage.total(), 18);
}

/// 默认 provider(无流式实现)+ 不支持流式的传输 → complete_streaming 降级到整段,一次性 emit。
#[tokio::test]
async fn streaming_falls_back_to_whole_text() {
    let canned = json!({"choices":[{"message":{"content":"整段回答"}}]});
    let p =
        OpenAiProvider::new("http://unused", "gpt-x", "key").with_http(Arc::new(StubHttp(canned))); // StubHttp 没实现 post_json_stream → 默认报错 → 降级
    let req = CompletionRequest {
        messages: vec![Message::user("hi")],
        tools: vec![],
    };
    let got = std::sync::Mutex::new(String::new());
    let c = p
        .complete_streaming(&req, &|c: StreamChunk| {
            if let StreamChunk::Answer(t) = c {
                got.lock().unwrap().push_str(&t);
            }
        })
        .await
        .unwrap();
    assert_eq!(c.text, "整段回答");
    assert_eq!(*got.lock().unwrap(), "整段回答");
}

/// 记录 provider 实际发出的一次请求。
#[derive(Clone)]
struct SeenRequest {
    url: String,
    headers: Vec<(String, String)>,
    body: Value,
}

/// 捕获型传输替身:记录 provider 拼出的请求,并回一个 canned 响应。
/// 用它校验「provider 把请求拼对了」——比起真打本地 server(在有系统代理的机器上会挂起),
/// 这是确定性、零网络、瞬时的,覆盖同样的东西(auth 头 / url / 请求体)。
struct CapturingHttp {
    seen: std::sync::Mutex<Option<SeenRequest>>,
    reply: Value,
}

impl CapturingHttp {
    fn new(reply: Value) -> Self {
        Self {
            seen: std::sync::Mutex::new(None),
            reply,
        }
    }

    fn seen(&self) -> SeenRequest {
        self.seen.lock().unwrap().clone().unwrap()
    }
}

#[async_trait::async_trait]
impl HttpClient for CapturingHttp {
    async fn post_json(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: &Value,
    ) -> Result<Value, ProviderError> {
        *self.seen.lock().unwrap() = Some(SeenRequest {
            url: url.to_string(),
            headers: headers.to_vec(),
            body: body.clone(),
        });
        Ok(self.reply.clone())
    }
}

#[tokio::test]
async fn openai_provider_sends_bearer_auth_and_correct_url() {
    let cap = Arc::new(CapturingHttp::new(
        json!({"choices":[{"message":{"content":"ok"}}]}),
    ));
    let p =
        OpenAiProvider::new("https://api.example.com", "gpt-x", "sk-test").with_http(cap.clone());
    let c = p
        .complete(&CompletionRequest {
            messages: vec![Message::user("hi")],
            tools: vec![],
        })
        .await
        .unwrap();

    assert_eq!(c.text, "ok");
    let seen = cap.seen();
    assert_eq!(seen.url, "https://api.example.com/chat/completions");
    assert!(seen
        .headers
        .iter()
        .any(|(k, v)| k == "Authorization" && v == "Bearer sk-test"));
    assert_eq!(seen.body["messages"][0]["role"], "user");
}

#[tokio::test]
async fn anthropic_provider_sends_api_key_version_and_system_top_level() {
    let cap = Arc::new(CapturingHttp::new(
        json!({"content":[{"type":"text","text":"ok"}]}),
    ));
    let p = AnthropicProvider::new("https://api.anthropic.com/v1", "claude-x", "sk-ant")
        .with_http(cap.clone());
    let c = p
        .complete(&CompletionRequest {
            messages: vec![Message::system("be terse"), Message::user("hi")],
            tools: vec![],
        })
        .await
        .unwrap();

    assert_eq!(c.text, "ok");
    let seen = cap.seen();
    assert_eq!(seen.url, "https://api.anthropic.com/v1/messages");
    assert!(seen
        .headers
        .iter()
        .any(|(k, v)| k == "x-api-key" && v == "sk-ant"));
    assert!(seen
        .headers
        .iter()
        .any(|(k, v)| k == "anthropic-version" && !v.is_empty()));
    // system 抽成顶层参数,不混进 messages。
    assert_eq!(seen.body["system"], "be terse");
    assert_eq!(seen.body["messages"][0]["role"], "user");
}

// ── iter-43 · OAuth(PKCE)订阅登录 ─────────────────────────────

#[test]
fn base64url_nopad_matches_rfc4648_vectors() {
    assert_eq!(oauth::base64url_nopad(b""), "");
    assert_eq!(oauth::base64url_nopad(b"f"), "Zg");
    assert_eq!(oauth::base64url_nopad(b"fo"), "Zm8");
    assert_eq!(oauth::base64url_nopad(b"foobar"), "Zm9vYmFy");
}

#[test]
fn pkce_challenge_is_s256_base64url_of_verifier() {
    // RFC 7636 附录 B 的规范向量。
    let pkce = oauth::Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
    assert_eq!(
        pkce.challenge,
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
}

#[test]
fn authorize_url_carries_challenge_scopes_state() {
    // 回归(iter-48 G1):anthropic URL 与泛化前逐字节等价的关键要素。
    let url = oauth::authorize_url(&oauth::ANTHROPIC, "CHAL123", "STATE789");
    assert!(url.starts_with("https://claude.ai/oauth/authorize?code=true&client_id="));
    assert!(url.contains("code_challenge=CHAL123"));
    assert!(url.contains("code_challenge_method=S256"));
    assert!(url.contains("state=STATE789"));
    assert!(url.contains("scope=org"));
}

#[test]
fn authorize_url_openai_uses_codex_endpoint_and_scopes() {
    // iter-48 G2:openai 授权 URL 打 auth.openai.com,标准 PKCE 参数,无 anthropic 特有 code=true。
    let url = oauth::authorize_url(&oauth::OPENAI, "CH", "ST");
    assert!(url.starts_with("https://auth.openai.com/oauth/authorize?client_id="));
    assert!(!url.contains("code=true"));
    assert!(url.contains("scope=openid%20profile%20email%20offline_access"));
    assert!(url.contains("code_challenge=CH"));
    assert!(url.contains("code_challenge_method=S256"));
    assert!(url.contains("state=ST"));
}

#[test]
fn parse_callback_path_extracts_code_state() {
    // 整个请求首行 / 裸 path / 整 URL 皆可;无 code → None。
    assert_eq!(
        oauth::parse_callback_path("GET /auth/callback?code=abc&state=xyz HTTP/1.1"),
        Some(("abc".into(), "xyz".into()))
    );
    assert_eq!(
        oauth::parse_callback_path("http://localhost:1455/auth/callback?state=s&code=c"),
        Some(("c".into(), "s".into()))
    );
    assert_eq!(
        oauth::parse_callback_path("GET /favicon.ico HTTP/1.1"),
        None
    );
    assert_eq!(
        oauth::parse_callback_path("/auth/callback?state=only"),
        None
    );
}

#[test]
fn parse_token_response_sets_expiry_from_now() {
    let v = json!({"access_token":"acc","refresh_token":"ref","expires_in":3600});
    let t = oauth::parse_token_response(&v, 1000, None).unwrap();
    assert_eq!(t.access_token, "acc");
    assert_eq!(t.refresh_token, "ref");
    assert_eq!(t.expires_at_epoch, 4600);
}

#[test]
fn parse_token_response_falls_back_refresh_when_absent() {
    // 刷新响应常不带新 refresh_token → 回落旧的。
    let v = json!({"access_token":"acc2","expires_in":100});
    let t = oauth::parse_token_response(&v, 0, Some("old-refresh")).unwrap();
    assert_eq!(t.refresh_token, "old-refresh");
}

#[test]
fn needs_refresh_true_past_expiry_false_when_fresh() {
    let t = oauth::OAuthToken {
        access_token: "a".into(),
        refresh_token: "r".into(),
        expires_at_epoch: 4600,
    };
    assert!(!t.needs_refresh(1000)); // 远未过期
    assert!(t.needs_refresh(5000)); // 已过期
    assert!(t.needs_refresh(4550)); // 60s 余量内也要刷
}

#[tokio::test]
async fn anthropic_oauth_headers_use_bearer_beta_not_apikey() {
    let cap = Arc::new(CapturingHttp::new(
        json!({"content":[{"type":"text","text":"ok"}]}),
    ));
    let p = AnthropicProvider::new_oauth("https://api.anthropic.com/v1", "claude-x", "tok-abc")
        .with_http(cap.clone());
    p.complete(&CompletionRequest {
        messages: vec![Message::user("hi")],
        tools: vec![],
    })
    .await
    .unwrap();

    let seen = cap.seen();
    assert!(seen
        .headers
        .iter()
        .any(|(k, v)| k == "Authorization" && v == "Bearer tok-abc"));
    assert!(seen.headers.iter().any(|(k, _)| k == "anthropic-beta"));
    // oauth 模式绝不发 x-api-key。
    assert!(!seen.headers.iter().any(|(k, _)| k == "x-api-key"));
    // system 首块注入了 Claude Code 身份(OAuth 路径要求)。
    assert!(seen.body["system"]
        .as_str()
        .unwrap()
        .contains("Claude Code"));
}

#[tokio::test]
async fn exchange_code_and_refresh_parse_via_fake_http() {
    let cap = Arc::new(CapturingHttp::new(
        json!({"access_token":"A","refresh_token":"R","expires_in":3600}),
    ));
    let t = oauth::exchange_code(
        cap.as_ref(),
        &oauth::ANTHROPIC,
        "the-code#the-state",
        "verifier-x",
        10,
    )
    .await
    .unwrap();
    assert_eq!(t.access_token, "A");
    assert_eq!(t.expires_at_epoch, 3610);
    // 交换请求体带 grant_type / code / code_verifier。
    let seen = cap.seen();
    assert_eq!(seen.body["grant_type"], "authorization_code");
    assert_eq!(seen.body["code"], "the-code");
    assert_eq!(seen.body["code_verifier"], "verifier-x");

    let r = oauth::refresh(cap.as_ref(), &oauth::ANTHROPIC, "R", 20)
        .await
        .unwrap();
    assert_eq!(r.access_token, "A");
    assert_eq!(r.expires_at_epoch, 3620);
}

#[tokio::test]
async fn openai_token_wire_goes_form_not_json() {
    // iter-48 G2:openai token 端点走 form-urlencoded(RFC 6749),不走 JSON body;
    // CapturingHttp 未实现 post_form → 默认 Err,即证 Json 路径未被走。
    let cap = Arc::new(CapturingHttp::new(
        json!({"access_token":"A","refresh_token":"R","expires_in":1}),
    ));
    let e = oauth::exchange_code(cap.as_ref(), &oauth::OPENAI, "c", "v", 0).await;
    assert!(e.unwrap_err().to_string().contains("form"));
}
