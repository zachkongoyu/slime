# ADR-010 — Solver State Observability via Blackboard

**Status:** Proposed
**Date:** 2026-04-14

---

## Context

Gaps in `Assigned` state are a black box. The Blackboard knows a solver is running but nothing about what it is doing — which iteration, what step, what the last result was. During a real run, the CLI shows "dispatched" and then silence for 10–30 seconds until "closed (done)".

The existing event bus (`Event::Snapshot`) already carries full `BlackboardSnapshot` payloads on every Blackboard mutation. But `BlackboardSnapshot` contains only `intent`, `gaps`, and `evidences` — no solver-level state. Consumers (CLI today, frontend later) cannot render solver progress because the data does not exist in the snapshot.

The Solver is ephemeral — constructed per-gap inside `tokio::spawn` in `drive_gaps`, dropped when it returns. No external code holds a reference to it.

## Decision

**Add a `SolverState` struct to the Blackboard. The Solver owns its state locally and pushes it to a `DashMap<Uuid, SolverState>` on the Blackboard at each state transition. Each push triggers a snapshot emission via the existing event bus.**

### New types (in `blackboard.rs`)

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum StepKind {
    Prompting,
    Executing { interpreter: Box<str> },
    Waiting,
    Finished,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SolverState {
    gap_id: Uuid,
    iteration: u32,
    max_iterations: u32,
    step: StepKind,
    last_result: Option<Box<str>>,
}
```

### Blackboard changes

New field:

```rust
solver_states: DashMap<Uuid, SolverState>,
```

New methods (same mutation + emit pattern as `set_gap_state`, `append_evidence`, etc.):

```rust
pub(crate) fn update_solver_state(&self, state: SolverState) {
    self.solver_states.insert(state.gap_id, state);
    let _ = self.tx.send(self.snapshot_json());
}

pub(crate) fn remove_solver_state(&self, gap_id: &Uuid) {
    self.solver_states.remove(gap_id);
    let _ = self.tx.send(self.snapshot_json());
}
```

### Snapshot change

```rust
pub(crate) struct BlackboardSnapshot {
    intent: Option<Box<str>>,
    gaps: HashMap<Uuid, Gap>,
    evidences: HashMap<Uuid, Vec<Evidence>>,
    solver_states: HashMap<Uuid, SolverState>,  // new
}
```

### Solver emission points

The Solver keeps a local `SolverState`, mutates it, and calls `blackboard.update_solver_state(state.clone())` at each transition:

| Moment | `step` | `last_result` |
|--------|--------|---------------|
| Top of iteration, before LLM call | `Prompting` | unchanged |
| After parsing `Code` step, before execution | `Executing { interpreter }` | unchanged |
| After code execution completes | `Executing { interpreter }` | `"exit 0"` / `"exit 1"` / `"timeout"` / `"guard rejected"` |
| After parsing `Ask` step | `Waiting` | unchanged |
| After receiving human answer | `Prompting` | `"answered"` |
| On `Done` or iteration exhaustion | `Finished` | `"done"` / `"exhausted"` |

`blackboard.remove_solver_state(&gap_id)` is called right before `Solver::run()` returns, so the snapshot reflects only active solvers.

## Alternatives considered

**Watch channels (Solver owns `watch::Sender`, Blackboard reads `watch::Receiver`).**
Eliminates explicit push calls and provides automatic dead-solver detection. Rejected because it loses snapshot emission on each state change — the Blackboard would only see updated solver state passively when some other mutation triggers a snapshot.

**Separate `Event` variants for solver lifecycle (`SolverStarted`, `SolverIteration`, etc.).**
Creates a parallel state channel alongside snapshots. Consumers must reconcile two streams to build a complete picture. Rejected in favor of one unified snapshot that carries everything.

**Tracing-only (no event bus, just structured logs).**
Adequate for operators but invisible to the CLI and future frontends. Does not solve the stated problem.

## Consequences

**Positive:**

- One snapshot payload contains everything a consumer needs: intent, gaps, evidence, and solver progress. No stream reconciliation.
- Late-joining consumers get full state on the first snapshot.
- Follows the existing Blackboard mutation pattern — no new concurrency primitives, no new event types.

**Negative:**

- Snapshot frequency increases. Solvers emit ~6 state updates per iteration × up to 10 iterations × N concurrent solvers. For 3 solvers averaging 3 iterations, that is ~54 additional snapshots per run. The broadcast channel must not lag.
- `last_result` is a compact summary string, not the full stdout. If a consumer needs full output, that requires a different mechanism (not in scope).

**Neutral:**

- No changes to `Event` variants, event bus, or Orchestrator.
- `ApprovalRequested` and `QuestionAsked` events remain separate — they serve the attention/input flow, not the state flow.

## Implementation scope

Two files: `src/moss/blackboard.rs` and `src/moss/solver.rs`.
