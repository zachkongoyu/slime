use std::sync::{Arc, Mutex};

use minijinja::{Environment, context};
use tracing::info;

use crate::error::MossError;
use crate::providers::{Message, Role, Provider};

use super::blackboard::{Blackboard, Gap};
use super::decomposition::Decomposition;
use super::runner::Runner;

pub(crate) struct Orchestrator {
    provider: Arc<dyn Provider>,
    runner: Runner,
    blackboard: Mutex<Arc<Blackboard>>,
}

impl Orchestrator {
    pub(crate) fn new(provider: Arc<dyn Provider>) -> Self {
        Self {
            provider: Arc::clone(&provider),
            runner: Runner::new(Arc::clone(&provider)),
            blackboard: Mutex::new(Arc::new(Blackboard::new())),
        }
    }

    /// Run a single user query end-to-end.
    pub(crate) async fn run(&self, query: &str) -> Result<String, MossError> {
        let board = self.blackboard.lock().unwrap().clone();

        let decomposition = self.decompose(query, &board).await?;

        let board = if decomposition.is_follow_up {
            board
        } else {
            // TODO: Sealing the old blackboard

            let fresh = Arc::new(Blackboard::new());
            *self.blackboard.lock().unwrap() = Arc::clone(&fresh);
            fresh
        };

        if let Some(ref intent) = decomposition.intent {
            board.set_intent(intent.as_str());
        }

        for spec in decomposition.gaps.unwrap_or_default() {
            let gap = Gap::new(
                spec.name,
                spec.description,
                spec.gap_type,
                spec.dependencies.into_iter().map(|s| s.into_boxed_str()).collect(),
                spec.constraints,
                spec.expected_output.map(|s| s.into_boxed_str()),
            );
            board.insert_gap(gap)?;
        }

        self.runner.run(Arc::clone(&board)).await?;

        self.synthesize(&board).await
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

        if let Ok(pretty) = serde_json::to_string_pretty(&decomposition) {
            info!("Decomposed DAG:\n{}", pretty);
        }

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
