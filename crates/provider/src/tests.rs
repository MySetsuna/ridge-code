use super::*;
use crate::http::HttpClient;
use serde_json::json;
use std::sync::{Arc, Mutex};

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

#[test]
fn anthropic_preserves_native_and_inline_thinking() {
    let wire = json!({
        "content": [
            {"type": "thinking", "thinking": "native plan"},
            {"type": "text", "text": "<think>inline detail</think>done"}
        ],
        "usage": {"input_tokens": 3, "output_tokens": 5}
    });

    let c = anthropic::parse_response(&wire).unwrap();

    assert_eq!(c.text, "done");
    assert_eq!(c.reasoning, "native plan\ninline detail");
    assert_eq!(c.usage.prompt_tokens, 3);
    assert_eq!(c.usage.completion_tokens, 5);
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

#[tokio::test]
async fn scripted_delay_streams_reasoning_before_answer() {
    let provider = ScriptedProvider::new(vec![Completion {
        reasoning: "thinking".into(),
        text: "answer".into(),
        ..Default::default()
    }])
    .with_delay(std::time::Duration::from_millis(1));
    let chunks = Mutex::new(Vec::new());
    let on_token = |chunk: StreamChunk| {
        chunks.lock().unwrap().push(match chunk {
            StreamChunk::Reasoning(text) => format!("reasoning:{text}"),
            StreamChunk::Answer(text) => format!("answer:{text}"),
        });
    };

    let completion = provider
        .complete_streaming(&CompletionRequest::default(), &on_token)
        .await
        .unwrap();

    assert_eq!(completion.reasoning, "thinking");
    assert_eq!(completion.text, "answer");
    assert_eq!(
        *chunks.lock().unwrap(),
        vec!["reasoning:thinking", "answer:answer"]
    );
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
    assert_eq!(extract_inline_think("  clean  ").0, "clean");

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

struct CaptureGet {
    value: Value,
    url: Arc<Mutex<Option<String>>>,
}

#[async_trait::async_trait]
impl HttpClient for CaptureGet {
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
        url: &str,
        _headers: &[(String, String)],
    ) -> Result<Value, ProviderError> {
        *self.url.lock().unwrap() = Some(url.to_string());
        Ok(self.value.clone())
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
fn parse_chatgpt_model_catalog_uses_slug_and_models_array() {
    let v = json!({
        "models": [
            {"slug": "gpt-5.3-codex", "context_window": 200000},
            {"slug": "gpt-5.4"},
            {"slug": "hidden-rollout", "visibility": "hide"}
        ]
    });
    assert_eq!(
        models::parse_model_list(&v),
        vec![
            models::ModelInfo {
                id: "gpt-5.3-codex".into(),
                context: Some(200000)
            },
            models::ModelInfo {
                id: "gpt-5.4".into(),
                context: None
            }
        ]
    );
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

#[tokio::test]
async fn fetch_chatgpt_models_uses_codex_client_version_query() {
    let url = Arc::new(Mutex::new(None));
    let got = models::fetch_chatgpt_models(
        &CaptureGet {
            value: json!({"models":[{"slug":"gpt-5.6-sol","visibility":"list"}]}),
            url: url.clone(),
        },
        "https://chatgpt.com/backend-api/codex",
        "access",
        Some("account"),
    )
    .await
    .unwrap();
    assert_eq!(got[0].id, "gpt-5.6-sol");
    assert!(url
        .lock()
        .unwrap()
        .as_deref()
        .is_some_and(|value| value
            .starts_with("https://chatgpt.com/backend-api/codex/models?client_version=")));
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

struct StreamingCapturingHttp {
    seen: std::sync::Mutex<Option<SeenRequest>>,
    frames: Vec<String>,
}

impl StreamingCapturingHttp {
    fn new(frames: Vec<String>) -> Self {
        Self {
            seen: std::sync::Mutex::new(None),
            frames,
        }
    }

    fn seen(&self) -> SeenRequest {
        self.seen.lock().unwrap().clone().unwrap()
    }
}

#[async_trait::async_trait]
impl HttpClient for StreamingCapturingHttp {
    async fn post_json(
        &self,
        _url: &str,
        _headers: &[(String, String)],
        _body: &Value,
    ) -> Result<Value, ProviderError> {
        Err("only streaming".into())
    }

    async fn post_json_stream(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: &Value,
        on_line: &(dyn Fn(String) + Send + Sync),
    ) -> Result<(), ProviderError> {
        *self.seen.lock().unwrap() = Some(SeenRequest {
            url: url.to_string(),
            headers: headers.to_vec(),
            body: body.clone(),
        });
        for frame in &self.frames {
            on_line(frame.clone());
        }
        Ok(())
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

#[test]
fn reasoning_effort_accepts_canonical_values_case_insensitively() {
    assert_eq!(normalize_reasoning_effort(" HIGH "), Some("high"));
    assert_eq!(normalize_reasoning_effort("xhigh"), Some("xhigh"));
    assert_eq!(normalize_reasoning_effort("unknown"), None);
}

#[tokio::test]
async fn chatgpt_provider_uses_codex_responses_wire() {
    let cap = Arc::new(StreamingCapturingHttp::new(vec![
        r#"{"type":"response.output_text.delta","delta":"ok"}"#.into(),
        r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","call_id":"call_1","delta":"{\"cmd\":"}"#.into(),
        r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","call_id":"call_1","delta":"\"pwd\"}"}"#.into(),
        r#"{"type":"response.output_item.done","item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"run_shell","arguments":"{\"cmd\":\"pwd\"}"}}"#.into(),
        r#"{"type":"response.completed","response":{"usage":{"input_tokens":4,"output_tokens":2}}}"#.into(),
    ]));
    let p = ChatGptProvider::new(
        "https://chatgpt.com/backend-api/codex/",
        "gpt-5",
        "oauth-access",
        Some("acct-123".into()),
    )
    .with_reasoning_effort("high")
    .with_http(cap.clone());
    let c = p
        .complete(&CompletionRequest {
            messages: vec![
                Message::system("be concise"),
                Message::user("inspect it"),
                Message::assistant("").with_tool_calls(vec![ToolCall {
                    id: "call-previous".into(),
                    name: "run_shell".into(),
                    arguments: json!({"cmd": "ls"}),
                }]),
                Message::tool_result("call-previous", "files"),
            ],
            tools: vec![ToolSpec {
                name: "run_shell".into(),
                description: "run a command".into(),
                schema: json!({"type": "object", "properties": {"cmd": {"type": "string"}}}),
            }],
        })
        .await
        .unwrap();

    assert_eq!(c.text, "ok");
    assert_eq!(c.tool_calls.len(), 1);
    assert_eq!(c.tool_calls[0].id, "call_1");
    assert_eq!(c.tool_calls[0].arguments, json!({"cmd": "pwd"}));
    assert_eq!(c.usage.total(), 6);

    let seen = cap.seen();
    assert_eq!(seen.url, "https://chatgpt.com/backend-api/codex/responses");
    assert!(seen
        .headers
        .iter()
        .any(|(k, v)| k == "Authorization" && v == "Bearer oauth-access"));
    assert!(seen
        .headers
        .iter()
        .any(|(k, v)| k == "ChatGPT-Account-Id" && v == "acct-123"));
    assert!(seen
        .headers
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("originator") && v == "codex_cli_rs"));
    assert_eq!(seen.body["model"], "gpt-5");
    assert_eq!(seen.body["reasoning"]["effort"], "high");
    assert_eq!(seen.body["instructions"], "be concise");
    assert!(seen.body.get("messages").is_none());
    assert_eq!(seen.body["input"][0]["type"], "message");
    assert_eq!(seen.body["input"][1]["type"], "function_call");
    assert_eq!(seen.body["input"][2]["type"], "function_call_output");
    assert_eq!(seen.body["tools"][0]["type"], "function");
    assert_eq!(seen.body["stream"], true);
    assert_eq!(seen.body["store"], false);
}

#[test]
fn repair_tool_history_removes_orphans_but_keeps_completed_pairs() {
    let history = vec![
        Message::user("inspect"),
        Message::assistant("").with_tool_calls(vec![
            ToolCall {
                id: "orphan".into(),
                name: "run_shell".into(),
                arguments: json!({"cmd": "never"}),
            },
            ToolCall {
                id: "complete".into(),
                name: "run_shell".into(),
                arguments: json!({"cmd": "pwd"}),
            },
        ]),
        Message::tool_result("complete", "ok"),
        Message::tool_result("stale", "discard"),
        Message::assistant("next"),
    ];

    let repaired = repair_tool_history(&history);
    assert_eq!(repaired.len(), 4);
    assert_eq!(repaired[1].tool_calls.len(), 1);
    assert_eq!(repaired[1].tool_calls[0].id, "complete");
    assert_eq!(repaired[2].tool_call_id.as_deref(), Some("complete"));
    assert_eq!(repaired[3].content, "next");

    let body = crate::responses::build_request(
        "gpt-5",
        &CompletionRequest {
            messages: history,
            tools: Vec::new(),
        },
    );
    let input = body["input"].as_array().expect("responses input");
    assert_eq!(input[1]["type"], "function_call");
    assert_eq!(input[1]["call_id"], "complete");
    assert_eq!(input[2]["type"], "function_call_output");
    assert_eq!(input[2]["call_id"], "complete");
    assert_eq!(input.len(), 4);
}

#[test]
fn responses_stream_accumulator_handles_text_reasoning_tools_usage_and_errors() {
    let mut acc = responses::StreamAcc::default();
    let chunks = Arc::new(Mutex::new(Vec::new()));
    let push_chunks = chunks.clone();
    let push = move |chunk| push_chunks.lock().unwrap().push(chunk);
    responses::accumulate_stream(
        &mut acc,
        &json!({"type":"response.output_text.delta","delta":"answer"}),
        &push,
    );
    responses::accumulate_stream(
        &mut acc,
        &json!({"type":"response.reasoning_summary_text.delta","delta":"think"}),
        &push,
    );
    responses::accumulate_stream(
        &mut acc,
        &json!({"type":"response.function_call_arguments.delta","call_id":"c1","item_id":"i1","delta":"{\"x\":"}),
        &push,
    );
    responses::accumulate_stream(
        &mut acc,
        &json!({"type":"response.function_call_arguments.delta","call_id":"c1","delta":"1}"}),
        &push,
    );
    responses::accumulate_stream(
        &mut acc,
        &json!({"type":"response.output_item.done","item":{"type":"function_call","id":"i1","call_id":"c1","name":"tool","arguments":"{\"x\":1}"}}),
        &push,
    );
    responses::accumulate_stream(
        &mut acc,
        &json!({"type":"response.output_item.done","item":{"type":"message","content":[{"text":"fallback"},{"text":""}]}}),
        &push,
    );
    responses::accumulate_stream(
        &mut acc,
        &json!({"type":"response.completed","response":{"usage":{"input_tokens":7,"output_tokens":3}}}),
        &push,
    );
    assert!(acc.completed);
    assert_eq!(acc.usage.prompt_tokens, 7);
    assert_eq!(acc.usage.completion_tokens, 3);
    let completion = acc.into_completion();
    assert_eq!(completion.text, "answer");
    assert_eq!(completion.reasoning, "think");
    assert_eq!(completion.tool_calls[0].arguments, json!({"x":1}));
    let chunks = chunks.lock().unwrap();
    assert!(matches!(chunks[0], StreamChunk::Answer(_)));
    assert!(matches!(chunks[1], StreamChunk::Reasoning(_)));

    let mut failed = responses::StreamAcc::default();
    responses::accumulate_stream(
        &mut failed,
        &json!({"type":"response.failed","response":{"error":{"message":"bad"}}}),
        &|_| {},
    );
    assert!(responses::stream_error(&failed)
        .unwrap()
        .to_string()
        .contains("bad"));
    responses::accumulate_stream(
        &mut failed,
        &json!({"type":"response.incomplete","response":{"incomplete_details":{"reason":"cutoff"}}}),
        &|_| {},
    );
    assert!(responses::stream_error(&failed)
        .unwrap()
        .to_string()
        .contains("cutoff"));
    responses::accumulate_stream(&mut failed, &json!({"type":"unknown"}), &|_| {});
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
    // OpenAI Codex OAuth 需标准 PKCE + 连接器 scopes + Hydra 简化流标记。
    let url = oauth::authorize_url(&oauth::OPENAI, "CH", "ST");
    assert!(
        url.starts_with("https://auth.openai.com/oauth/authorize?id_token_add_organizations=true")
    );
    assert!(!url.contains("code=true"));
    assert!(url.contains("scope=openid%20profile%20email%20offline_access%20api.connectors.read%20api.connectors.invoke"));
    assert!(url.contains("id_token_add_organizations=true"));
    assert!(url.contains("codex_cli_simplified_flow=true"));
    assert!(url.contains("originator=codex_cli_rs"));
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
fn parse_authorization_input_validates_state_for_url_and_code_pair() {
    assert_eq!(
        oauth::parse_authorization_input(
            "GET /auth/callback?code=abc&state=expected HTTP/1.1",
            "expected",
        )
        .unwrap(),
        "abc#expected"
    );
    assert_eq!(
        oauth::parse_authorization_input("abc#expected", "expected").unwrap(),
        "abc#expected"
    );
    assert!(oauth::parse_authorization_input("abc#wrong", "expected")
        .unwrap_err()
        .to_string()
        .contains("state mismatch"));
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
fn chatgpt_account_id_is_extracted_from_oauth_claims() {
    let payload = json!({
        "https://api.openai.com/auth": {"chatgpt_account_id": "acct-xyz"}
    });
    let id_token = format!(
        "e30.{}.sig",
        oauth::base64url_nopad(payload.to_string().as_bytes())
    );
    assert_eq!(
        oauth::chatgpt_account_id(Some(&id_token), "opaque-access"),
        Some("acct-xyz".into())
    );

    let token = oauth::parse_token_response(
        &json!({
            "access_token": "opaque-access",
            "refresh_token": "refresh",
            "expires_in": 60,
            "id_token": id_token,
        }),
        100,
        None,
    )
    .unwrap();
    assert_eq!(token.account_id.as_deref(), Some("acct-xyz"));
    assert!(token.id_token.is_some());
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
        id_token: None,
        account_id: None,
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

#[tokio::test]
async fn openai_device_code_flow_parses_user_code_and_completion() {
    let request = Arc::new(CapturingHttp::new(json!({
        "device_auth_id": "device-1",
        "user_code": "ABCD-EFGH",
        "interval": "5"
    })));
    let device = oauth::request_device_code(request.as_ref(), oauth::OPENAI.client_id)
        .await
        .unwrap();
    assert_eq!(device.device_auth_id, "device-1");
    assert_eq!(device.user_code, "ABCD-EFGH");
    assert_eq!(device.interval_secs, 5);

    let completion = Arc::new(CapturingHttp::new(json!({
        "authorization_code": "auth-code",
        "code_challenge": "challenge",
        "code_verifier": "verifier"
    })));
    let authorization = oauth::poll_device_code(completion.as_ref(), &device)
        .await
        .unwrap();
    assert_eq!(authorization.authorization_code, "auth-code");
    assert_eq!(authorization.code_challenge, "challenge");
    assert_eq!(authorization.code_verifier, "verifier");
}
