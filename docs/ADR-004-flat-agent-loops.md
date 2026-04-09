# ADR-004 — Micro-Agent = ReAct Loop, Not Recursive Orchestrator

**Status:** Superseded by ADR-009

---

The MicroAgent concept is removed — the Solver's unified loop handles both simple (one-shot) and complex (iterative) Gaps without a separate agent runtime. The core insight (don't nest Orchestrators) remains valid and is preserved in ADR-009's design.

## Original Decision (historical context)

A Reactive Gap is executed by a `MicroAgent` running a ReAct loop. It does not instantiate a new Orchestrator, does not have a sub-Blackboard. The only output is a single `Evidence` record posted to the parent Blackboard.
