# Phase 8 — Unified Solver

**Status:** Ready
**ADR:** [ADR-009](../docs/ADR-009-unified-solver.md)
**Effort:** Medium. One new file, two deletions, prompt tuning.

This phase implements the unified execution model defined in ADR-009. It
collapses the old Compiler + Executor + Artifact split into a single
`Solver` component that runs a fixed-frame/mutable-memory loop for every
gap.

---

## Scope

### What Phase 8 does
- Creates `src/moss/solver.rs` — the unified loop + step parser
- Ships `src/moss/prompts/solver.md` — the fixed frame prompt
- Deletes `compiler.rs`, `executor.rs`, `compiler.md`, and the `Artifact` enum
- Wires the Orchestrator to call `Solver::run` instead of the old
  compile → scan → execute sequence
- Keeps DefenseClaw (invoked per code block inside the loop)
- Keeps HITL via `insert_gate` (now triggered by the `ask` step)
- Keeps SignalBus (unchanged)

### What Phase 8 does NOT do
- No MCP bridge. No tool-calling Provider methods. No external tool
  infrastructure of any kind.
- No persistent Python REPL. Each code block runs in a fresh process.
  Persistent state across iterations is the LLM's responsibility, via
  scratch-block working memory and/or writing to files.
- No browser session pooling. If a gap needs a browser across iterations,
  it uses `playwright`'s `storage_state` to serialize cookies/sessions.

---

## Files

| File | Action |
|------|--------|
| `src/moss/solver.rs` | **new** — Solver struct, `run` loop, step parser, scratch extraction |
| `src/moss/prompts/solver.md` | **new** — the fixed frame, already written |
| `src/moss/compiler.rs` | **delete** |
| `src/moss/prompts/compiler.md` | **delete** |
| `src/moss/prompts/compiler.xml` | **delete** if present |
| `src/moss/executor.rs` | **delete** (logic absorbed into Solver) |
| `src/moss/mod.rs` | drop `compiler`, `executor`; add `solver` |
| `src/moss/orchestrator.rs` | replace `compile + scan + execute` with `solver.run` |
| `src/moss/blackboard.rs` | optional: add `related_evidence(gap_id)` helper for context injection |
| `src/error.rs` | add `MossError::Solver(String)` variant if not present |
| `ARCHITECTURE.md` | update L2 section, Section 3 runtime loop, status table |

---

## The Solver Struct

```rust
// src/moss/solver.rs

use std::sync::Arc;
use minijinja::{Environment, context};
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::error::MossError;
use crate::providers::{Message, Provider, Role};

use super::artifact_guard::{ArtifactGuard, ScanVerdict};
use super::blackboard::{Blackboard, Evidence, EvidenceStatus, Gap};

const DEFAULT_MAX_ITERATIONS: u32 = 12;
const MAX_OUTPUT_CHARS: usize = 8_000;   // cap stdout/stderr fed back per iteration

pub(crate) struct Solver {
    provider: Arc<dyn Provider>,
    guard:    Arc<ArtifactGuard>,
}

impl Solver {
    pub(crate) fn new(provider: Arc<dyn Provider>, guard: Arc<ArtifactGuard>) -> Self {
        Self { provider, guard }
    }

    /// Drive a single gap to completion. Posts one Evidence record on exit.
    pub(crate) async fn run(
        &self,
        gap:        &Gap,
        blackboard: &Blackboard,
    ) -> Result<(), MossError> {
        let mut working_memory: String = String::new();
        let mut last_output:    String = String::new();
        let prior_errors = self.collect_prior_errors(gap, blackboard);

        for iteration in 0..DEFAULT_MAX_ITERATIONS {
            debug!(gap = %gap.name(), iteration, "solver turn");

            let prompt = self.render_prompt(gap, blackboard, &working_memory, &last_output, &prior_errors)?;
            let messages = vec![Message { role: Role::User, content: prompt.into_boxed_str() }];
            let response = self.provider.complete_chat(messages).await?;

            // Extract scratch side-channel first (orthogonal to the step).
            if let Some(new_memory) = parse_scratch(&response) {
                working_memory = new_memory;
            }

            match parse_step(&response)? {
                Step::Code { lang, code } => {
                    // DefenseClaw scan per code block
                    if let ScanVerdict::Rejected { reason } = self.guard.scan_code(&lang, &code) {
                        warn!(gap = %gap.name(), %reason, "code rejected by guard");
                        self.post_failure(gap, blackboard, &format!("guard rejected: {reason}"));
                        return Ok(());
                    }
                    last_output = self.execute_code(&lang, &code).await;
                    // loop continues
                }

                Step::Ask { question } => {
                    info!(gap = %gap.name(), "solver asked human");
                    let rx = blackboard.insert_gate(gap.gap_id(), &question);
                    let approved = rx.await.map_err(|_| MossError::Solver("gate closed".into()))?;
                    // For free-form answers, extend insert_gate to return a String;
                    // for now we model answers as a boolean approval + text via a
                    // side-channel in the Blackboard. See "Open Questions" below.
                    last_output = format!("human answered: {approved}");
                }

                Step::Done { answer } => {
                    info!(gap = %gap.name(), "solver declared done");
                    let ev = Evidence::new(
                        gap.gap_id(),
                        self.next_attempt(gap, blackboard),
                        answer,
                        EvidenceStatus::Success,
                    );
                    blackboard.append_evidence(ev);
                    return Ok(());
                }
            }
        }

        // Hit the iteration ceiling without a done.
        warn!(gap = %gap.name(), "solver exceeded max iterations");
        self.post_failure(gap, blackboard, "exceeded max iterations");
        Ok(())
    }

    // ... private helpers: render_prompt, execute_code, post_failure,
    //     next_attempt, collect_prior_errors
}
```

## The Step Enum

```rust
enum Step {
    Code { lang: String, code: String },
    Ask  { question: String },
    Done { answer: Value },
}
```

Exactly two executable variants (`Code`, `Ask`) and one terminal (`Done`).

## The Parser

```rust
fn parse_step(response: &str) -> Result<Step, MossError> {
    // 1. Look for a `done` JSON first (highest priority — terminal wins).
    if let Some(answer) = extract_done_json(response) {
        return Ok(Step::Done { answer });
    }

    // 2. Look for an `ask` block.
    if let Some(question) = extract_fenced_block(response, "ask") {
        return Ok(Step::Ask { question: question.trim().to_string() });
    }

    // 3. Look for an executable code fence (python | shell | js variants).
    if let Some((lang, code)) = extract_code_fence(response) {
        return Ok(Step::Code {
            lang: normalize_language(&lang),
            code,
        });
    }

    // 4. Ambiguous — parser could not find any recognized step.
    Err(MossError::Solver(
        "response contained no code, ask, or done block".into(),
    ))
}

fn parse_scratch(response: &str) -> Option<String> {
    extract_fenced_block(response, "scratch").map(|s| s.trim().to_string())
}
```

**Parser priority is `Done > Ask > Code`.** If the LLM accidentally emits
both a code block and a done JSON, the done wins (the LLM is signaling it
is finished and the code was supplementary). This matches the "prefer
termination" intent.

### Helper extraction functions

- `extract_done_json`: scan the response for a standalone JSON object whose
  top-level key is `"done"`. Use a small hand-rolled scanner (track brace
  depth) to avoid pulling in a full JSON parser for the whole response.
- `extract_fenced_block(response, tag)`: match ```` ```tag\n...\n``` ```` or
  ````~~~tag\n...\n~~~```` and return the inner content. Used for `ask`,
  `scratch`, and any future side-channels.
- `extract_code_fence`: match any fenced block whose info string is a
  recognized language (`python`, `py`, `shell`, `sh`, `bash`, `javascript`,
  `js`, `node`). Return `(lang, body)`.
- `normalize_language`: collapse aliases — `python3` → `python`, `sh`/`bash` → `shell`, `js`/`node` → `javascript`.

## Code Execution

Reuses the existing script-running logic from the old Executor:

```rust
async fn execute_code(&self, lang: &str, code: &str) -> String {
    // Write code to a temp file with the correct extension,
    // spawn the interpreter under a bounded tokio::time::timeout,
    // capture stdout + stderr + exit status.
    //
    // Format as feedable text:
    //
    //   [exit: 0]
    //   [stdout]
    //   ...
    //   [stderr]
    //   ...
    //
    // Truncate to MAX_OUTPUT_CHARS if either stream is too long.
}
```

Timeout defaults to 60 seconds per code block. Add a `MOSS_CODE_TIMEOUT_SECS`
env var later if needed.

## Orchestrator Wiring

```rust
// src/moss/orchestrator.rs

pub(crate) struct Orchestrator {
    provider:   Arc<dyn Provider>,
    solver:     Arc<Solver>,          // replaces compiler + guard fields
    blackboard: Mutex<Arc<Blackboard>>,
    tx:         broadcast::Sender<signal::Payload>,
}

impl Orchestrator {
    pub(crate) fn new(provider: Arc<dyn Provider>, tx: broadcast::Sender<signal::Payload>) -> Self {
        let guard = Arc::new(ArtifactGuard::new());
        Self {
            solver: Arc::new(Solver::new(Arc::clone(&provider), Arc::clone(&guard))),
            provider,
            blackboard: Mutex::new(Arc::new(Blackboard::new(tx.clone()))),
            tx,
        }
    }

    // drive_gaps body: the task closure now does:
    //
    //   let solver = Arc::clone(&self.solver);
    //   tasks.spawn(async move {
    //       solver.run(&gap, &bb).await
    //   });
    //
    // No more Compiler, no more ArtifactGuard call at this level, no more
    // Executor call.
}
```

## HITL Answer Channel

The current `insert_gate` returns `oneshot::Receiver<bool>` — approve or
reject. The Solver's `ask` step needs free-form text answers.

**Minimal change:** extend the gate machinery to carry a `String` answer:

```rust
// Old
pub(crate) fn insert_gate(&self, gap_id: Uuid, reason: &str) -> oneshot::Receiver<bool>;

// New
pub(crate) fn insert_gate(&self, gap_id: Uuid, question: &str) -> oneshot::Receiver<GateAnswer>;

pub(crate) enum GateAnswer {
    Approved,              // used by DefenseClaw's HITL path
    Rejected,              // used by DefenseClaw's HITL path
    Reply(String),         // used by Solver's ask step
}
```

The CLI's existing `approve_gate` / `reject_gate` calls become:
- `approve_gate(uuid)` sends `GateAnswer::Approved`
- `reject_gate(uuid)` sends `GateAnswer::Rejected`
- new `answer_gate(uuid, text)` sends `GateAnswer::Reply(text)`

This keeps DefenseClaw's flow unchanged and adds the text channel for the
Solver.

## Testing Strategy

**Unit tests (in `solver.rs` #[cfg(test)] module):**

Create a `MockProvider` that returns a scripted sequence of responses.
Verify each Step path end-to-end:

| Test | Mock script | Expected outcome |
|------|-------------|------------------|
| `one_shot_done_json` | `[r#"{"done": 42}"#]` | 1 LLM call, Evidence = 42 |
| `code_then_done` | `[code_block, r#"{"done": "ok"}"#]` | 2 LLM calls, code executed once |
| `code_with_scratch` | `[code + scratch]` | scratch stored, iteration 2 sees it |
| `ask_then_code_then_done` | `[ask, code, done]` | insert_gate called, then code, then evidence |
| `max_iterations_hit` | 13× code blocks, never done | Evidence is Failure("exceeded max iterations") |
| `guard_rejects_code` | `[bad_code_block]` | Evidence is Failure("guard rejected: ..."), no execution |
| `parser_ambiguous` | `["just some prose"]` | MossError::Solver returned |
| `code_fails_recovery` | `[bad_python, good_python, done]` | stderr visible in iteration 2, evidence = success |

**Parser unit tests (separate module):**

- `extract_done_json`: finds top-level `{"done": ...}`, handles nested braces, ignores code-fenced JSON
- `extract_fenced_block`: handles both ```` ``` ```` and `~~~` delimiters
- `extract_code_fence`: picks the first recognized language block, not ask/scratch
- `parser_priority`: done > ask > code when multiple present

**Integration test (mark `#[ignore]` so CI can skip without network):**

- A "real" test against OpenRouter that runs a simple gap like "compute
  compound interest, principal=1000 rate=0.05 years=3" and verifies the
  LLM actually produces a valid code block or done JSON.

## Migration Checklist

1. [ ] Write `solver.rs` with full Solver struct + helpers + tests
2. [ ] Write parser helpers with exhaustive unit tests (fail cases first)
3. [ ] Extend `Blackboard::insert_gate` to return `GateAnswer`, update DefenseClaw path
4. [ ] Add `MossError::Solver(String)` variant
5. [ ] Rewire `Orchestrator::new` and `drive_gaps` to use `Solver`
6. [ ] Delete `compiler.rs`, `executor.rs`, `compiler.md`, `compiler.xml`
7. [ ] Remove `Artifact` enum and all references
8. [ ] Update `src/moss/mod.rs` module list
9. [ ] Run all existing tests — expect Decomposer + Blackboard + Orchestrator + DefenseClaw tests to still pass; Compiler/Executor tests will be deleted and replaced by Solver tests
10. [ ] Update `ARCHITECTURE.md` Sections 2, 3, and the status matrix
11. [ ] Move `phases/phase-8-agent-loop.md` to `phases/completed/` after merge

## Open Questions (not blocking)

1. **Should `GapType` survive?** Current plan: keep it as a Solver prompt
   hint for `max_iterations`. If it never meaningfully changes behavior,
   delete in a follow-up.
2. **Persistent REPL later?** If stateless per-iteration execution proves
   too slow for heavy browser workflows, add an optional long-lived Python
   subprocess. Not Phase 8.
3. **Context window cap.** The solver prompt includes only the last output
   and working memory, not full history. If a gap exceeds the provider's
   context limit anyway, fail cleanly with a specific error. Add a
   token-counting guard in a follow-up.
4. **Solver prompt regression testing.** Without a dedicated eval harness,
   prompt quality is hard to verify. Add a small test suite of known gaps
   with expected outcomes in a future phase.
