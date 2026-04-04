pub mod error;
pub mod moss;
pub mod providers;

use std::sync::Arc;

use error::MossError;
use moss::blackboard::{Blackboard, Gap};
use moss::orchestrator::Orchestrator;
use providers::Provider;

/// The public entry point for Moss.
/// Owns the shared Blackboard and all internal components.
pub struct Moss {
    orchestrator: Orchestrator,
    blackboard: Arc<Blackboard>,
}

impl Moss {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        let blackboard = Arc::new(Blackboard::new());
        Self {
            orchestrator: Orchestrator::new(Arc::clone(&provider)),
            blackboard,
        }
    }

    /// Run a single user query:
    /// 1. Decompose into gaps (LLM)
    /// 2. Update Blackboard with intent + gaps
    /// 3. Return a summary string for the caller to display
    /// Phase 5 will trigger the Runner between steps 2 and 3.
    pub async fn run(&self, query: &str) -> Result<String, MossError> {
        let decomposition = self.orchestrator.decompose(query, &self.blackboard).await?;

        // Update blackboard with LLM output
        if let Some(ref intent) = decomposition.intent {
            self.blackboard.set_intent(intent.as_str());
        }

        let mut gap_names = Vec::new();
        if let Some(specs) = decomposition.gaps {
            for spec in specs {
                let name = spec.name.clone();
                let gap = Gap::new(
                    spec.name,
                    spec.description,
                    spec.gap_type,
                    spec.dependencies.into_iter().map(|s| s.into_boxed_str()).collect(),
                    spec.constraints,
                    spec.expected_output.map(|s| s.into_boxed_str()),
                );
                self.blackboard.insert_gap(gap)?;
                gap_names.push(name);
            }
        }

        let intent = self.blackboard
            .get_intent()
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(none)".to_string());

        if gap_names.is_empty() {
            Ok(format!("Intent: {intent}\nNo gaps — query can be answered directly."))
        } else {
            let names = gap_names.join(", ");
            Ok(format!("Intent: {intent}\nGaps: {names}"))
        }
    }
}
