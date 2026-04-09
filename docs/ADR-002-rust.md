# ADR-002 — Rust as Implementation Language

**Status:** Accepted

---

## Context

The system is a local daemon with hard latency requirements (sub-second response to scheduling decisions) and concurrent execution of LLM calls, subprocesses, and tool invocations.

## Decision

Rust with Tokio async runtime.

## Rationale

Zero-cost async, no GC pauses, strong type system for modeling state machines (Gap lifecycle), and excellent subprocess management. The `DashMap` + `tokio::JoinSet` combination gives us concurrent DAG execution without manual thread management.

## Trade-offs

Slower iteration speed than Python. Smaller ecosystem for LLM tooling (though `async-openai` and the MCP Rust SDK exist). Higher learning curve for contributors.
