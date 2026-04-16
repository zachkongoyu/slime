use std::sync::{Arc, Mutex};

use minijinja::{Environment, context};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tracing::{info, warn};
use crate::error::MossError;
use crate::providers::{Message, Role, Provider};

use super::artifact_guard::ArtifactGuard;
use super::blackboard::Blackboard;
use super::types::{Gap, GapState};
use super::decomposition::Decomposition;
use super::signal::{self};
use super::solver::Solver;

pub(crate) struct Orchestrator {
    provider: Arc<dyn Provider>,
    guard: Arc<ArtifactGuard>,
    environment: String,
    blackboard: Mutex<Arc<Blackboard>>,
    tx: mpsc::Sender<signal::Event>,
}

/// Probe the local system for available runtimes and report OS info.
/// Called once at startup; result is passed into `Solver::new()`.
fn detect_environment() -> String {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    let runtimes: Vec<(&str, &str)> = vec![
        ("python3", "python3 --version"),
        ("node",    "node --version"),
        ("sh",      "sh --version"),
    ];

    let mut lines = vec![format!("**OS:** {os} ({arch})")];

    let mut available = Vec::new();
    for (name, cmd) in &runtimes {
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        if let Ok(output) = std::process::Command::new(parts[0])
            .args(&parts[1..])
            .output()
        {
            if output.status.success() {
                let version = String::from_utf8_lossy(&output.stdout)
                    .trim()
                    .to_string();
                available.push(format!("{name} ({version})"));
            } else {
                available.push(name.to_string());
            }
        }
    }

    if available.is_empty() {
        lines.push("**Runtimes:** none detected".into());
    } else {
        lines.push(format!("**Runtimes:** {}", available.join(", ")));
    }

    lines.join("\n")
}

impl Orchestrator {
    pub(crate) fn new(provider: Arc<dyn Provider>, tx: mpsc::Sender<signal::Event>) -> Self {
        let guard = Arc::new(ArtifactGuard::new());
        let environment = detect_environment();
        Self {
            provider,
            guard,
            environment,
            blackboard: Mutex::new(Arc::new(Blackboard::new(tx.clone()))),
            tx,
        }
    }

    /// Run a single user query end-to-end.
    pub(crate) async fn run(&self, query: &str) -> Result<String, MossError> {
        let board = self.blackboard.lock().unwrap().clone();

        let decomposition = self.decompose(query, &board).await?;

        let board = if decomposition.is_follow_up {
            board
        } else {
            // TODO: Sealing the old blackboard

            let fresh = Arc::new(Blackboard::new(self.tx.clone()));
            *self.blackboard.lock().unwrap() = Arc::clone(&fresh);
            fresh
        };

        if let Some(ref intent) = decomposition.intent {
            board.set_intent(intent.as_str());
        }

        for spec in decomposition.gaps.unwrap_or_default() {
            let gap = Gap::new(
                spec.name,
                spec.description,
                spec.dependencies.into_iter().map(|s| s.into_boxed_str()).collect(),
                spec.constraints,
                spec.expected_output.map(|s| s.into_boxed_str()),
            );
            board.insert_gap(gap)?;
        }

        self.drive_gaps(Arc::clone(&board)).await?;

        self.synthesize(&board).await
    }

    /// Drive the Gap DAG to completion: dispatch ready gaps to the Solver.
    async fn drive_gaps(&self, blackboard: Arc<Blackboard>) -> Result<(), MossError> {
        let mut tasks: JoinSet<Result<(), MossError>> = JoinSet::new();

        loop {
            blackboard.promote_unblocked();

            for gap in blackboard.drain_ready() {
                let solver = Solver::new(
                    Arc::clone(&self.provider),
                    Arc::clone(&self.guard),
                    self.environment.clone(),
                    self.tx.clone(),
                );
                let bb = Arc::clone(&blackboard);

                // info!(gap = %gap.name(), "dispatched");

                tasks.spawn(async move {
                    solver.run(&gap, &bb).await?;

                    // let last_success = bb
                    //     .get_evidence(&gap.gap_id())
                    //     .last()
                    //     .map(|e| matches!(e.status(), EvidenceStatus::Success))
                    //     .unwrap_or(false);

                    // if last_success {
                    //     info!(gap = %gap.name(), "closed (success)");
                    // } else {
                    //     warn!(gap = %gap.name(), "solver failed — closing");
                    // }
                    bb.set_gap_state(&gap.gap_id(), GapState::Closed)
                });
            }

            if tasks.is_empty() {
                return if blackboard.all_closed() {
                    Ok(())
                } else {
                    Err(MossError::Deadlock)
                };
            }

            if let Some(result) = tasks.join_next().await {
                result.map_err(|e| MossError::Blackboard(format!("task panicked: {e}")))??;
            }
        }
    }

    /// Ask the LLM to decompose the query into a Gap DAG and insert gaps into the Blackboard.
    /// Returns the intent string and the names of the gaps created.
    pub(crate) async fn decompose(&self, query: &str, blackboard: &Blackboard) -> Result<Decomposition, MossError> {
        let template_src = include_str!("prompts/decompose.md");

        let blackboard_state = blackboard.snapshot();

        let mut env = Environment::new();
        env.add_template("decompose", template_src)
            .map_err(|e| MossError::Blackboard(format!("template error: {e}")))?;

        let tmpl = env.get_template("decompose")
            .map_err(|e| MossError::Blackboard(format!("template load error: {e}")))?;

        let rendered = tmpl
            .render(context! { user_query => query, blackboard_state => blackboard_state })
            .map_err(|e| MossError::Blackboard(format!("template render error: {e}")))?;

        let messages = vec![Message { role: Role::User, content: rendered.into_boxed_str() }];

        let raw = self.provider.complete_chat(messages).await?;

        // Strip markdown fences if the model wrapped the JSON anyway
        let json_str = raw
            .trim()
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();

        let decomposition: Decomposition = serde_json::from_str(json_str)?;

        // if let Ok(pretty) = serde_json::to_string_pretty(&decomposition) {
        //     info!("Decomposed DAG:\n{}", pretty);
        // }

        Ok(decomposition)
    }

    /// Collect evidence from the Blackboard and synthesize a final answer.
    pub(crate) async fn synthesize(&self, blackboard: &Blackboard) -> Result<String, MossError> {
        let template_src = include_str!("prompts/synthesize.md");

        let intent = blackboard
            .get_intent()
            .map(|s| s.to_string())
            .unwrap_or_else(|| "unknown intent".to_string());

        let evidence = serde_json::to_string_pretty(&blackboard.all_evidence())?;

        let mut env = Environment::new();
        env.add_template("synthesize", template_src)
            .map_err(|e| MossError::Blackboard(format!("template error: {e}")))?;

        let tmpl = env.get_template("synthesize")
            .map_err(|e| MossError::Blackboard(format!("template load error: {e}")))?;

        let rendered = tmpl
            .render(context! { intent => intent, evidence => evidence })
            .map_err(|e| MossError::Blackboard(format!("template render error: {e}")))?;

        let messages = vec![Message { role: Role::User, content: rendered.into_boxed_str() }];

        // info!("synthesizing final answer");
        let response = self.provider.complete_chat(messages).await?;

        Ok(response)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;

    use crate::error::ProviderError;
    use crate::moss::blackboard::Blackboard;
    use crate::moss::types::{Gap, GapState};
    use tokio::sync::mpsc;
    use crate::providers::{Message, Provider};

    use super::Orchestrator;

    fn bb() -> Arc<Blackboard> { Arc::new(Blackboard::new(mpsc::channel(1).0)) }

    fn orchestrator(provider: impl Provider + 'static) -> Orchestrator {
        let (tx, _rx) = mpsc::channel(16);
        Orchestrator::new(Arc::new(provider), tx)
    }

    /// Returns a done JSON — the Solver parses it and posts success evidence.
    struct AlwaysSucceedProvider;

    #[async_trait]
    impl Provider for AlwaysSucceedProvider {
        async fn complete_chat(&self, _: Vec<Message>) -> Result<String, ProviderError> {
            Ok(r#"{"step":"done","value":{"result":"ok"}}"#.into())
        }
    }

    fn gap(name: &str, deps: Vec<&str>) -> Gap {
        Gap::new(
            name,
            "test gap",
            deps.into_iter().map(|s| s.into()).collect(),
            None,
            None,
        )
    }

    #[tokio::test]
    async fn single_gap_closes_on_success() {
        let o = orchestrator(AlwaysSucceedProvider);
        let bb = bb();
        bb.insert_gap(gap("g1", vec![])).unwrap();
        o.drive_gaps(Arc::clone(&bb)).await.unwrap();
        let id = bb.get_gap_id_by_name("g1").unwrap();
        assert_eq!(bb.get_gap(&id).unwrap().state(), &GapState::Closed);
    }

    #[tokio::test]
    async fn linear_chain_closes_in_order() {
        let o = orchestrator(AlwaysSucceedProvider);
        let bb = bb();
        bb.insert_gap(gap("A", vec![])).unwrap();
        bb.insert_gap(gap("B", vec!["A"])).unwrap();
        o.drive_gaps(Arc::clone(&bb)).await.unwrap();
        let a_id = bb.get_gap_id_by_name("A").unwrap();
        let b_id = bb.get_gap_id_by_name("B").unwrap();
        assert_eq!(bb.get_gap(&a_id).unwrap().state(), &GapState::Closed);
        assert_eq!(bb.get_gap(&b_id).unwrap().state(), &GapState::Closed);
    }

    #[tokio::test]
    async fn deadlock_if_deps_never_close() {
        let o = orchestrator(AlwaysSucceedProvider);
        let bb = bb();
        bb.insert_gap(gap("B", vec!["A"])).unwrap();
        let result = o.drive_gaps(Arc::clone(&bb)).await;
        assert!(matches!(result, Err(crate::error::MossError::Deadlock)));
    }
}
