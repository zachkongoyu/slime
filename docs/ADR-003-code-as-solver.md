# ADR-003 — LLM-Generated Code as the Execution Primitive

**Status:** Accepted

---

## Context

Gaps need to be resolved by "doing something" — calling APIs, transforming data, navigating websites. Options: (a) a fixed toolkit of Rust-native functions the LLM selects from, (b) LLM generates executable code (scripts) on the fly.

## Decision

LLM generates code. The Solver writes and executes code on every iteration.

## Rationale

A fixed toolkit scales linearly with development effort and suffers from selection errors as it grows (the LLM must choose from an ever-larger menu). Code generation scales with the LLM's capability: as models improve, the range of solvable Gaps expands without code changes to Moss. Scripts are also inspectable and auditable (logged to M2).

## Trade-offs

Security risk from executing LLM-generated code (mitigated by DefenseClaw + sandboxing). Latency overhead of an extra LLM call per Gap (mitigated by parallelism). Debugging is harder when the execution logic is generated at runtime.
