use std::io::Write;
use std::time::Duration;

use serde_json::Value;
use tokio::process::Command;
use tokio::time::timeout;
use tracing::debug;

use crate::error::MossError;

use super::blackboard::{Blackboard, Evidence, EvidenceStatus, Gap};
use super::compiler::Artifact;

// ── Executor ──────────────────────────────────────────────────────────────────

/// Stateless — no LLM, no shared state. Runs an Artifact and writes Evidence.
#[derive(Clone, Copy)]
pub(crate) struct Executor;

impl Executor {
    pub(crate) fn new() -> Self {
        Self
    }

    /// Run the Artifact for the given Gap and append Evidence to the Blackboard.
    pub(crate) async fn run(
        &self,
        gap: &Gap,
        artifact: &Artifact,
        blackboard: &Blackboard,
    ) -> Result<(), MossError> {
        match artifact {
            Artifact::Script { language, code, timeout_secs } => {
                self.run_script(gap, language, code, *timeout_secs, blackboard).await
            }
            Artifact::Agent { .. } => {
                let ev = Evidence::new(
                    gap.gap_id(),
                    self.next_attempt(gap, blackboard),
                    Value::Null,
                    EvidenceStatus::Failure {
                        reason: "agent execution not yet implemented".into(),
                    },
                );
                blackboard.append_evidence(ev);
                Ok(())
            }
        }
    }

    async fn run_script(
        &self,
        gap: &Gap,
        language: &str,
        code: &str,
        timeout_secs: u64,
        blackboard: &Blackboard,
    ) -> Result<(), MossError> {
        // Map common aliases to the real interpreter name.
        let interpreter = match language {
            "python" | "python3"         => "python3",
            "shell"  | "sh"   | "bash"   => "sh",
            "javascript" | "js"          => "node",
            other                        => other,   // pass through — LLM knows best
        };

        // Write code to a temp file with the correct extension so the interpreter
        // recognises the file type. Auto-deleted when `tmp` is dropped.
        let ext = match interpreter {
            "python3"           => ".py",
            "sh"                => ".sh",
            "node"              => ".js",
            _                   => ".tmp",
        };
        let mut tmp = tempfile::Builder::new()
            .prefix("moss-gap-")
            .suffix(ext)
            .tempfile()?;
        tmp.write_all(code.as_bytes())?;
        debug!(path = ?tmp.path(), "temp script written");

        // Spawn and wait, bounded by timeout.
        let run = Command::new(interpreter).arg(tmp.path()).output();
        let result = timeout(Duration::from_secs(timeout_secs), run).await;
        // `tmp` drops here → temp file deleted automatically (Drop trait)

        let (content, status) = match result {
            Err(_elapsed) => (
                Value::Null,
                EvidenceStatus::Failure {
                    reason: format!("timed out after {timeout_secs}s"),
                },
            ),
            Ok(Err(io_err)) => return Err(MossError::Io(io_err)),
            Ok(Ok(output)) => {
                if output.status.success() {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    match serde_json::from_str::<Value>(stdout.trim()) {
                        Ok(json)  => (json, EvidenceStatus::Success),
                        // Script ran but didn't emit JSON — store raw output as Partial.
                        Err(_)    => (
                            Value::String(stdout.trim().to_string()),
                            EvidenceStatus::Partial,
                        ),
                    }
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                    (Value::Null, EvidenceStatus::Failure { reason: stderr })
                }
            }
        };

        let ev = Evidence::new(gap.gap_id(), self.next_attempt(gap, blackboard), content, status);
        debug!(evidence = ?ev, "evidence written");
        blackboard.append_evidence(ev);

        Ok(())
    }

    fn next_attempt(&self, gap: &Gap, blackboard: &Blackboard) -> u32 {
        blackboard.get_evidence(&gap.gap_id()).len() as u32 + 1
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use crate::moss::blackboard::{Blackboard, EvidenceStatus, Gap, GapType};
    use crate::moss::signal;
    use super::{Artifact, Executor};

    fn bb() -> Blackboard { Blackboard::new(signal::channel(1).0) }

    fn make_gap(name: &str) -> Gap {
        Gap::new(name, "desc", GapType::Proactive, vec![], None, None)
    }

    fn shell_script(code: &str) -> Artifact {
        Artifact::Script {
            language: "shell".into(),
            code: code.into(),
            timeout_secs: 10,
        }
    }

    #[tokio::test]
    async fn script_success_writes_evidence() {
        let bb = bb();
        let gap = make_gap("echo_gap");
        bb.insert_gap(gap.clone()).unwrap();

        Executor::new()
            .run(&gap, &shell_script("echo '{\"ok\": true}'"), &bb)
            .await
            .unwrap();

        let evs = bb.get_evidence(&gap.gap_id());
        assert_eq!(evs.len(), 1);
        assert!(matches!(evs[0].status(), EvidenceStatus::Success));
        assert_eq!(evs[0].content()["ok"], true);
    }

    #[tokio::test]
    async fn script_non_json_stdout_is_partial() {
        let bb = bb();
        let gap = make_gap("plain_gap");
        bb.insert_gap(gap.clone()).unwrap();

        Executor::new()
            .run(&gap, &shell_script("echo 'hello world'"), &bb)
            .await
            .unwrap();

        let evs = bb.get_evidence(&gap.gap_id());
        assert!(matches!(evs[0].status(), EvidenceStatus::Partial));
    }

    #[tokio::test]
    async fn script_nonzero_exit_is_failure() {
        let bb = bb();
        let gap = make_gap("fail_gap");
        bb.insert_gap(gap.clone()).unwrap();

        Executor::new()
            .run(&gap, &shell_script("exit 1"), &bb)
            .await
            .unwrap();

        let evs = bb.get_evidence(&gap.gap_id());
        assert!(matches!(evs[0].status(), EvidenceStatus::Failure { .. }));
    }

    #[tokio::test]
    async fn attempt_increments_on_retry() {
        let bb = bb();
        let gap = make_gap("retry_gap");
        bb.insert_gap(gap.clone()).unwrap();

        let ex = Executor::new();
        ex.run(&gap, &shell_script("echo '{\"n\": 1}'"), &bb).await.unwrap();
        ex.run(&gap, &shell_script("echo '{\"n\": 2}'"), &bb).await.unwrap();

        let evs = bb.get_evidence(&gap.gap_id());
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].attempt(), 1);
        assert_eq!(evs[1].attempt(), 2);
    }
}
