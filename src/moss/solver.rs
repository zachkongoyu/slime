use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use minijinja::{Environment, context};
use serde::Deserialize;
use serde_json::Value;
use tokio::process::Command;
use tokio::sync::oneshot;
use tokio::time::timeout;
use tracing::{debug, info, warn};

use crate::error::MossError;
use crate::providers::{Message, Role, Provider};

use super::artifact_guard::{ArtifactGuard, ScanVerdict};
use super::blackboard::{Blackboard, Evidence, EvidenceStatus, Gap};
use super::signal::Event;

/// Maximum iterations before the Solver force-stops a gap.
const MAX_ITERATIONS: u32 = 10;

/// Maximum execution time for a single code block.
const CODE_TIMEOUT_SECS: u64 = 120;

// ── Step Model ────────────────────────────────────────────────────────────────

/// JSON envelope returned by the LLM on every turn.
#[derive(Debug, Deserialize)]
#[serde(tag = "step", rename_all = "lowercase")]
enum Step {
    Code { interpreter: String, ext: String, code: String },
    Ask { question: String },
    Done { value: Value },
}

/// Wrapper that includes the optional scratch field.
#[derive(Debug, Deserialize)]
struct SolverResponse {
    #[serde(flatten)]
    step: Step,
    scratch: Option<String>,
}

// ── Solver ────────────────────────────────────────────────────────────────────

pub(crate) struct Solver {
    provider: Arc<dyn Provider>,
    guard: Arc<ArtifactGuard>,
    environment: String,
}

impl Solver {
    pub(crate) fn new(provider: Arc<dyn Provider>, guard: Arc<ArtifactGuard>, environment: String) -> Self {
        Self { provider, guard, environment }
    }

    /// Run the unified solver loop for a single Gap.
    ///
    /// The loop renders the prompt, calls the LLM, parses the response into a
    /// Step, and acts accordingly.  Iterates until the LLM emits a `Done` step
    /// or `MAX_ITERATIONS` is reached.
    pub(crate) async fn run(&self, gap: &Gap, blackboard: &Blackboard) -> Result<(), MossError> {
        let mut working_memory = String::new();
        let mut last_output: Option<String> = None;

        for iteration in 0..MAX_ITERATIONS {
            let related_evidence = self.gather_related_evidence(gap, blackboard);

            let prompt = self.render_prompt(
                gap,
                &working_memory,
                last_output.as_deref(),
                &related_evidence,
            )?;

            let messages = vec![Message { role: Role::User, content: prompt.into_boxed_str() }];
            let response = self.provider.complete_chat(messages).await?;

            debug!(gap = %gap.name(), iteration, "solver response received");

            let parsed: SolverResponse = match parse_response(&response) {
                Ok(r) => r,
                Err(_) => {
                    warn!(gap = %gap.name(), iteration, "unparseable response — re-prompting");
                    last_output = Some(
                        "ERROR: your previous response was not valid JSON. Respond with a single JSON object: {\"step\":\"code\",...}, {\"step\":\"ask\",...}, or {\"step\":\"done\",...}".into()
                    );
                    continue;
                }
            };

            if let Some(ref scratch) = parsed.scratch {
                if !working_memory.is_empty() {
                    working_memory.push('\n');
                }
                working_memory.push_str(scratch);
            }

            match parsed.step {
                Step::Code { interpreter, ext, code } => {
                    match self.guard.scan_code(&code) {
                        ScanVerdict::Approved => {}
                        ScanVerdict::Rejected { reason } => {
                            warn!(gap = %gap.name(), %reason, "rejected by guard");
                            last_output = Some(format!("ERROR: code rejected by guard: {reason}"));
                            continue;
                        }
                        ScanVerdict::Gated { reason } => {
                            let (tx, rx) = oneshot::channel();
                            blackboard.register_approval(gap.gap_id(), tx);
                            let _ = blackboard.signal_tx().send(Event::ApprovalRequested {
                                gap_id: gap.gap_id(),
                                gap_name: gap.name().into(),
                                reason: reason.clone(),
                            });

                            let approved = rx.await.unwrap_or(false);
                            if !approved {
                                warn!(gap = %gap.name(), "guard denied by user");
                                last_output = Some(format!("ERROR: code denied by user: {reason}"));
                                continue;
                            }
                        }
                    }

                    match self.execute_code(&interpreter, &ext, &code).await {
                        Ok(stdout) => {
                            last_output = Some(stdout);
                        }
                        Err(e) => {
                            last_output = Some(format!("ERROR: execution error: {e}"));
                        }
                    }
                }

                Step::Ask { question } => {
                    let (tx, rx) = oneshot::channel();
                    blackboard.register_question(gap.gap_id(), tx);
                    let _ = blackboard.signal_tx().send(Event::QuestionAsked {
                        gap_id: gap.gap_id(),
                        gap_name: gap.name().into(),
                        question: question.into(),
                    });

                    let answer = rx.await.unwrap_or_else(|_| "(no answer received)".into());
                    last_output = Some(format!("Human answered: {answer}"));
                }

                Step::Done { value } => {
                    blackboard.append_evidence(Evidence::new(
                        gap.gap_id(),
                        value,
                        EvidenceStatus::Success,
                    ));
                    info!(gap = %gap.name(), "closed (done)");
                    return Ok(());
                }
            }
        }

        // Exhausted all iterations — post failure evidence.
        blackboard.append_evidence(Evidence::new(
            gap.gap_id(),
            Value::Null,
            EvidenceStatus::Failure {
                reason: format!("solver exhausted {MAX_ITERATIONS} iterations"),
            },
        ));
        warn!(gap = %gap.name(), "solver exhausted iterations");
        Ok(())
    }

    fn render_prompt(
        &self,
        gap: &Gap,
        working_memory: &str,
        last_output: Option<&str>,
        related_evidence: &str,
    ) -> Result<String, MossError> {
        let template_src = include_str!("prompts/solver.md");

        let wm = if working_memory.is_empty() {
            "(empty — this is iteration 1)"
        } else {
            working_memory
        };

        let constraints_str = gap
            .constraints()
            .map(|c| serde_json::to_string_pretty(c).unwrap_or_else(|_| c.to_string()));

        let gap_ctx = context! {
            description => gap.description(),
            expected_output => gap.expected_output(),
            constraints => constraints_str,
        };

        let mut env = Environment::new();
        env.add_template("solver", template_src)
            .map_err(|e| MossError::Blackboard(format!("template error: {e}")))?;

        let tmpl = env
            .get_template("solver")
            .map_err(|e| MossError::Blackboard(format!("template load error: {e}")))?;

        let rendered = tmpl
            .render(context! {
                gap => gap_ctx,
                environment => &self.environment,
                related_evidence => if related_evidence.is_empty() { None } else { Some(related_evidence) },
                working_memory => wm,
                last_output => last_output,
            })
            .map_err(|e| MossError::Blackboard(format!("template render error: {e}")))?;

        Ok(rendered)
    }

    fn gather_related_evidence(&self, gap: &Gap, blackboard: &Blackboard) -> String {
        let mut parts = Vec::new();
        for dep_name in gap.dependencies() {
            if let Some(dep_id) = blackboard.get_gap_id_by_name(dep_name) {
                for ev in blackboard.get_evidence(&dep_id) {
                    if matches!(ev.status(), EvidenceStatus::Success) {
                        parts.push(format!("**{}:** {}", dep_name, ev.content()));
                    }
                }
            }
        }
        parts.join("\n\n")
    }

    async fn execute_code(&self, interpreter: &str, ext: &str, code: &str) -> Result<String, MossError> {
        let mut tmp = tempfile::Builder::new()
            .prefix("moss-solver-")
            .suffix(ext)
            .tempfile()?;
        tmp.write_all(code.as_bytes())?;
        debug!(path = ?tmp.path(), "temp script written");

        let run = Command::new(interpreter).arg(tmp.path()).output();
        let result = timeout(Duration::from_secs(CODE_TIMEOUT_SECS), run).await;

        match result {
            Err(_) => Ok(format!("ERROR: execution timed out after {CODE_TIMEOUT_SECS}s")),
            Ok(Err(io_err)) => Err(MossError::Io(io_err)),
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();

                if output.status.success() {
                    if stderr.is_empty() {
                        Ok(stdout)
                    } else {
                        Ok(format!("{stdout}\n[stderr]: {stderr}"))
                    }
                } else {
                    Ok(format!(
                        "EXIT CODE: {}\n[stdout]: {stdout}\n[stderr]: {stderr}",
                        output.status
                    ))
                }
            }
        }
    }
}

// ── Response Parser ───────────────────────────────────────────────────────────

/// Strip markdown fences that some models wrap around JSON output.
fn strip_json_fences(response: &str) -> &str {
    let trimmed = response.trim();
    trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|s| s.strip_suffix("```"))
        .map(|s| s.trim())
        .unwrap_or(trimmed)
}

fn parse_response(response: &str) -> Result<SolverResponse, serde_json::Error> {
    let clean = strip_json_fences(response);
    serde_json::from_str(clean)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use serde_json::json;

    use crate::error::ProviderError;
    use crate::moss::blackboard::{Blackboard, EvidenceStatus, Gap};
    use crate::moss::signal;
    use crate::providers::{Message, Provider};

    use super::*;

    // ── Parser unit tests ────────────────────────────────────────────────────

    #[test]
    fn parse_code_step() {
        let response = r#"{"step":"code","interpreter":"python3","ext":".py","code":"print('hello')\n"}"#;
        let parsed = parse_response(response).unwrap();
        match parsed.step {
            Step::Code { interpreter, ext, code } => {
                assert_eq!(interpreter, "python3");
                assert_eq!(ext, ".py");
                assert_eq!(code, "print('hello')\n");
            }
            _ => panic!("expected Code step"),
        }
        assert!(parsed.scratch.is_none());
    }

    #[test]
    fn parse_shell_step() {
        let response = r#"{"step":"code","interpreter":"sh","ext":".sh","code":"echo hello\n"}"#;
        let parsed = parse_response(response).unwrap();
        match parsed.step {
            Step::Code { interpreter, ext, .. } => {
                assert_eq!(interpreter, "sh");
                assert_eq!(ext, ".sh");
            }
            _ => panic!("expected Code step"),
        }
    }

    #[test]
    fn parse_ask_step() {
        let response = r#"{"step":"ask","question":"Which file should I modify?"}"#;
        let parsed = parse_response(response).unwrap();
        match parsed.step {
            Step::Ask { question } => assert_eq!(question, "Which file should I modify?"),
            _ => panic!("expected Ask step"),
        }
    }

    #[test]
    fn parse_done_step() {
        let response = r#"{"step":"done","value":{"result":42}}"#;
        let parsed = parse_response(response).unwrap();
        match parsed.step {
            Step::Done { value } => assert_eq!(value, json!({"result": 42})),
            _ => panic!("expected Done step"),
        }
    }

    #[test]
    fn parse_done_with_scratch() {
        let response = r#"{"step":"done","value":{"price":67432.50},"scratch":"final price obtained"}"#;
        let parsed = parse_response(response).unwrap();
        match parsed.step {
            Step::Done { value } => assert_eq!(value["price"], 67432.50),
            _ => panic!("expected Done step"),
        }
        assert_eq!(parsed.scratch.as_deref(), Some("final price obtained"));
    }

    #[test]
    fn parse_code_with_scratch() {
        let response = r#"{"step":"code","interpreter":"python3","ext":".py","code":"print(1)\n","scratch":"progress: step 1 done"}"#;
        let parsed = parse_response(response).unwrap();
        assert!(matches!(parsed.step, Step::Code { .. }));
        assert_eq!(parsed.scratch.as_deref(), Some("progress: step 1 done"));
    }

    #[test]
    fn parse_fenced_json_stripped() {
        let response = "```json\n{\"step\":\"done\",\"value\":{\"result\":1}}\n```";
        let parsed = parse_response(response).unwrap();
        match parsed.step {
            Step::Done { value } => assert_eq!(value, json!({"result": 1})),
            _ => panic!("expected Done step"),
        }
    }

    #[test]
    fn parse_invalid_json_is_err() {
        let response = "I don't have any code to run, the answer is 42.";
        assert!(parse_response(response).is_err());
    }

    // ── Solver integration tests ─────────────────────────────────────────────

    fn bb() -> Blackboard {
        Blackboard::new(signal::channel(1).0)
    }

    fn make_gap(name: &str) -> Gap {
        Gap::new(name, "Test description", vec![], None, None)
    }

    struct DoneProvider;

    #[async_trait]
    impl Provider for DoneProvider {
        async fn complete_chat(&self, _: Vec<Message>) -> Result<String, ProviderError> {
            Ok(r#"{"step":"done","value":{"result":"ok"}}"#.into())
        }
    }

    struct FailThenDoneProvider {
        calls: AtomicUsize,
    }

    impl FailThenDoneProvider {
        fn new() -> Self {
            Self { calls: AtomicUsize::new(0) }
        }
    }

    #[async_trait]
    impl Provider for FailThenDoneProvider {
        async fn complete_chat(&self, _: Vec<Message>) -> Result<String, ProviderError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok(r#"{"step":"code","interpreter":"sh","ext":".sh","code":"exit 1\n"}"#.into())
            } else {
                Ok(r#"{"step":"done","value":{"result":"recovered"}}"#.into())
            }
        }
    }

    #[tokio::test]
    async fn solver_done_posts_success_evidence() {
        let bb = bb();
        let gap = make_gap("simple_gap");
        bb.insert_gap(gap.clone()).unwrap();

        let guard = Arc::new(ArtifactGuard::new());
        let solver = Solver::new(Arc::new(DoneProvider), guard, "test".into());
        solver.run(&gap, &bb).await.unwrap();

        let evs = bb.get_evidence(&gap.gap_id());
        assert_eq!(evs.len(), 1);
        assert!(matches!(evs[0].status(), EvidenceStatus::Success));
        assert_eq!(evs[0].content()["result"], "ok");
    }

    struct ExhaustedProvider;

    #[async_trait]
    impl Provider for ExhaustedProvider {
        async fn complete_chat(&self, _: Vec<Message>) -> Result<String, ProviderError> {
            // Return code that always fails — forces iteration until exhaustion.
            Ok(r#"{"step":"code","interpreter":"sh","ext":".sh","code":"exit 1\n"}"#.into())
        }
    }

    #[tokio::test]
    async fn solver_exhausts_iterations() {
        let bb = bb();
        let gap = make_gap("hard_gap");
        bb.insert_gap(gap.clone()).unwrap();

        let guard = Arc::new(ArtifactGuard::new());
        let solver = Solver::new(Arc::new(ExhaustedProvider), guard, "test".into());
        solver.run(&gap, &bb).await.unwrap();

        let evs = bb.get_evidence(&gap.gap_id());
        assert!(!evs.is_empty());
        let last = evs.last().unwrap();
        assert!(matches!(last.status(), EvidenceStatus::Failure { .. }));
    }

    #[test]
    fn guard_rejection_surfaces_as_error() {
        let response = r#"{"step":"code","interpreter":"python3","ext":".py","code":"import os\nos.listdir('.')\n"}"#;
        let parsed = parse_response(response).unwrap();
        match &parsed.step {
            Step::Code { code, .. } => {
                let guard = ArtifactGuard::new();
                let verdict = guard.scan_code(code);
                assert!(matches!(verdict, ScanVerdict::Rejected { .. }));
            }
            _ => panic!("expected Code step"),
        }
    }
}
