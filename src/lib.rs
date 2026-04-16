pub mod cli;
pub mod error;
pub mod moss;
pub mod providers;

use std::sync::Arc;

use tokio::sync::mpsc;

use error::MossError;
use moss::orchestrator::Orchestrator;
use providers::Provider;

pub struct Moss {
    orchestrator: Orchestrator,
}

impl Moss {
    pub fn new(provider: Arc<dyn Provider>) -> (Self, mpsc::Receiver<moss::signal::Event>) {
        let (tx, rx) = mpsc::channel(64);
        (Self { orchestrator: Orchestrator::new(provider, tx) }, rx)
    }

    pub async fn run(&self, query: &str) -> Result<String, MossError> {
        self.orchestrator.run(query).await
    }
}
