use tokio::io::AsyncBufReadExt;
use tokio::sync::broadcast;

use crate::error::MossError;
use crate::Moss;
use crate::moss::signal;

pub struct Cli {
    moss: Moss,
    rx: broadcast::Receiver<signal::Payload>,
}

impl Cli {
    pub fn new(moss: Moss) -> Self {
        let rx = moss.subscribe();
        Self { moss, rx }
    }

    pub async fn run(&mut self) -> Result<(), MossError> {
        let stdin = tokio::io::stdin();
        let mut lines = tokio::io::BufReader::new(stdin).lines();

        loop {
            match lines.next_line().await? {
                Some(raw) => self.handle_input(raw.trim_end()).await?,
                None => break,
            }
        }

        Ok(())
    }

    async fn handle_input(&mut self, input: &str) -> Result<(), MossError> {
        match input {
            "" => {}
            "exit" | "quit" => std::process::exit(0),
            query => {
                tokio::pin!(let fut = self.moss.run(query););
                loop {
                    tokio::select! {
                        result = &mut fut => {
                            match result {
                                Ok(response) => println!("{response}"),
                                Err(e) => eprintln!("[moss] error: {e}"),
                            }
                            break;
                        }
                        signal = self.rx.recv() => match signal {
                            Ok(snapshot) => tracing::debug!(snapshot = %snapshot, "board updated"),
                            Err(broadcast::error::RecvError::Lagged(n)) => {
                                tracing::warn!(skipped = n, "signal bus lagged");
                            }
                            Err(broadcast::error::RecvError::Closed) => break,
                        },
                    }
                }
            }
        }
        Ok(())
    }
}
