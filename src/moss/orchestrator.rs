use std::sync::Arc;

use minijinja::{Environment, context};
use tracing::info;

use crate::error::MossError;
use crate::providers::{Message, Role, Provider};

use super::blackboard::Blackboard;
use super::decomposition::Decomposition;

pub(crate) struct Orchestrator {
    provider: Arc<dyn Provider>,
}

impl Orchestrator {
    pub(crate) fn new(provider: Arc<dyn Provider>) -> Self {
        Self { provider }
    }

    /// Ask the LLM to decompose the query into a Gap DAG and insert gaps into the Blackboard.
    /// Returns the intent string and the names of the gaps created.
    pub(crate) async fn decompose(&self, query: &str, blackboard: &Blackboard) -> Result<Decomposition, MossError> {
        let template_src = include_str!("prompts/decompose.md");

        let blackboard_state = blackboard.snapshot();

        let mut env = Environment::new();
        env.add_template("decompose", template_src)
            .map_err(|e| MossError::Blackboard(format!("template error: {e}")))?;

        let tmpl = env.get_template("decompose")
            .map_err(|e| MossError::Blackboard(format!("template load error: {e}")))?;

        let rendered = tmpl
            .render(context! { user_query => query, blackboard_state => blackboard_state })
            .map_err(|e| MossError::Blackboard(format!("template render error: {e}")))?;

        let messages = vec![Message { role: Role::User, content: rendered.into_boxed_str() }];

        let raw = self.provider.complete_chat(messages).await?;

        // Strip markdown fences if the model wrapped the JSON anyway
        let json_str = raw
            .trim()
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();

        let decomposition: Decomposition = serde_json::from_str(json_str)?;

        let gap_names: Vec<&str> = decomposition.gaps.as_deref()
            .unwrap_or(&[])
            .iter()
            .map(|g| g.name.as_str())
            .collect();
        info!(intent = ?decomposition.intent, gaps = ?gap_names, "decomposed");

        Ok(decomposition)
    }

    /// Collect evidence from the Blackboard and synthesize a final answer.
    pub(crate) async fn synthesize(&self, blackboard: &Blackboard) -> Result<String, MossError> {
        let template_src = include_str!("prompts/synthesize.md");

        let intent = blackboard
            .get_intent()
            .map(|s| s.to_string())
            .unwrap_or_else(|| "unknown intent".to_string());

        let evidence = serde_json::to_string_pretty(&blackboard.all_evidence())?;

        let mut env = Environment::new();
        env.add_template("synthesize", template_src)
            .map_err(|e| MossError::Blackboard(format!("template error: {e}")))?;

        let tmpl = env.get_template("synthesize")
            .map_err(|e| MossError::Blackboard(format!("template load error: {e}")))?;

        let rendered = tmpl
            .render(context! { intent => intent, evidence => evidence })
            .map_err(|e| MossError::Blackboard(format!("template render error: {e}")))?;

        let messages = vec![Message { role: Role::User, content: rendered.into_boxed_str() }];

        info!("synthesizing final answer");
        let response = self.provider.complete_chat(messages).await?;

        Ok(response)
    }
}
