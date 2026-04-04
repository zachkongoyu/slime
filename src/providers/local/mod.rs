use crate::providers::{Provider, Message, Role};
use crate::error::ProviderError;
use async_trait::async_trait;

pub struct LocalMock {}

impl LocalMock {
    pub fn new() -> Self { Self {} }
}

#[async_trait]
impl Provider for LocalMock {
    async fn complete_chat(&self, messages: Vec<Message>) -> Result<String, ProviderError> {
        let m = messages
            .into_iter()
            .rev()
            .find(|m| matches!(m.role, Role::User))
            .ok_or_else(|| ProviderError::Request("no user message".into()))?;
        let echo = format!("echo: {}", m.content);
        Ok(echo)
    }
}
