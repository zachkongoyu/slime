use async_trait::async_trait;
use serde_json::Value;
use serde_json::json;
use crate::providers::{Provider, Message};
use crate::error::ProviderError;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use std::time::Duration;

pub struct OpenRouter {
    base_url: String,
    api_key: String,
    model: String,
    timeout: Duration,
}

impl OpenRouter {
    pub fn new(model: Option<String>, api_key: Option<String>) -> Result<Self, String> {
        super::load_dotenv(None);
        let api = api_key.or_else(|| std::env::var("OPENROUTER_API_KEY").ok());
        let api = api.ok_or_else(|| "OpenRouter API key is required (OPENROUTER_API_KEY)".to_string())?;
        let base = std::env::var("OPENROUTER_BASE_URL")
            .unwrap_or_else(|_| "https://openrouter.ai/api/v1".to_string());
        let model = model.unwrap_or_else(|| "google/gemini-3-flash-preview".to_string());
        Ok(Self {
            base_url: base,
            api_key: api,
            model,
            timeout: Duration::from_secs(60),
        })
    }

    fn build_headers(&self) -> Result<HeaderMap, ProviderError> {
        let mut headers = HeaderMap::new();

        let auth = HeaderValue::from_str(&format!("Bearer {}", self.api_key))
            .map_err(|e| ProviderError::Request(format!("invalid authorization header: {e}")))?;
        headers.insert("Authorization", auth);

        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let hv = HeaderValue::from_str(&self.base_url)
            .map_err(|e| ProviderError::Request(format!("invalid OPENROUTER_BASE_URL header: {e}")))?;
        headers.insert("Referer", hv);

        Ok(headers)
    }

    fn extract_content_text(value: &Value) -> String {
        match value {
            Value::String(s) => s.clone(),
            Value::Array(arr) => {
                let mut chunks = String::new();
                for item in arr {
                    if let Value::Object(map) = item {
                        let val = map.get("text").or_else(|| map.get("content"));
                        if let Some(v) = val {
                            chunks.push_str(v.to_string().trim_matches('"'));
                        }
                    }
                }
                chunks
            }
            other => other.to_string(),
        }
    }
}

#[async_trait]
impl Provider for OpenRouter {
    async fn complete_chat(&self, messages: Vec<Message>) -> Result<String, ProviderError> {
        let client = reqwest::Client::builder()
            .timeout(self.timeout)
            .build()
            .map_err(|e| ProviderError::Request(e.to_string()))?;

        let msgs = serde_json::to_value(&messages)
            .map_err(|e| ProviderError::Parse(e.to_string()))?;

        let body = json!({
            "model": self.model,
            "messages": msgs,
        });

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let headers = self.build_headers()?;

        let res = client
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Request(e.to_string()))?;

        let status = res.status();
        let text = res.text().await.map_err(|e| ProviderError::Request(e.to_string()))?;

        if !status.is_success() {
            return Err(ProviderError::ApiError { status: status.as_u16(), body: text });
        }

        let mut json: Value = serde_json::from_str(&text)
            .map_err(|e| ProviderError::Parse(e.to_string()))?;

        let content = json
            .pointer_mut("/choices/0/message/content")
            .map(|v| v.take())
            .ok_or_else(|| ProviderError::Parse("OpenRouter response missing content".into()))?;

        Ok(Self::extract_content_text(&content))
    }
}
