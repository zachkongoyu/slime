# ADR-001 — Blackboard Pattern over Message-Passing Agents

**Status:** Accepted

---

## Context

The system needs to coordinate multiple specialist tasks (web search, file operations, code generation) that operate on shared context. Two common patterns: (a) Blackboard — shared memory with a central coordinator reading/writing, (b) Actor/message-passing — each agent has private state and communicates via async channels.

## Decision

Blackboard pattern, implemented with `DashMap` for concurrent access.

## Rationale

The Orchestrator needs a global view of all Gaps and Evidence to make scheduling decisions and detect deadlocks. With message-passing, this global view requires either a centralized broker (which is functionally a Blackboard) or expensive all-to-all communication. The Blackboard makes the shared state explicit and inspectable, which simplifies debugging and enables the HUD to stream deltas directly from the data structure.

## Trade-offs

Blackboard contention under very high parallelism (mitigated by DashMap's per-shard locking). Less isolation between components than pure message-passing. The DashMap approach means we cannot trivially distribute across processes — this is acceptable for a single-machine AIOS.
