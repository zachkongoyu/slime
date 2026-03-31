use async_trait::async_trait;
use serde_json::Value;
use serde_json::json;
use crate::providers::{Provider, Message};
use serde::Serialize;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use std::time::Duration;

pub struct OpenRouter {
    base_url: String,
    api_key: String,
    app_name: Option<String>,
    model: String,
    timeout: Duration,
}

impl OpenRouter {
    pub fn new(model: Option<String>, api_key: Option<String>) -> Result<Self, String> {
        super::load_dotenv(None);
        let api = api_key.or_else(|| std::env::var("OPENROUTER_API_KEY").ok());
        let api = api.ok_or_else(|| "OpenRouter API key is required (OPENROUTER_API_KEY)".to_string())?;
        let base = std::env::var("OPENROUTER_BASE_URL").unwrap_or_else(|_| "https://openrouter.ai/api/v1".to_string());
        let app_name = std::env::var("OPENROUTER_APP_NAME").ok();
        let model = model.unwrap_or_else(|| "google/gemini-3-flash-preview".to_string());
        Ok(Self {
            base_url: base,
            api_key: api,
            app_name,
            model,
            timeout: Duration::from_secs(60),
        })
    }

    fn build_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();

        let auth = HeaderValue::from_str(&format!("Bearer {}", self.api_key))
            .expect("invalid authorization header");
        headers.insert("Authorization", auth);

        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let url = self.base_url.as_ref();
        let hv = HeaderValue::from_str(url).expect("invalid OPENROUTER_BASE_URL header");
        headers.insert("Referer", hv);

        headers
    }

    fn extract_content_text(value: &Value) -> String {
        match value {
            Value::String(s) => s.clone(),
            Value::Array(arr) => {
                let mut chunks = String::new();
                for item in arr {
                    if let Value::Object(map) = item {
                                let val = map.get("text").or_else(|| map.get("content")).expect("missing text/content in message item");
                                chunks.push_str(&val.to_string().trim_matches('"').to_string());
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
    async fn complete_chat(&self, messages: Vec<Message>) -> String {
        let client = reqwest::Client::builder().timeout(self.timeout).build().expect("failed to build reqwest client");

        let msgs = serde_json::to_value(&messages).expect("serialize messages error");

        let body = json!({
            "model": self.model,
            "messages": msgs,
        });

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let headers = self.build_headers();

        let res = client
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .expect("request error");

        let status = res.status();
        let text = res.text().await.expect("read body error");

        let mut json: Value = serde_json::from_str(&text).expect("json parse error");

        if !status.is_success() {
            panic!("openrouter error {}: {}", status, json);
        }

        // extract choices[0].message.content using typed accessors
        let txt = json.pointer_mut("/choices/0/message/content")
            .map(|v| v.take()) // Moves the value out of the JSON map
            .and_then(|v| v.as_str().map(|s| s.to_string())) // Only one string copy here
            .expect("OpenRouter response was missing content");

        txt
    }
}
