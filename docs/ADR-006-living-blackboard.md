# ADR-006 — Living Blackboard with Mutable Intent and Growable DAG

**Status:** Accepted — supersedes the "round-scoped immutable Blackboard" design from v0.4

---

## Context

The original design created a fresh Blackboard for every user message and sealed it immediately after synthesis. Follow-ups required reconstructing context from M1 summaries or M3 Crystals — a lossy process that threw away the rich Evidence the system just produced. The sealed-per-round model also introduced an artificial lifecycle boundary that didn't match how users actually interact: a follow-up like "make it business class" after "book a flight" is clearly the same conversation thread, not a new one.

## Decision

A Blackboard is a living workspace. It stays open across follow-up messages. On each user input, the Orchestrator receives the full Blackboard state (intent + Gaps + Evidence summaries), returns an updated intent and new Gaps. New Gaps are appended — the Gap array only grows. The intent is mutable and evolves to reflect the user's expanding scope. The Blackboard is sealed only when the Orchestrator's decompose output signals a new, unrelated topic, or when the session ends.

## Rationale

- Follow-ups get full-fidelity access to prior Evidence — no information loss from summarization or crystallization.
- The Runner and DAG scheduler require zero changes: `drain_ready()` skips Closed Gaps, `promote_unblocked()` handles dependencies on already-Closed Gaps naturally, `insert_gap()` works on a board with existing Closed Gaps.
- New-topic detection is absorbed into the decompose call — no separate classifier, no extra LLM call, no explicit mode flag. `Decomposition::is_follow_up` carries the decision.
- The Orchestrator already receives the Blackboard state for planning. Asking it to also refine the intent and decide topic continuity adds zero cost.

## Trade-offs

- A long-running Blackboard accumulates many Gaps and Evidence records. The `snapshot()` serialization sent to the Orchestrator could grow large. Mitigation: summarize Evidence in the planner view rather than including raw content; cap the number of Gap entries shown to the LLM.
- Crystallization timing changes: Crystals are now produced less frequently (on topic change rather than every message). Each Crystal covers more ground, which may be better or worse for M3 retrieval precision. This is an open question to evaluate once M3 is implemented.
- The "new-topic" inference heuristic (no new Gaps reference existing ones + intent diverges) may have edge cases. If it proves unreliable, a fallback is an explicit user command (`/new`) to force a board seal.
