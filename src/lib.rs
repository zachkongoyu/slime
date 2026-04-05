pub mod error;
pub mod moss;
pub mod providers;

use std::sync::Arc;

use error::MossError;
use moss::blackboard::{Blackboard, Gap};
use moss::orchestrator::Orchestrator;
use moss::runner::Runner;
use providers::Provider;

/// The public entry point for Moss.
/// Owns the shared Blackboard and all internal components.
pub struct Moss {
    orchestrator: Orchestrator,
    runner: Runner,
    blackboard: Arc<Blackboard>,
}

impl Moss {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        let blackboard = Arc::new(Blackboard::new());
        Self {
            orchestrator: Orchestrator::new(Arc::clone(&provider)),
            runner: Runner::new(Arc::clone(&provider)),
            blackboard,
        }
    }

    /// Run a single user query end-to-end:
    /// 1. Decompose into a Gap DAG
    /// 2. Execute all gaps (compile → run → retry) until closed
    /// 3. Synthesize a final answer from the collected Evidence
    pub async fn run(&self, query: &str) -> Result<String, MossError> {
        // 1. Decompose
        let decomposition = self.orchestrator.decompose(query, &self.blackboard).await?;

        if let Some(ref intent) = decomposition.intent {
            self.blackboard.set_intent(intent.as_str());
        }

        if let Some(specs) = decomposition.gaps {
            for spec in specs {
                let gap = Gap::new(
                    spec.name,
                    spec.description,
                    spec.gap_type,
                    spec.dependencies.into_iter().map(|s| s.into_boxed_str()).collect(),
                    spec.constraints,
                    spec.expected_output.map(|s| s.into_boxed_str()),
                );
                self.blackboard.insert_gap(gap)?;
            }
        }

        // 2. Execute
        self.runner.run(Arc::clone(&self.blackboard)).await?;

        // 3. Synthesize
        self.orchestrator.synthesize(&self.blackboard).await
    }
}
