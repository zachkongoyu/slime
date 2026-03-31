use moss::providers::remote::openrouter::OpenRouter;
use moss::providers::{DynProvider, Message, Role};
use moss::moss::orchestrator::Orchestrator;
use moss::moss::blackboard::Blackboard;

#[tokio::main]
async fn main() {
    println!("Moss CLI — simple interactive shell");

    let provider: DynProvider = match OpenRouter::new(None, None) {
        Ok(p) => {
            println!("Using OpenRouter provider");
            Box::new(p) as DynProvider
        }
        Err(e) => {
            eprintln!("Provider not configured: {}. Exiting.", e);
            std::process::exit(1);
        }
    };

    let orchestrator = Orchestrator::new(provider);

    use tokio::io::{self, AsyncBufReadExt, BufReader};

    let stdin = io::stdin();
    let mut lines = BufReader::new(stdin).lines();

    println!("Chat loop: type messages and press Enter to send. Type 'exit' to quit.");

    loop {
        match lines.next_line().await {
            Ok(Some(raw)) => {
                let input = raw.trim_end();
                if input == "exit" || input == "quit" {
                    break;
                }
                if input.is_empty() {
                    continue;
                }

                let resp = orchestrator.synthesize(input, &Blackboard::new()).await;
                // println!("=> {}", resp);
            }
            Ok(None) => break, // EOF
            Err(e) => {
                eprintln!("stdin error: {}", e);
                break;
            }
        }
    }

    println!("bye");
}
