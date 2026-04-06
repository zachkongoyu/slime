pub mod error;
pub mod moss;
pub mod providers;

use std::sync::{Arc, Mutex};

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
    /// Swappable: replaced when the Orchestrator signals a new topic.
    blackboard: Mutex<Arc<Blackboard>>,
}

impl Moss {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self {
            orchestrator: Orchestrator::new(Arc::clone(&provider)),
            runner: Runner::new(Arc::clone(&provider)),
            blackboard: Mutex::new(Arc::new(Blackboard::new())),
        }
    }

    /// Run a single user query end-to-end:
    /// 1. Snapshot the current Blackboard state for the Orchestrator
    /// 2. Decompose — LLM sees full board state, signals follow-up or new topic
    /// 3. Swap to a fresh Blackboard if new topic
    /// 4. Refine intent on the active board
    /// 5. Insert new Gaps
    /// 6. Execute all Gaps (compile → run → retry) until closed
    /// 7. Synthesize a final answer from the collected Evidence
    pub async fn run(&self, query: &str) -> Result<String, MossError> {
        // [1] Snapshot the current board for the Orchestrator
        let board = self.blackboard.lock().unwrap().clone();

        // [2] Decompose — LLM sees the full board state and signals follow-up or new topic
        let decomposition = self.orchestrator.decompose(query, &board).await?;

        // [3] Swap board if this is a new topic
        let board = if decomposition.is_follow_up {
            board
        } else {
            // TODO: Sealing the old blackboard

            let fresh = Arc::new(Blackboard::new());
            *self.blackboard.lock().unwrap() = Arc::clone(&fresh);
            fresh
        };

        // [4] Refine intent on the active board
        if let Some(ref intent) = decomposition.intent {
            board.set_intent(intent.as_str());
        }

        // [5] Insert new Gaps
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

        // [6] Execute
        self.runner.run(Arc::clone(&board)).await?;

        // [7] Synthesize
        self.orchestrator.synthesize(&board).await
    }
}
