# Moss Implementation Phases

Each phase has its own spec file. Phases are independently testable and ship in order.

## Status

| Phase | Name | Status | Spec |
|-------|------|--------|------|
| 0 | Clean the Foundation | Done | [completed/phase-0.md](completed/phase-0.md) |
| 1 | Rebuild Blackboard | Done | [completed/phase-1.md](completed/phase-1.md) |
| 2 | Orchestrator + Moss Facade | Done | [completed/phase-2.md](completed/phase-2.md) |
| 3 | Compiler | Done | [completed/phase-3.md](completed/phase-3.md) |
| 4 | Executor | Done | [completed/phase-4.md](completed/phase-4.md) |
| 5 | Runner + Observability | Done | [completed/phase-5.md](completed/phase-5.md) |
| — | Blackboard Lifecycle Fix | Done | [completed/lifecycle-fix.md](completed/lifecycle-fix.md) |
| 6 | Signal Bus + Runner Rewrite | Done | [completed/phase-6.md](completed/phase-6.md) |
| 7 | DefenseClaw | Done | [completed/phase-7-defense-claw.md](completed/phase-7-defense-claw.md) |
| 8 | Agent Loop (Reactive Gaps) | Ready | [phase-8-agent-loop.md](phase-8-agent-loop.md) |
| 8a | Provider Types + Tool-Calling | Ready | ↑ same file |
| 8b | McpBridge (stdio transport) | Ready | ↑ same file |
| 8c | MicroAgent + Executor wiring | Ready | ↑ same file |
| 9 | Memory M1 | Planned | [phase-9-memory-m1.md](phase-9-memory-m1.md) |
| 10 | Memory M2 + M3 | Planned | [phase-10-memory-m2-m3.md](phase-10-memory-m2-m3.md) |
| 11 | HUD | Planned | [phase-11-hud.md](phase-11-hud.md) |

## Dependency Graph

```
0 → 1 → 2 → 3 → 4 → 5 → Lifecycle Fix
                                |
                    +-----------+-----------+
                    |                       |
                    v                       v
              6 (Signal Bus)          8 (Agent Loop)
                    |                  can run in parallel
                    v
              7 (DefenseClaw)
                    |
                    v
              9 → 10 → 11
```

Phase 8 (Agent Loop) has no dependency on Phase 6 (Signal Bus). They can be built in parallel.

## ADRs

Key design decisions that shaped the phases:

| ADR | Title | Key Decision |
|-----|-------|-------------|
| [ADR-007](../docs/ADR-007-defenseclaw-and-hitl-gating.md) | DefenseClaw & HITL Gating | Gated = I/O. Gap stays on JoinSet. Runner has no gate logic. |
| [ADR-008](../docs/ADR-008-broadcast-foundation.md) | Broadcast Foundation | Generic `SignalBus` — one channel, any producer, any consumer. |

## Files Per Phase

| Phase | Files |
|-------|-------|
| 6a | `src/moss/signal.rs` (new), `src/moss/mod.rs`, `src/moss/blackboard.rs`, `src/moss/orchestrator.rs`, `src/lib.rs` |
| 6b | `src/moss/runner.rs` |
| 6c | `src/cli.rs` (new), `src/main.rs` (simplified to bootstrap), `src/lib.rs` (add `approve_gate`/`reject_gate`) |
| 7 | `src/moss/defense_claw.rs` (new), `src/moss/runner.rs`, `src/moss/mod.rs` |
| 8a | `src/providers/mod.rs`, `src/providers/remote/openrouter.rs` |
| 8b | `src/providers/mcp.rs` (new) |
| 8c | `src/moss/micro_agent.rs` (new), `src/moss/executor.rs`, `src/moss/orchestrator.rs` |
| 9 | `src/memory/` (new), `src/ma