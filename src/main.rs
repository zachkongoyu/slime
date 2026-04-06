use std::sync::Arc;

use moss::Moss;
use moss::providers::remote::openrouter::OpenRouter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .pretty()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("moss=info")),
        )
        .without_time()
        .init();

    println!("  Log level: set RUST_LOG=moss=info|debug|trace");

    println!("Moss CLI — simple interactive shell");

    let provider = match OpenRouter::new(None, None) {
        Ok(p) => {
            println!("Using OpenRouter provider");
            Arc::new(p)
        }
        Err(e) => {
            eprintln!("Provider not configured: {}. Exiting.", e);
            std::process::exit(1);
        }
    };

    let moss = Moss::new(provider);

    use tokio::io::{self, AsyncBufReadExt, BufReader};
    let stdin = io::stdin();
    let mut lines = BufReader::new(stdin).lines();

    println!("Chat loop: type a message and press Enter. Type 'exit' to quit.");

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
                    Err(e) => eprintln!("error: {e}"),
                }
            }
            Ok(None) => break,
            Err(e) => {
                eprintln!("stdin error: {e}");
                break;
            }
        }
    }

    println!("bye");
}
