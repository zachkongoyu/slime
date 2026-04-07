use std::sync::Arc;

use tokio::task::JoinSet;
use tracing::{info, warn};

use crate::error::MossError;
use crate::providers::Provider;

use super::blackboard::{Blackboard, EvidenceStatus, GapState};
use super::compiler::Compiler;
use super::executor::Executor;

/// After this many failed attempts a Gap is force-closed to prevent infinite loops.
const MAX_RETRIES: u32 = 3;

// ── Runner ────────────────────────────────────────────────────────────────────

pub(crate) struct Runner {
    compiler: Arc<Compiler>,
}

impl Runner {
    pub(crate) fn new(provider: Arc<dyn Provider>) -> Self {
        Self {
            compiler: Arc::new(Compiler::new(provider)),
        }
    }

    pub(crate) async fn run(&self, blackboard: Arc<Blackboard>) -> Result<(), MossError> {
        let mut tasks: JoinSet<Result<(), MossError>> = JoinSet::new();

        loop {
            blackboard.promote_unblocked();

            for gap in blackboard.drain_ready() {
                let compiler = Arc::clone(&self.compiler);
                let bb: Arc<Blackboard> = Arc::clone(&blackboard);

                let dispatched = gap.name().to_string();
                info!(gap = %dispatched, "dispatched");

                tasks.spawn(async move {
                    let evs = bb.get_evidence(&gap.gap_id());
                    let attempt_count = evs.len() as u32;

                    let prior: Vec<Box<str>> = evs
                        .iter()
                        .filter_map(|ev| match ev.status() {
                            EvidenceStatus::Failure { reason } => Some(reason.as_str().into()),
                            _ => None,
                        })
                        .collect();

                    if attempt_count >= MAX_RETRIES {
                        warn!(gap = %gap.name(), "max retries reached — force closing");
                        return bb.set_gap_state(&gap.gap_id(), GapState::Closed);
                    }

                    let artifact = compiler.compile(&gap, &prior).await?;
                    Executor::new().run(&gap, &artifact, &bb).await?;

                    let last_success = bb
                        .get_evidence(&gap.gap_id())
                        .last()
                        .map(|e| matches!(e.status(), EvidenceStatus::Success))
                        .unwrap_or(false);

                    if last_success {
                        info!(gap = %gap.name(), "closed (success)");
                        bb.set_gap_state(&gap.gap_id(), GapState::Closed)
                    } else {
                        warn!(gap = %gap.name(), "execution failed — will retry");
                        bb.set_gap_state(&gap.gap_id(), GapState::Ready)
                    }
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
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

    use async_trait::async_trait;

    use crate::error::ProviderError;
    use crate::moss::blackboard::{Blackboard, Gap, GapState, GapType};
    use crate::moss::signal;
    use crate::providers::{Message, Provider};

    fn bb() -> Arc<Blackboard> { Arc::new(Blackboard::new(signal::channel(1).0)) }

    use super::Runner;

    struct AlwaysSucceedProvider;

    #[async_trait]
    impl Provider for AlwaysSucceedProvider {
        async fn complete_chat(&self, _: Vec<Message>) -> Result<String, ProviderError> {
            Ok(r#"{"type":"SCRIPT","language":"shell","code":"echo '{\"result\":\"ok\"}'","timeout_secs":10}"#.into())
        }
    }

    /// Fails on the first call, succeeds on all subsequent calls.
    struct FailOnceThenSucceedProvider {
        calls: Arc<AtomicUsize>,
    }

    impl FailOnceThenSucceedProvider {
        fn new() -> Self {
            Self { calls: Arc::new(AtomicUsize::new(0)) }
        }
    }

    #[async_trait]
    impl Provider for FailOnceThenSucceedProvider {
        async fn complete_chat(&self, _: Vec<Message>) -> Result<String, ProviderError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok(r#"{"type":"SCRIPT","language":"shell","code":"exit 1","timeout_secs":10}"#.into())
            } else {
                Ok(r#"{"type":"SCRIPT","language":"shell","code":"echo '{\"result\":\"ok\"}'","timeout_secs":10}"#.into())
            }
        }
    }

    fn gap(name: &str, deps: Vec<&str>) -> Gap {
        Gap::new(
            name,
            "test gap",
            GapType::Proactive,
            deps.into_iter().map(|s| s.into()).collect(),
            None,
            None,
        )
    }

    #[tokio::test]
    async fn single_gap_closes_on_success() {
        let bb = bb();
        bb.insert_gap(gap("g1", vec![])).unwrap();

        Runner::new(Arc::new(AlwaysSucceedProvider))
            .run(Arc::clone(&bb))
            .await
            .unwrap();

        let id = bb.get_gap_id_by_name("g1").unwrap();
        assert_eq!(bb.get_gap(&id).unwrap().state(), &GapState::Closed);
    }

    #[tokio::test]
    async fn linear_chain_closes_in_order() {
        let bb = bb();
        bb.insert_gap(gap("A", vec![])).unwrap();
        bb.insert_gap(gap("B", vec!["A"])).unwrap();

        Runner::new(Arc::new(AlwaysSucceedProvider))
            .run(Arc::clone(&bb))
            .await
            .unwrap();

        let a_id = bb.get_gap_id_by_name("A").unwrap();
        let b_id = bb.get_gap_id_by_name("B").unwrap();
        assert_eq!(bb.get_gap(&a_id).unwrap().state(), &GapState::Closed);
        assert_eq!(bb.get_gap(&b_id).unwrap().state(), &GapState::Closed);
    }

    #[tokio::test]
    async fn gap_retries_after_failure() {
        let bb = bb();
        bb.insert_gap(gap("retry_gap", vec![])).unwrap();

        Runner::new(Arc::new(FailOnceThenSucceedProvider::new()))
            .run(Arc::clone(&bb))
            .await
            .unwrap();

        let id = bb.get_gap_id_by_name("retry_gap").unwrap();
        assert_eq!(bb.get_gap(&id).unwrap().state(), &GapState::Closed);
        // Two evidence entries: one failure + one success
        assert_eq!(bb.get_evidence(&id).len(), 2);
    }

    #[tokio::test]
    async fn deadlock_if_deps_never_close() {
        // B depends on A, but A is never inserted — B stays Blocked forever.
        let bb = bb();
        bb.insert_gap(gap("B", vec!["A"])).unwrap();

        let result = Runner::new(Arc::new(AlwaysSucceedProvider))
            .run(Arc::clone(&bb))
            .await;

        assert!(matches!(result, Err(crate::error::MossError::Deadlock)));
    }
}
