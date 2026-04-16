# ADR-010 — Solver State Observability via Event Bus

**Status:** Accepted
**Date:** 2026-04-16

---

## Context

Gaps in `Assigned` state are a black box. The Blackboard knows a solver is running but nothing about what it is doing — which iteration, what step, what the last result was. During a real run, the CLI shows "dispatched" and then silence for 10–30 seconds until "closed (done)".

The existing event bus carries `Event::Snapshot` (full Blackboard state on every mutation), `Event::ApprovalRequested`, and `Event::QuestionAsked`. The Solver already sends `ApprovalRequested` and `QuestionAsked` directly via `blackboard.signal_tx().send(...)` — bypassing Blackboard storage entirely for ephemeral attention events.

The Solver is ephemeral — constructed per-gap inside `tokio::spawn` in `drive_gaps`, dropped when it returns. Solver progress is equally ephemeral: once a gap closes, its solver state is meaningless. Storing it on the Blackboard would require explicit cleanup (`remove_solver_state`) and bloat the Blackboard's responsibility.

## Decision

**Add `Event::SolverProgress` to the signal bus. The Solver holds a cloned `broadcast::Sender` and fires progress events directly — no storage on the Blackboard, no cleanup responsibility.**

### Signal change (in `signal.rs`)

```rust
pub enum Event {
    Snapshot(Box<str>),
    ApprovalRequested { gap_id: Uuid, gap_name: Box<str>, reason: Box<str> },
    QuestionAsked     { gap_id: Uuid, gap_name: Box<str>, question: Box<str> },
    SolverProgress    {              // new
        gap_id:        Uuid,
        gap_name:      Box<str>,
        iteration:     u32,
        max_iterations: u32,
        step:          Box<str>,           // "prompting" | "code: python3" | "ask" | "done"
        last_result:   Option<Box<str>>,   // "exit 0" | "exit 1" | "timeout" | "guard rejected" | "answered" | "done" | "exhausted"
    },
}
```

### Solver struct change (in `solver.rs`)

```rust
pub(crate) struct Solver {
    provider:    Arc<dyn Provider>,
    guard:       Arc<ArtifactGuard>,
    environment: String,
    tx:          broadcast::Sender<signal::Payload>,  // new
}
```

The Orchestrator passes `self.tx.clone()` at construction in `drive_gaps`. The Solver continues to take `&Blackboard` in `run()` for evidence, approvals, and questions — only the progress notifications go via `self.tx`.

### Step label (in `solver.rs`)

Rather than a separate enum, `Step` gains a display method:

```rust
impl Step {
    fn label(&self) -> Box<str> {
        match self {
            Step::Code { interpreter, .. } => format!("code: {interpreter}").into(),
            Step::Ask  { .. }              => "ask".into(),
            Step::Done { .. }              => "done".into(),
        }
    }
}
```

### Solver emission points

The Solver constructs `SolverProgress` from local variables and calls `self.tx.send(...)` at each transition:

| Moment | `step` | `last_result` |
|--------|--------|---------------|
| Top of iteration, before LLM call | `"prompting"` | unchanged |
| After parsing `Code` step, before execution | `"code: {interpreter}"` | unchanged |
| After code execution completes | `"code: {interpreter}"` | `"exit 0"` / `"exit 1"` / `"timeout"` / `"guard rejected"` |
| After parsing `Ask` step | `"ask"` | unchanged |
| After receiving human answer | `"prompting"` | `"answered"` |
| On `Done` or iteration exhaustion | `"done"` | `"done"` / `"exhausted"` |

No cleanup call is needed. When the Solver drops, its `tx` clone is released. The consumer sees the gap transition to `Closed` via the next `Snapshot` and discards its local solver state entry.

## Alternatives considered

**Store solver state on the Blackboard (`DashMap<Uuid, SolverState>` + `remove_solver_state`).**
Reuses the existing snapshot emission path. Rejected because it grows the Blackboard with data that does not belong there. The Blackboard outlives every Solver; an insert-then-remove pattern creates a cleanup obligation and a leak risk on solver panic. Solver progress is observer data, not workspace state.

**Watch channels (Solver owns `watch::Sender`, Blackboard reads `watch::Receiver`).**
Provides automatic dead-solver detection. Rejected because it requires the Blackboard to poll or subscribe to per-solver channels, adding coupling and complexity.

**Tracing-only (no event bus, just structured logs).**
Adequate for operators but invisible to the CLI and future frontends. Does not solve the stated problem.

## Consequences

**Positive:**

- Blackboard is not modified. Its invariant — durable workspace state only — is preserved.
- No cleanup. The Solver dying is sufficient; the consumer reconciles via the next `Snapshot`.
- Follows the established pattern: `ApprovalRequested` and `QuestionAsked` already fire directly from the Solver via `signal_tx()`. `SolverProgress` is the same pattern.
- `Step::label()` eliminates a redundant enum (`StepKind`) and keeps the step display co-located with the step definition.

**Negative:**

- A consumer that joins mid-run has no solver progress history — it must wait for the next `SolverProgress` event to populate its local map. This is acceptable: solver progress is inherently live data, not replay data.
- `last_result` is a compact summary string, not full stdout. Full output requires a separate mechanism (not in scope).

**Neutral:**

- `Snapshot` frequency is unchanged. Progress events are additive, not substitutive.
- `ApprovalRequested` and `QuestionAsked` continue unchanged — they serve the attention/input flow.
- `BlackboardSnapshot` is unchanged.

## Implementation scope

Three files: `src/moss/signal.rs`, `src/moss/solver.rs`, `src/moss/orchestrator.rs`.
