use crate::providers::{Provider, Message, Role};
use async_trait::async_trait;

pub struct LocalMock {}

impl LocalMock {
    pub fn new() -> Self { Self {} }
}

#[async_trait]
impl Provider for LocalMock {
    async fn complete_chat(&self, messages: Vec<Message>) -> String {
        // Simple deterministic mock response: echo last user message
        let m = messages.into_iter().rev().find(|m| matches!(m.role, Role::User)).expect("no user message");
        let echo = format!("echo: {}", m.content);
        echo
    }
}
