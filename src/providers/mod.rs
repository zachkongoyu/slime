use serde::{Deserialize, Serialize};
use serde_json::Value;
use async_trait::async_trait;

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
    async fn complete_chat(&self, messages: Vec<Message>) -> String;
}

pub type DynProvider = Box<dyn Provider>;
