use std::sync::Arc;

use moss::Moss;
use moss::providers::remote::openrouter::OpenRouter;

#[tokio::main]
async fn main() {
    // Configure tracing to output structured JSON for better observability
    tracing_subscriber::fmt()
        .pretty()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("moss=info")),
        )
        .init();

    tracing::info!("Moss CLI started. Set RUST_LOG=moss=info|debug|trace to change log level.");

    let provider = match OpenRouter::new(None, None) {
        Ok(p) => {
            tracing::info!("Using OpenRouter provider");
            Arc::new(p)
        }
        Err(e) => {
            tracing::error!(error = %e, "Provider not configured. Exiting.");
            std::process::exit(1);
        }
    };

    let moss = Moss::new(provider);

    use tokio::io::{self, AsyncBufReadExt, BufReader};
    let stdin = io::stdin();
    let mut lines = BufReader::new(stdin).lines();

    tracing::info!("Chat loop: type a message and press Enter. Type 'exit' to quit.");

    loop {
        match lines.next_line().await {
            Ok(Some(raw)) => {
                let input = raw.trim_end().to_string();
                if input == "exit" || input == "quit" {
                    break;
                }
                if input.is_empty() {
                    continue;
                }

                match moss.run(&input).await {
                    Ok(msg) => println!("{msg}"),
                    Err(e) => tracing::error!(error = %e, "Failed to run moss"),
                }
            }
            Ok(None) => break,
            Err(e) => {
                tracing::error!(error = %e, "stdin error");
                break;
            }
        }
    }

    tracing::info!("bye");
}
