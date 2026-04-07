# Phase 7 — Gate

**Status:** Next
**Effort:** ~100 lines. One focused session.

---

## Spec

Security scanner inserted between compile and execute in the Runner's per-gap task.

```rust
pub(crate) struct Gate {
    blocklist: Vec<Box<str>>,
    max_script_size: usize,
}

pub(crate) enum ScanVerdict {
    Approved,
    Gated { reason: Box<str> },
    Rejected { reason: Box<str> },
}

impl Gate {
    pub(crate) fn scan(&self, artifact: &Artifact, constraints: Option<&Value>) -> ScanVerdict;
}
```

**Four-stage pipeline (first non-Approved verdict wins):**

| Stage | Check | Verdict |
|-------|-------|---------|
| 1. Static analysis | Forbidden imports, network calls in Proactive scripts, writes outside sandbox | Rejected |
| 2. Capability check | Artifact tools vs. Gap constraints | Rejected |
| 3. Resource bounds | Script size within `max_script_size` | Rejected |
| 4. HITL gate | High-risk action patterns (email, delete, purchase) against blocklist | Gated |

`Gate` is stateless policy — no I/O, no async. Pure function of artifact + constraints.

**Wiring into Runner (per-gap task):**

```rust
let artifact = compiler.compile(&gap, &prior).await?;

match gate.scan(&artifact, gap.constraints()) {
    ScanVerdict::Approved => {}
    ScanVerdict::Gated { reason } => {
        bb.set_gap_state(&gap.gap_id(), GapState::Gated)?;
        // emit GateRequested through signal channel — Cli prompts user
        // this is the moment signal::Payload evolves from Box<str> to Event enum
        let approved = /* await human decision via oneshot */ false;
        if !approved {
            bb.append_evidence(Evidence::rejection(&gap, reason));
            bb.set_gap_state(&gap.gap_id(), GapState::Closed)?;
            return Ok(());
        }
        bb.set_gap_state(&gap.gap_id(), GapState::Assigned)?;
    }
    ScanVerdict::Rejected { reason } => {
        bb.append_evidence(Evidence::rejection(&gap, reason));
        bb.set_gap_state(&gap.gap_id(), GapState::Closed)?;
        return Ok(());
    }
}

Executor::new().run(&gap, &artifact, &bb).await?;
```

**Signal evolution:** `Gated` is the first semantic event that `Cli` must render to the user. This phase is the natural point to evolve `signal::Payload` from `type Payload = Box<str>` to a typed `Event` enum (`Snapshot`, `GateRequested { gap_id, reason }`). `Cli` then pattern-matches on `Event` instead of parsing raw strings.

**Files:** `src/moss/gate.rs` (new), `src/moss/runner.rs`, `src/moss/mod.rs`

**Tests:** Unit test each scan stage independently. Integration test: mock provider producing one gatable + one clean artifact, verify gate fires while clean gap runs concurrently.
