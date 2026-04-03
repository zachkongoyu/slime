## Plan: Moss Implementation

**Current codebase state:** `blackboard.rs` has the old `Gap` (no name/deps, `Pulse` enum) and `Evidence` (single per gap, `done: bool`). `orchestrator.rs` has one method that calls the LLM, writes `output.json`, and returns nothing useful. `Cargo.toml` has `async-openai` and `sqlx` that are never used. The target is ARCHITECTURE.md v0.4.0.

---

### Phase 0 — Clean the Foundation
*Unblocks everything. Mechanical changes, no logic.*

1. **`Cargo.toml`**: remove `async-openai`, `sqlx`. Add `thiserror = "1"`, `minijinja = "2"`. Keep `tokio`, `dashmap`, `serde`, `serde_json`, `reqwest`, `async-trait`, `uuid`.
2. **`src/lib.rs`**: add `pub mod error;`
3. **Create `src/error.rs`**: define `MossError` with variants from §11 — `Provider`, `Compiler`, `Executor`, `DefenseRejection`, `Blackboard`, `Deadlock`, `Mcp`, `Io`, `Json`. Add `ProviderError` as a sub-type. Use `thiserror`.
4. **`providers/mod.rs`**: change `complete_chat` signature to `async fn complete_chat(&self, messages: Vec<Message>) -> Result<String, ProviderError>`. Add `complete_with_tools` stub (returns `Err(ProviderError::NotSupported)` for now).
5. **`providers/remote/openrouter.rs`**: replace all `.expect()` / `panic!()` with `?` and `Result`. Remove panics from `build_headers`, `complete_chat`.
6. **`providers/local/mod.rs`**: update mock to return `Ok(echo)`.
7. **`main.rs`**: update call sites to handle `Result`, print errors to stderr. Keep the loop alive — don't crash.

*Verify: `cargo build` passes. No panics in the call path.*

---

### Phase 1 — Rebuild Blackboard
*Replaces the current blackboard.rs entirely. Self-contained, unit-testable.*

8. **`blackboard.rs`** — replace file:
   - Remove `Pulse` enum entirely.
   - New `GapState`: `Blocked | Ready | Assigned | Gated | Closed`
   - New `GapType`: `Proactive | Reactive`
   - New `Gap` struct: `gap_id`, `name`, `state`, `description`, `gap_type`, `dependencies: Vec<Box<str>>`, `constraints: Option<Value>`, `expected_output: Option<Box<str>>`
   - New `EvidenceStatus`: `Success | Failure { reason } | Partial`
   - New `Evidence`: `gap_id`, `attempt: u32`, `content: Value`, `status: EvidenceStatus`
   - New `Blackboard` fields: `intent`, `gaps: DashMap<Uuid, Gap>`, `name_index: DashMap<Box<str>, Uuid>`, `evidences: DashMap<Uuid, Vec<Evidence>>`, `gates: DashMap<Uuid, Value>`
   - Methods: `insert_gap` (writes both `gaps` and `name_index`), `set_gap_state`, `append_evidence`, `get_gap`, `get_gap_id_by_name`, `get_evidence`, `drain_ready`, `promote_unblocked`, `all_closed`, `all_gated_or_closed`, `insert_gate`, `set_intent`
   - All visibility: `pub` (needed by `Runner` in a sibling module)

*Verify: write unit tests in `blackboard.rs` for `promote_unblocked`, `drain_ready`, `all_gated_or_closed`. Test a 3-gap linear chain and a 2-gap parallel fan-out.*

---

### Phase 2 — Orchestrator: Decompose Only
*Replaces `synthesize` with a proper `decompose` method. Output goes into the Blackboard.*

9. **Create `src/moss/plan.rs`**: define `Plan { intent: String, gaps: Vec<GapSpec> }` and `GapSpec { name, description, gap_type, dependencies, constraints, expected_output }` — the deserialized form of the LLM JSON output.
10. **`orchestrator.rs`** — rewrite:
    - Struct: `Orchestrator { provider: Arc<dyn Provider> }` (no `compiler`, no `executor`)
    - Remove `synthesize` (old version). Remove `output.json` write.
    - Add `pub async fn decompose(&self, query: &str, blackboard: &Blackboard) -> Result<Plan>`: use `minijinja` to render `orchestrator.xml` (replace `{user_query}`, `{blackboard_state}` with proper template vars), call provider, strip markdown fences, `serde_json::from_str` into `Plan`.
    - Add `pub async fn synthesize(&self, blackboard: &Blackboard) -> Result<String>`: collect latest `Evidence` per gap, render into a synthesis prompt, call provider, return the text string.
    - Switch prompt loading to `include_str!("prompts/orchestrator.xml")`.
11. **`mod.rs`**: add `pub mod plan;`
12. **`main.rs`**: call `orchestrator.decompose(input, &blackboard)`, insert gaps into blackboard, print gap names to confirm DAG.

*Verify: run `cargo run`, type a query, see gap names printed. No `output.json`. No panics.*

---

### Phase 3 — Compiler
*New file. Testable with `LocalMock` provider.*

13. **Create `src/moss/compiler.rs`**:
    - `Artifact` enum: `Script { code: String, timeout: Duration }` | `AgentSpec { goal, tools: Vec<String>, instructions, max_iterations: u32, timeout: Duration }`
    - `Compiler { provider: Arc<dyn Provider> }`
    - `pub async fn compile(&self, gap: &Gap, prior_attempts: &[Evidence]) -> Result<Artifact>`: render `compiler.xml` via `minijinja` with gap + prior attempt errors as context, call provider, parse JSON into `Artifact`.
    - Switch prompt loading to `include_str!("prompts/compiler.xml")`.
14. **`mod.rs`**: add `pub mod compiler;`

*Verify: unit test with `LocalMock` — pass a `GapType::Proactive` gap, assert `Artifact::Script` is returned. Pass a `GapType::Reactive` gap, assert `Artifact::AgentSpec`.*

---

### Phase 4 — Executor (Script Path)
*New file. No MCP needed yet — only Proactive script execution.*

15. **Create `src/moss/executor.rs`**:
    - `Executor { sandbox_dir: PathBuf }` (no MCP yet)
    - `pub async fn run(&self, gap_id: Uuid, artifact: Artifact) -> Result<Evidence>`: for `Artifact::Script`, write code to a temp file in `sandbox_dir`, spawn `tokio::process::Command::new("python3")` with `-c` arg and `timeout` capped via `tokio::time::timeout`, capture stdout, parse as JSON into `Evidence { gap_id, attempt: 1, content, status: EvidenceStatus::Success }`. On non-zero exit or timeout: return `Evidence` with `EvidenceStatus::Failure { reason: stderr }`.
    - For `Artifact::AgentSpec`: return `Err(MossError::Executor { reason: "MCP not yet available".into() })` — stub.
16. **`mod.rs`**: add `pub mod executor;`

*Verify: write a test that compiles a trivial script (`print(json.dumps({"result": 42}))`), runs it, asserts Evidence content = `{"result": 42}`.*

---

### Phase 5 — Runner + Full Execution Loop
*Wires everything together. First true end-to-end path.*

17. **Create `src/moss/runner.rs`**:
    - `Runner { compiler: Arc<Compiler>, executor: Arc<Executor> }` (no `DefenseClaw` yet — add in Phase 6)
    - `pub async fn execute(&self, plan: Plan, blackboard: Arc<Blackboard>) -> Result<()>`: implement the loop from §3.2 pseudocode exactly — insert gaps, `promote_unblocked`, JoinSet + Semaphore fan-out, `drain_ready`, `append_evidence`, `set_gap_state`, `promote_unblocked` on each completion, three-branch `None` termination.
    - Retry logic: on `EvidenceStatus::Failure`, if `attempt < MAX_RETRIES (2)`, re-queue the gap as `Ready` and increment attempt counter. After max retries, mark `Closed` and propagate failure to dependents via a `propagate_failure(&self, gap_id)` method on Blackboard.
18. **`mod.rs`**: add `pub mod runner;`
19. **`main.rs`**: full wiring — `orchestrator.decompose` → `runner.execute` → `orchestrator.synthesize` → print response. Blackboard lives for the session duration (not recreated per query).

*Verify: end-to-end test. Query: "What is 2 + 2?" — expect a single Proactive gap, script runs `print(json.dumps({"result": 4}))`, synthesis returns a sentence containing "4".*

---

### Phase 6 — DefenseClaw
*Inserts into the Runner between compile and execute. No new dependencies.*

20. **Create `src/moss/defense_claw.rs`**:
    - `ScanVerdict` enum: `Approved | Gated { reason } | Rejected { reason }`
    - `DefenseClaw { blocklist: Vec<String>, max_script_size: usize }`
    - `pub fn scan(&self, artifact: &Artifact, constraints: &Option<Value>) -> ScanVerdict`: regex/string match against `blocklist` patterns in `Script.code` → `Rejected` if hit; match against HITL action patterns (configurable list: `"send_email"`, `"delete_file"`, `"make_purchase"`) → `Gated`; otherwise `Approved`. `AgentSpec` checks tool list against constraints.
21. Add `defense_claw: Arc<DefenseClaw>` to `Runner`. Wire `scan()` call into the JoinSet task between `compile` and `execute.run`, matching the §3.2 pseudocode `match` block.
22. Add `Gated` branch handling to `runner.rs` — `insert_gate`, `all_gated_or_closed`, `cli::print_pending_gates`.
23. Add `approve <name>` / `reject <name>` commands to `main.rs` CLI loop.

*Verify: craft a gap whose compiled script contains `os.system`. Assert `ScanVerdict::Rejected`. Craft a gap whose script contains `delete_file`. Assert `ScanVerdict::Gated`. CLI shows the gate, `approve` resumes execution.*

---

### Phase 7 — MCP Bridge + Micro-Agent (Reactive Gaps)
*Unblocks the Reactive execution path.*

24. **Create `src/providers/mcp.rs`**: `McpBridge { servers, tool_registry }` with `discover()`, `call()`, `tool_definitions()`. Start with stdio transport to a single hardcoded MCP server (filesystem).
25. **Create `src/moss/micro_agent.rs`**: `MicroAgent` struct + `run()` as specified in §4.4. Uses `provider.complete_with_tools()` — implement this method on `OpenRouter` first.
26. Wire `Executor::run` `AgentSpec` branch to instantiate and run `MicroAgent`.
27. Add `mcp: Arc<McpBridge>` to `Executor`.

*Verify: test scenario 1 — "Move Downloads/report.pdf to ~/Documents". Expect one Reactive gap, MicroAgent uses filesystem MCP tools, Evidence shows success.*

---

### Phase 8 — Memory M1
*Enables multi-turn conversation within a session.*

28. **Create `src/memory/mod.rs`** and **`src/memory/session_buffer.rs`**: `SessionBuffer { entries: VecDeque<SessionEntry>, capacity: usize, last_interaction: Instant }`. `m1_recent()` returns last N entries as `Vec<Message>`. Idle timeout check on each call.
29. Wire into `main.rs` — update buffer after each `handle_query` call with user message + response summary.
30. Pass `session_ctx` into `orchestrator.decompose`.

---

### Phase 9 — Memory M2 + M3
*Deferred — no blockers on core flow.*

31. **M2**: add `sled` to `Cargo.toml`. Audit log appends every compiled artifact.
32. **M3**: add `qdrant-client`. Implement `crystallize()` and `m3_search()` on `MemoryManager`.

---

### Phase 10 — HUD
*Polish, last.*

33. Add `tokio::broadcast` channel to `Blackboard`. Emit `BlackboardDelta` on every state transition. `main.rs` spawns a HUD task that prints delta events inline.

---

**Relevant files touched per phase**

| Phase | Files |
|---|---|
| 0 | `Cargo.toml`, `src/lib.rs`, `src/error.rs`, `src/providers/mod.rs`, `src/providers/remote/openrouter.rs`, `src/providers/local/mod.rs`, `src/main.rs` |
| 1 | `src/moss/blackboard.rs` |
| 2 | `src/moss/plan.rs` (new), `src/moss/orchestrator.rs`, `src/moss/mod.rs`, `src/main.rs` |
| 3 | `src/moss/compiler.rs` (new), `src/moss/mod.rs` |
| 4 | `src/moss/executor.rs` (new), `src/moss/mod.rs` |
| 5 | `src/moss/runner.rs` (new), `src/moss/mod.rs`, `src/main.rs` |
| 6 | `src/moss/defense_claw.rs` (new), `src/moss/runner.rs`, `src/main.rs` |
| 7 | `src/providers/mcp.rs` (new), `src/moss/micro_agent.rs` (new), `src/moss/executor.rs`, `src/providers/remote/openrouter.rs` |
| 8 | `src/memory/` (new), `src/main.rs` |
| 9 | `Cargo.toml`, `src/memory/` |
| 10 | `src/moss/blackboard.rs`, `src/main.rs` |

**Dependency order:** Phase 0 → 1 → 2 → (3, 4 in parallel) → 5 → 6 → 7 → 8 → 9 → 10

**First milestone worth shipping:** End of Phase 5 — query in, DAG executes in parallel, synthesized answer printed.
