# ADR-005 — Human-in-the-Loop via `GapState::Gated`

**Status:** Accepted — updated by ADR-007 (HITL gating as I/O)

---

## Context

Some Gap actions require human involvement before they can proceed. This includes security-sensitive actions (deleting files, sending email, making purchases) flagged by DefenseClaw, but also any situation where the system needs human input, judgment, or physical action (entering a 2FA code, choosing between options, confirming a preference).

## Decision

When any component determines a Gap needs human action, the Gap transitions to `Gated` state and a Gate is inserted into the Blackboard (which emits `Signal::GateRequested` via the `SignalBus` — see ADR-008). The gap task **stays alive on the JoinSet**, awaiting the human response on a per-gate `oneshot` channel — this is just I/O, no different from awaiting a web request. The Runner has no gate-specific logic; it processes one JoinSet completion per iteration and loops. Other gaps keep executing concurrently while the human acts. The CLI subscribes to the `SignalBus` and surfaces Gate prompts in real-time. The user runs `approve <name>` or `reject <name>`, which sends on the gate's `oneshot`. On approval, the gap task resumes execution. On rejection, the Gap posts Failure evidence and transitions to Closed.

## Rationale

`Gated` is a first-class state for **observability** (HUD, planner view, CLI display). The Runner doesn't check for it — it just sees async tasks on the JoinSet. Human latency doesn't block unrelated gaps because the JoinSet processes completions incrementally (one at a time), not in batch. The terminal condition is simply `all_closed()` — `all_gated_or_closed()` is removed.

## Trade-offs

A Gated Gap blocks all downstream Gaps that depend on it, since they cannot promote from Blocked until their dependency is Closed. This is correct — downstream tasks that depend on a human-gated action cannot proceed until that action is confirmed. Independent branches are unaffected.
