use serde::{Deserialize, Serialize};
use async_trait::async_trait;
use crate::error::ProviderError;

pub mod local;
pub mod remote;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    System,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Box<str>,
}

#[async_trait]
pub trait Provider: Send + Sync {
    async fn complete_chat(&self, messages: Vec<Message>) -> Result<String, ProviderError>;

    async fn complete_with_tools(&self, messages: Vec<Message>) -> Result<String, ProviderError> {
        let _ = messages;
        Err(ProviderError::NotSupported)
    }
}

pub type DynProvider = Box<dyn Provider>;
