pub mod cli;
pub mod error;
pub mod moss;
pub mod providers;

use std::sync::Arc;

use tokio::sync::broadcast;
use uuid::Uuid;

use error::MossError;
use moss::orchestrator::Orchestrator;
use moss::signal;
use providers::Provider;

pub struct Moss {
    orchestrator: Orchestrator,
    tx: broadcast::Sender<moss::signal::Payload>,
}

impl Moss {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        let (tx, _) = signal::channel(64);
        Self {
            orchestrator: Orchestrator::new(provider, tx.clone()),
            tx,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<moss::signal::Payload> {
        self.tx.subscribe()
    }

    pub fn approve(&self, gap_id: Uuid, approved: bool) {
        self.orchestrator.approve(gap_id, approved);
    }

    pub fn answer(&self, gap_id: Uuid, answer: String) {
        self.orchestrator.answer(gap_id, answer);
    }

    pub async fn run(&self, query: &str) -> Result<String, MossError> {
        self.orchestrator.run(query).await
    }
}
