use crate::{moss::blackboard, providers::{DynProvider, Message, Role}};
use serde_json::Value;

use super::blackboard::{Blackboard, Evidence, GapState};

pub struct Orchestrator {
    provider: DynProvider
}

impl Orchestrator {
    pub fn new(provider: DynProvider) -> Self {
        Self { provider }
    }

    pub async fn synthesize(&self, query: &str, blackboard: &Blackboard) -> Value {
        let template = std::fs::read_to_string("src/moss/prompts/orchestrator.xml")
            .expect("orchestrator prompt file missing: src/moss/prompts/orchestrator.xml");

        let blackboard_state = serde_json::to_string(blackboard).unwrap();

        let rendered = template
            .replace("{user_query}", query)
            .replace("{blackboard_state}", &blackboard_state);

        let messages = vec![Message { role: Role::User, content: rendered.into_boxed_str() }];

        let response = self.provider.complete_chat(messages).await;

        let clean = response
            .trim()
            .strip_prefix("```json")
            .unwrap_or(&response)
            .strip_prefix("```")
            .unwrap_or(&response)
            .strip_suffix("```")
            .unwrap_or(&response)
            .trim();

        let value = serde_json::from_str(clean).unwrap();

        std::fs::write("output.json", serde_json::to_string_pretty(&value).unwrap())
            .expect("write failed");

        value
    }
}
