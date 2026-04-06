## Plan: Moss Implementation

---

### Phase 0 — Clean the Foundation ✅
- Removed unused deps (`async-openai`, `sqlx`). Added `thiserror`, `minijinja`.
- Created `src/error.rs` with `MossError` and `ProviderError` via `thiserror`.
- Updated `Provider` trait: `complete_chat` returns `Result<String, ProviderError>`.
- Replaced all `.expect()` / `panic!()` with `?` in `openrouter.rs`.
- `main.rs` handles errors to stderr, keeps loop alive.

---

### Phase 1 — Rebuild Blackboard ✅
- Full rewrite of `blackboard.rs`.
- `GapState`: `Blocked | Ready | Assigned | Gated | Closed`
- `GapType`: `Proactive | Reactive`
- `Gap`: `gap_id`, `name`, `state`, `description`, `gap_type`, `dependencies: Vec<Box<str>>`, `constraints: Option<Value>`, `expected_output: Option<Box<str>>`
- `EvidenceStatus`: `Success | Failure { reason } | Partial`
- `Evidence`: `gap_id`, `attempt: u32`, `content: Value`, `status`
- `Blackboard`: `intent: Mutex<Option<Box<str>>>`, `gaps: DashMap<Uuid, Gap>`, `name_index`, `evidences`, `gates`
- Methods: `insert_gap`, `set_gap_state`, `append_evidence`, `get_gap`, `get_gap_id_by_name`, `get_evidence`, `drain_ready`, `promote_unblocked`, `all_closed`, `all_gated_or_closed`, `insert_gate`, `set_intent`, `all_evidence`, `status_summary`
- All fields `pub(crate)` via getters. Derives `Debug`.
- Unit tests: linear chain, parallel fanout, gated/closed, evidence.

---

### Phase 2 — Orchestrator + Moss Facade ✅
- Created `src/moss/decomposition.rs` (was `plan.rs` — renamed for clarity).
  - `Decomposition { intent: Option<String>, gaps: Option<Vec<GapSpec>> }`
  - `GapSpec { name, description, gap_type, dependencies, constraints, expected_output }` — all `String` fields (short-lived DTO).
- Rewrote `orchestrator.rs`:
  - `decompose`: renders `prompts/decompose.md` via minijinja, calls LLM, strips markdown fences, deserializes into `Decomposition`.
  - `synthesize`: renders `prompts/synthesize.md`, passes real evidence from `blackboard.all_evidence()`.
- Prompt format: Markdown instructions + XML-tagged input variables. More portable across LLM providers than pure XML.
- Created `src/lib.rs` — `Moss` facade (the only `pub` entry point):
  - `Moss::new(provider)` wires Orchestrator + Runner + Blackboard.
  - `Moss::run(query)` → decompose → execute → synthesize → return answer.
- `main.rs` simplified: only uses `Moss`.

---

### Phase 3 — Compiler ✅
- Created `src/moss/compiler.rs`.
- `Artifact` enum with `#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]`:
  - `Script { language: Box<str>, code: Box<str>, timeout_secs: u64 }`
  - `Agent { role, goal, tools: Vec<Box<str>>, instructions }`
- `Compiler { provider: Arc<dyn Provider> }`.
- `compile(&self, gap: &Gap, prior_attempts: &[Box<str>]) -> Result<Artifact>`: renders `prompts/compiler.md`, calls LLM, deserializes.
- Prompt (`compiler.md`): language-agnostic — LLM picks best language for each gap. `prior_attempts` injected so LLM avoids repeating past mistakes.
- Unit tests via `MockCompilerProvider` (no real HTTP). Tests: Script, Agent, markdown fence stripping, prior attempts.

---

### Phase 4 — Executor ✅
- Created `src/moss/executor.rs`.
- `Executor` is a **zero-size unit struct** (`#[derive(Clone, Copy)]`) — stateless, no sandbox dir.
- `run(&self, gap: &Gap, artifact: &Artifact, blackboard: &Blackboard) -> Result<()>`:
  - Script path: writes code to `tempfile::NamedTempFile` (auto-deleted on drop), spawns interpreter via `tokio::process::Command`, wraps in `tokio::time::timeout`.
  - Language → interpreter mapping: `python/python3 → python3`, `shell/sh/bash → sh`, `javascript/js → node`, any other string → passed through directly.
  - stdout parsed as JSON → `EvidenceStatus::Success`. Non-JSON stdout → `EvidenceStatus::Partial`. Non-zero exit → `EvidenceStatus::Failure`. Timeout → `EvidenceStatus::Failure`.
  - Agent path: stub — writes `Failure` evidence, returns `Ok(())`.
- Evidence written directly to Blackboard (Blackboard pattern).
- Unit tests using real `sh` subprocesses.

---

### Phase 5 — Runner + Observability ✅
- Created `src/moss/runner.rs`.
- `Runner { compiler: Arc<Compiler> }` — no `Arc<Executor>` (Executor is zero-cost to construct).
- `run(&self, blackboard: Arc<Blackboard>) -> Result<(), MossError>`:
  - Loop: `promote_unblocked → drain_ready → JoinSet fan-out → join_next`.
  - Each task: check `attempt_count >= MAX_RETRIES (3)` → force close. Else compile → execute → close or set back to Ready for retry.
  - Termination: `drain_ready` empty + `all_gated_or_closed` → `Ok(())`. Empty + not all closed → `Err(Deadlock)`.
- `MAX_RETRIES = 3`. Failed gaps set back to `Ready`; Runner picks them up next round.
- Wired into `Moss::run()` in `lib.rs`: decompose → `runner.run(Arc::clone(&blackboard))` → synthesize.
- Added `tracing` + `tracing-subscriber` (pretty format). Log levels:
  - `RUST_LOG=moss=info` — pipeline flow (intent, gap open/close)
  - `RUST_LOG=moss=debug` — + evidence + full blackboard state each round
  - `RUST_LOG=moss=trace` — + gap detail before compile + blackboard after each execution
- **Known issue:** Blackboard is created once in `Moss::new()` and shared across all `run()` calls. Gaps accumulate across queries. Fix in next item.

---

### Blackboard Lifecycle Fix 🔜
*Small fix, high priority — must land before Phase 6.*

- Move `Arc<Blackboard>` creation into `Moss::run()` (fresh per query).
- Remove `blackboard` field from `Moss` struct.
- Each query gets an isolated Blackboard; prior query state does not leak.

---

### Phase 6 — DefenseClaw 🔜
*Security scanner inserted between compile and execute.*

- `ScanVerdict`: `Approved | Gated { reason } | Rejected { reason }`
- `DefenseClaw { blocklist: Vec<String>, max_script_size: usize }`
- `scan(&self, artifact: &Artifact, constraints: &Option<Value>) -> ScanVerdict`:
  - Script: regex/string match on `code` against `blocklist` → `Rejected`.
  - Script: match against HITL patterns (`send_email`, `delete_file`, `make_purchase`) → `Gated`.
  - Agent: check tool list against constraints.
- Wire into Runner's JoinSet task between `compile` and `executor.run`.
- `Gated` branch: `insert_gate`, `all_gated_or_closed`, CLI prints pending gates.
- Add `approve <name>` / `reject <name>` commands to `main.rs`.

---

### Phase 7 — Agent Loop (Reactive Gaps) 🔜
*Fills in the Executor's Agent stub.*

- `MicroAgent { provider: Arc<dyn Provider>, tools: Vec<Box<str>>, max_iterations: u32 }`
- Runs a ReAct loop: Reason (LLM call) → Act (tool call) → Observe (result) → Reflect.
- Implement `complete_with_tools` on `OpenRouter` (currently returns `NotSupported`).
- Wire Executor's Agent branch to instantiate and run `MicroAgent`.
- MCP bridge (`src/providers/mcp.rs`): `discover()`, `call()`, `tool_definitions()`. Start with stdio transport to filesystem MCP server.

---

### Phase 8 — Memory M1 🔜
*Multi-turn conversation within a session.*

- `SessionBuffer { entries: VecDeque<SessionEntry>, capacity: usize }`.
- `m1_recent()` returns last N entries as `Vec<Message>`.
- Wire into `Moss::run()` — update buffer after each query, inject into `orchestrator.decompose`.

---

### Phase 9 — Memory M2 + M3 🔜
- M2: `sled` embedded KV — user preferences, audit trail of compiled artifacts.
- M3: `qdrant-client` — Knowledge Crystals, compressed outcomes from past sessions. `crystallize()` + `m3_search()`.

---

### Phase 10 — HUD 🔜
- `tokio::broadcast` channel on Blackboard. Emit `BlackboardDelta` on every state transition.
- `main.rs` spawns a HUD task streaming delta events to the terminal in real time.

---

**Dependency order:** 0 → 1 → 2 → 3 → 4 → 5 → Lifecycle Fix → 6 → 7 → 8 → 9 → 10

**Files per phase**

| Phase | Files |
|---|---|
| 0 | `Cargo.toml`, `src/error.rs`, `src/providers/mod.rs`, `src/providers/remote/openrouter.rs`, `src/providers/local/mod.rs`, `src/main.rs` |
| 1 | `src/moss/blackboard.rs` |
| 2 | `src/moss/decomposition.rs` (new), `src/moss/orchestrator.rs`, `src/moss/mod.rs`, `src/lib.rs`, `src/main.rs` |
| 3 | `src/moss/compiler.rs` (new), `src/moss/prompts/compiler.md` (new), `src/moss/mod.rs` |
| 4 | `src/moss/executor.rs` (new), `src/moss/mod.rs`, `Cargo.toml` (+tempfile) |
| 5 | `src/moss/runner.rs` (new), `src/moss/mod.rs`, `src/lib.rs`, `src/main.rs`, `Cargo.toml` (+tracing) |
| Lifecycle | `src/lib.rs` |
| 6 | `src/moss/defense_claw.rs` (new), `src/moss/runner.rs`, `src/main.rs` |
| 7 | `src/providers/mcp.rs` (new), `src/moss/micro_agent.rs` (new), `src/moss/executor.rs`, `src/providers/remote/openrouter.rs` |
| 8 | `src/memory/` (new), `src/main.rs` |
| 9 | `Cargo.toml`, `src/memory/` |
| 10 | `src/moss/blackboard.rs`, `src/main.rs` |
