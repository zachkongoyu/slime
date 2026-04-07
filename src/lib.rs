pub mod error;
pub mod moss;
pub mod providers;

use std::sync::Arc;

use error::MossError;
use moss::orchestrator::Orchestrator;
use providers::Provider;

/// The public entry point for Moss.
/// Owns the Orchestrator, which acts as the strategic coordinator.
pub struct Moss {
    orchestrator: Orchestrator,
}

impl Moss {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self {
            orchestrator: Orchestrator::new(provider),
        }
    }

    /// Run a single user query end-to-end:
    /// Triggers the full decomposition, execution, and synthesis loop inside the Orchestrator.
    pub async fn run(&self, query: &str) -> Result<String, MossError> {
        self.orchestrator.run(query).await
    }
}
