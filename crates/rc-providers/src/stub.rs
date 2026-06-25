//! 脚本化假模型 provider(离线 eval / 单测用)。按调用顺序返回预制回复,
//! 队列耗尽后返回一个无工具调用的「完成。」文本——足以驱动 agent 工具循环收敛。

use std::collections::VecDeque;
use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;
use rc_types::{Completion, Message, Role, ToolSpec, Usage};

use crate::LlmProvider;

pub struct StubProvider {
    model: String,
    replies: Mutex<VecDeque<Completion>>,
}

impl StubProvider {
    pub fn new(model: impl Into<String>, replies: Vec<Completion>) -> Self {
        Self { model: model.into(), replies: Mutex::new(replies.into()) }
    }
}

#[async_trait]
impl LlmProvider for StubProvider {
    async fn complete(&self, _messages: &[Message], _tools: &[ToolSpec]) -> Result<Completion> {
        let mut q = self.replies.lock().unwrap();
        Ok(q.pop_front().unwrap_or_else(|| Completion {
            message: Message {
                role: Role::Assistant,
                content: "完成。".into(),
                tool_calls: Vec::new(),
                tool_call_id: None,
            },
            usage: Usage { input_tokens: 10, output_tokens: 5 },
        }))
    }

    fn model_id(&self) -> &str {
        &self.model
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn returns_scripted_then_default() {
        let scripted = Completion {
            message: Message {
                role: Role::Assistant,
                content: "脚本回复".into(),
                tool_calls: Vec::new(),
                tool_call_id: None,
            },
            usage: Usage { input_tokens: 1, output_tokens: 1 },
        };
        let p = StubProvider::new("stub", vec![scripted]);
        let r1 = p.complete(&[], &[]).await.unwrap();
        assert_eq!(r1.message.content, "脚本回复");
        let r2 = p.complete(&[], &[]).await.unwrap();
        assert_eq!(r2.message.content, "完成。");
        assert!(r2.message.tool_calls.is_empty());
    }
}
