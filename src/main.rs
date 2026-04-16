use std::sync::Arc;

use moss::Moss;
use moss::cli::Cli;
use moss::providers::remote::openrouter::OpenRouter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .pretty()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("moss=info")),
        )
        .init();

    let provider = match OpenRouter::new(None, None) {
        Ok(p) => Arc::new(p),
        Err(e) => {
            tracing::error!(error = %e, "provider not configured");
            std::process::exit(1);
        }
    };

    let (moss, rx) = Moss::new(provider);
    let mut cli = Cli::new(moss, rx);

    if let Err(e) = cli.run().await {
        tracing::error!(error = %e, "fatal");
        std::process::exit(1);
    }
}

