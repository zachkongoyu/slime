# ADR-011 — Single mpsc Channel for All Events

**Status:** Accepted
**Date:** 2026-04-16

---

## Context

ADR-010 introduced `Event::SolverProgress` on a `broadcast` bus. HITL interactions (ArtifactGuard gating and Solver questions) were routed through the Blackboard: the Solver parked a `oneshot::Sender` in a `DashMap`, emitted a bare gap_id on the broadcast bus, and the CLI called `Moss::approve` / `Moss::answer` to thread the response back through Orchestrator → Blackboard → DashMap → Solver.

This created four problems:

1. **`broadcast` requires `T: Clone`.** A `oneshot::Sender` is not `Clone`, so it cannot travel with the event. The DashMap exists solely to work around this.
2. **Interaction state on the Blackboard.** `pending_approvals` and `pending_questions` DashMaps hold ephemeral, per-gap-run state that belongs to the Solver's execution, not to the shared workspace.
3. **Side-channel methods.** `approve` and `answer` on `Moss` and `Orchestrator` exist only to route responses back to a DashMap slot.
4. **Two consumers assumed.** `broadcast` is designed for multiple subscribers. Moss has exactly one consumer: the CLI.

## Decision

**Replace `broadcast` with a single `mpsc::channel<Event>`. Add `Approval` and `Question` as variants of `Event`, each carrying the `oneshot::Sender` directly. Remove the Blackboard DashMaps and all side-channel methods.**

### `Event` (in `signal.rs`)

```rust
#[derive(Debug)]
pub enum Event {
    Snapshot(Box<str>),
    SolverProgress {
        gap_id:         Uuid,
        gap_name:       Box<str>,
        iteration:      u32,
        max_iterations: u32,
        step:           Box<str>,
        last_result:    Option<Box<str>>,
    },
    Approval {
        gap_id:   Uuid,
        gap_name: Box<str>,
        reason:   Box<str>,
        tx:       oneshot::Sender<bool>,
    },
    Question {
        gap_id:   Uuid,
        gap_name: Box<str>,
        question: Box<str>,
        tx:       oneshot::Sender<String>,
    },
}
```

`Event` is not `Clone`. That is correct: `Approval` and `Question` are single-consumer by construction.

### Channel threading

`Moss::new()` creates the channel and returns the receiver to the caller:

```rust
pub fn new(provider: Arc<dyn Provider>) -> (Self, mpsc::Receiver<Event>) {
    let (tx, rx) = mpsc::channel(64);
    (Self { orchestrator: Orchestrator::new(provider, tx) }, rx)
}
```

`Orchestrator` holds `mpsc::Sender<Event>` and clones it into each `Solver::new()`. The Solver holds a single `tx: mpsc::Sender<Event>` for all emission — progress, approvals, and questions.

### Solver HITL

```rust
// Approval
let (otx, rx) = oneshot::channel();
let _ = self.tx.send(Event::Approval { gap_id, gap_name, reason, tx: otx }).await;
let approved = rx.await.unwrap_or(false);

// Question
let (otx, rx) = oneshot::channel();
let _ = self.tx.send(Event::Question { gap_id, gap_name, question, tx: otx }).await;
let answer = rx.await.unwrap_or_else(|_| "(no answer received)".into());
```

No Blackboard involvement. No DashMap lookup.

### CLI

`Cli` holds `rx: mpsc::Receiver<Event>` as a sibling field alongside `moss: Moss` — not nested inside `Moss` — so that `tokio::select!` can borrow them independently:

```rust
tokio::select! {
    result = &mut fut         => { /* run completed */ }
    event  = self.rx.recv()  => match event {
        Some(Event::Snapshot(..))       => { /* debug log */ }
        Some(Event::SolverProgress{..}) => { /* debug log */ }
        Some(Event::Approval { tx, .. }) => {
            // prompt user, then:
            let _ = tx.send(approved);
        }
        Some(Event::Question { tx, .. }) => {
            // prompt user, then:
            let _ = tx.send(answer);
        }
        None => break,
    }
}
```

`moss.approve()` and `moss.answer()` no longer exist.

### Removals

- `Blackboard::pending_approvals` and `pending_questions` DashMaps
- `Blackboard::register_approval()`, `register_question()`, `approve()`, `answer_question()`
- `Orchestrator::approve()`, `Orchestrator::answer()`
- `Moss::approve()`, `Moss::answer()`, `Moss::subscribe()`
- `Event::ApprovalRequested`, `Event::QuestionAsked`
- The `broadcast` dependency on `Blackboard`, `Orchestrator`, and `Solver`

## Alternatives considered

**Dedicated `mpsc::Sender<HitlRequest>` alongside the broadcast bus.** This was the first design: keep `broadcast` for observability events, add a second channel only for HITL. Rejected because it keeps two channels for a single consumer, retains the `T: Clone` constraint on `broadcast`, and adds a second receiver the CLI must manage.

**Wrap `oneshot::Sender` in `Arc<Mutex<Option<_>>>` to satisfy `T: Clone`.** Satisfies the broadcast constraint. Rejected: convoluted, "first taker wins" semantics are implicit, and the wrapping exists solely to placate a type bound that should be removed.

**Keep DashMap rendezvous.** Works today. Rejected because it conflates workspace state (Blackboard) with per-run interaction state, and forces an awkward call chain from CLI → Moss → Orchestrator → Blackboard → DashMap for every HITL response.

## Consequences

- One channel, one consumer, no shared mutable state for HITL.
- `Event` cannot be broadcast to multiple subscribers. Acceptable: Moss is designed for single-user, single-session operation.
- The Solver's single `tx` field drives all communication — progress and interaction — which simplifies construction and reduces argument count.
- `Moss::new()` returns a tuple. Call sites must destructure it; they cannot treat `Moss` as a self-contained object without also holding the receiver.

**Positive:**

- Blackboard is purely durable workspace state. All interaction state is gone from it.
- `Moss` public API loses `approve` and `answer` — the surface is smaller and correct.
- The `oneshot::Sender` travels with the request. No gap_id lookup, no DashMap parking, no reverse routing chain.
- `mpsc` enforces single-consumer semantics structurally — only one CLI can respond to a HITL request, which is the correct invariant.
- `Event::ApprovalRequested` and `Event::QuestionAsked` are removed from the broadcast bus; the bus becomes purely observational.

**Negative:**

- Two channels to subscribe to instead of one (`broadcast` for observations, `mpsc` for HITL). Consumers must wire both.
- `mpsc::Receiver` is not `Clone`, so a second consumer (e.g., a future web UI) cannot independently receive HITL requests — one consumer owns the interaction channel. This is a feature (single authority for human responses), not a bug, but it constrains multi-frontend scenarios.

## Implementation scope

`src/moss/signal.rs` (or new `hitl.rs`), `src/moss/blackboard.rs`, `src/moss/solver.rs`, `src/moss/orchestrator.rs`, `src/lib.rs`, `src/cli.rs`.
