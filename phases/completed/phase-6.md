# Phase 6 — Signal Bus + Runner Rewrite

**Status:** Next
**Blocked by:** Phase 5 (done)
**Blocks:** Phase 7 (DefenseClaw)
**ADRs:** [ADR-007](../docs/ADR-007-defenseclaw-and-hitl-gating.md), [ADR-008](../docs/ADR-008-broadcast-foundation.md)
**Effort:** ~150 lines across 3 sub-phases. One focused session.

---

## 6a — Signal Bus

Create `src/moss/signal.rs` — the generic broadcast foundation for Moss.

**New types:**

```rust
// Signal — flat, extensible, Clone + Debug
pub(crate) enum Signal {
    GapStateChanged { gap_id: Uuid, gap_name: Box<str>, old_state: Box<str>, new_state: Box<str> },
    GapInserted { gap_id: Uuid, gap_name: Box<str> },
    GateRequested { gap_id: Uuid, gap_name: Box<str>, reason: Box<str> },
    GateResolved { gap_id: Uuid, approved: bool },
    EvidencePosted { gap_id: Uuid, gap_name: Box<str>, status: Box<str> },
    IntentUpdated { intent: Box<str> },
    BoardSealed,
    System { level: SignalLevel, message: Box<str> },
}

pub(crate) enum SignalLevel { Info, Warn, Error }

// SignalBus — created once, cloned everywhere
pub(crate) struct SignalBus {
    tx: broadcast::Sender<Signal>,
}

impl SignalBus {
    pub fn new(capacity: usize) -> Self;
    pub fn emit(&self, signal: Signal);          // non-blocking, silent drop
    pub fn subscribe(&self) -> Receiver<Signal>; // independent per subscriber
}
```

**Threading:**

- `Moss::new()` creates `SignalBus::new(64)`.
- `Moss::subscribe()` exposes it to CLI.
- `Moss` → `Orchestrator` → `Blackboard` all hold a `SignalBus` clone.

**Blackboard changes:**

- `Blackboard::new(bus: SignalBus)` — stores bus as field.
- `set_gap_state()` emits `Signal::GapStateChanged`.
- `insert_gap()` emits `Signal::GapInserted`.
- `append_evidence()` emits `Signal::EvidencePosted`.
- `set_intent()` emits `Signal::IntentUpdated`.
- `insert_gate()` emits `Signal::GateRequested`, stores `oneshot::Sender<bool>`, returns `oneshot::Receiver<bool>`.

**Files:** `src/moss/signal.rs` (new), `src/moss/mod.rs`, `src/moss/blackboard.rs`, `src/moss/orchestrator.rs`, `src/lib.rs`

**Tests:** Emit from Blackboard methods → subscribe → assert. No-subscriber silent drop. Lagged receiver.

---

## 6b — Runner Rewrite

Rewrite `runner.rs` — persistent JoinSet, one-completion-per-iteration.

**Current loop (batch drain):**
```rust
loop {
    promote → drain_ready → spawn ALL into JoinSet
    while let Some(result) = tasks.join_next().await { ... }  // drain ALL
}
```

**New loop (incremental):**
```rust
let mut tasks: JoinSet<...> = JoinSet::new();
loop {
    promote → drain_ready → spawn new into JoinSet
    if tasks.is_empty() {
        if all_closed() { return Ok(()) }
        else { return Err(Deadlock) }
    }
    // Wait for ONE task, not all
    if let Some(result) = tasks.join_next().await { result??; }
}
```

**Removals:**
- Delete `all_gated_or_closed()` from `blackboard.rs` and its tests.
- Remove the `all_gated_or_closed` branch from Runner's terminal check.

**Files:** `src/moss/runner.rs`, `src/moss/blackboard.rs`

---

## 6c — CLI Module

Extract the interface layer (L5) into its own module: `src/cli.rs`. The CLI owns all user-facing I/O — input parsing, output rendering, signal handling. `main.rs` becomes a thin bootstrap.

**Why a module, not just main.rs:**
- The CLI is about to get a `tokio::select!` loop over stdin + SignalBus — that's real logic, not bootstrap.
- Gate approval commands (`approve <name>`, `reject <name>`) need parsing and dispatch.
- Future: HUD rendering, colored output, progress bars — all L5 concerns that shouldn't live in `main()`.
- Testability: a struct with methods can be unit tested. A `main()` function can't.

**Structure:**

```rust
// src/cli.rs

use tokio::sync::broadcast;
use crate::moss::signal::{Signal, SignalLevel};

pub struct Cli {
    moss: Moss,
    rx: broadcast::Receiver<Signal>,
}

impl Cli {
    pub fn new(moss: Moss) -> Self {
        let rx = moss.subscribe();
        Self { moss, rx }
    }

    /// Main event loop. Runs until stdin closes or user types "exit".
    pub async fn run(&mut self) -> Result<(), MossError> {
        let stdin = tokio::io::stdin();
        let mut lines = tokio::io::BufReader::new(stdin).lines();

        loop {
            tokio::select! {
                line = lines.next_line() => {
                    match line? {
                        Some(raw) => self.handle_input(raw.trim_end()).await?,
                        None => break,
                    }
                }
                signal = self.rx.recv() => {
                    if let Ok(signal) = signal {
                        self.handle_signal(signal);
                    }
                }
            }
        }
        Ok(())
    }

    /// Parse and dispatch user input.
    async fn handle_input(&mut self, input: &str) -> Result<(), MossError> {
        match input {
            "" => {}
            "exit" | "quit" => std::process::exit(0),
            s if s.starts_with("approve ") => {
                let name = &s["approve ".len()..];
                self.moss.approve_gate(name)?;
            }
            s if s.starts_with("reject ") => {
                let name = &s["reject ".len()..];
                self.moss.reject_gate(name)?;
            }
            query => {
                match self.moss.run(query).await {
                    Ok(response) => self.print_response(&response),
                    Err(e) => self.print_error(&e),
                }
            }
        }
        Ok(())
    }

    /// Render a signal to the terminal.
    fn handle_signal(&self, signal: Signal) {
        match signal {
            Signal::GateRequested { gap_name, reason, .. } => {
                // TODO: colored output
                println!("[moss] '{}' needs your action: {}", gap_name, reason);
                println!("       approve {} / reject {}", gap_name, gap_name);
            }
            Signal::GapStateChanged { gap_name, new_state, .. } => {
                // Quiet for now — HUD will render these later
                tracing::debug!(gap = %gap_name, state = %new_state, "state changed");
            }
            Signal::System { level: SignalLevel::Error, message, .. } => {
                eprintln!("[moss] error: {}", message);
            }
            _ => {} // other signals: ignore until HUD
        }
    }

    fn print_response(&self, response: &str) {
        println!("{response}");
    }

    fn print_error(&self, error: &MossError) {
        eprintln!("[moss] error: {error}");
    }
}
```

**main.rs becomes thin bootstrap:**

```rust
// src/main.rs
use moss::Moss;
use moss::cli::Cli;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().pretty()
        .with_env_filter(/* ... */)
        .init();

    let provider = /* ... */;
    let moss = Moss::new(provider);
    let mut cli = Cli::new(moss);

    if let Err(e) = cli.run().await {
        tracing::error!(error = %e, "fatal");
        std::process::exit(1);
    }
}
```

**Gate approval wiring:**

`Cli` calls `moss.approve_gate(name)` / `moss.reject_gate(name)`. These are new methods on `Moss` that look up the gate's `oneshot::Sender` by gap name and send `true`/`false`. This means `Moss` needs a way to access the Blackboard's gate map — either directly or through a thin API.

**Files:** `src/cli.rs` (new), `src/main.rs` (simplified), `src/lib.rs` (add `pub mod cli`, expose `approve_gate`/`reject_gate` on `Moss`)
