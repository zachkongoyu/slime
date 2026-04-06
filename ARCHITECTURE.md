# Moss AIOS — Architecture Specification

**Version:** 0.6.0-draft
**Date:** 2026-04-06
**Status:** Living document — each component is marked with its implementation status.

**Status legend:**

| Tag | Meaning |
|-----|---------|
| `IMPLEMENTED` | Code exists, compiles, and is exercised by at least one path |
| `PARTIAL` | Skeleton or stub exists; core logic incomplete |
| `PLANNED` | Designed but no code yet |

---

## 1. Overview

Moss is a local-first AI Operating System that transforms a single user intent into a parallel execution plan, runs it, and synthesizes a result. It is built in Rust on Tokio and uses one or more LLM providers for reasoning.

The system follows the **Blackboard architecture pattern** (Hearsay-II lineage): independent specialist components read from and write to a shared, structured memory space (the Blackboard), coordinated by a central Orchestrator that decomposes intent into a Directed Acyclic Graph (DAG) of atomic tasks called Gaps.

### 1.1 Design Principles

1. **Living Blackboard.** A Blackboard is a workspace, not a transaction. It stays open across follow-up messages: the Orchestrator appends new Gaps and refines the intent as the conversation evolves. The Gap array only grows — Gaps are never removed. A new Blackboard is created only when the Orchestrator determines the user's query is unrelated to the current workspace, or when the session ends.
2. **Code as the universal solver.** Every Gap is resolved by generating and executing code (a deterministic script or a reactive agent loop), not by prompting the LLM to "think harder."
3. **Failure containment.** A failing Gap does not corrupt the global Blackboard. Reactive tasks run inside encapsulated Micro-Agent instances running an isolated ReAct loop.
4. **Concurrency by default.** Independent Gaps execute in parallel via `tokio::JoinSet`. The DAG structure — not a global lock — determines ordering.
5. **Defense in depth.** All generated artifacts pass through a security scanner (DefenseClaw) before execution.

---

## 2. System Layers

```
L5  Interface          CLI daemon, HUD delta streamer
L4  Orchestrator       Intent decomposition, DAG management, response synthesis
L3  Blackboard         Living workspace: Gaps (append-only), Evidence, Gates, mutable intent
L2  Compiler/Executor  Gap-to-artifact compilation, sandboxed execution
L1  Memory             Session context (M1), local DB (M2), vector store (M3)
L0  Infrastructure     LLM providers, MCP bridge, DefenseClaw scanner
```

### Layer responsibilities

**L5 — Interface** `PARTIAL`
The user-facing surface. Currently a minimal async CLI (`tokio::io::BufReader` over stdin). The planned HUD component will stream Blackboard deltas (Gap state transitions, Evidence arrivals) to the terminal in real time.

| Sub-component | Status | Notes |
|---|---|---|
| CLI input loop | `IMPLEMENTED` | `main.rs` — reads lines, passes to `Moss::run`, prints response. |
| HUD delta streamer | `PLANNED` | Requires Blackboard change notification (see Section 5.2) |

**L4 — Orchestrator** `PARTIAL`
The strategic coordinator. Receives user input and the full Blackboard state (intent + Gaps + Evidence), decomposes the query into new Gaps, and refines the intent on follow-ups. Hands the plan to the **Runner** (L2) to drive execution. When all Gaps are Closed, synthesizes the final response from all Evidence using the latest intent.

| Sub-component | Status | Notes |
|---|---|---|
| Intent-to-DAG decomposition (single LLM call) | `IMPLEMENTED` | `orchestrator.rs` — `decompose` renders `prompts/decompose.md` via `minijinja`, calls LLM, deserializes into `Decomposition`. `Moss::run` inserts gaps into Blackboard. |
| Response synthesis (Evidence → answer) | `IMPLEMENTED` | `orchestrator.rs` — `synthesize` renders `prompts/synthesize.md`, passes real evidence from `blackboard.all_evidence()`. |
| Execution loop (poll, dispatch, evidence, synthesis) | `IMPLEMENTED` | `runner.rs` — full JoinSet fan-out loop. Wired in `Moss::run`. See Section 3. |
| Context injection (M1/M3 retrieval before planning) | `PLANNED` | — |

**L3 — Blackboard** `PARTIAL`
Living workspace using `DashMap` for lock-free concurrent access. Holds the intent (mutable), the Gap DAG (append-only), accumulated Evidence, and human-in-the-loop Gates. A Blackboard stays open across follow-up messages — new Gaps are inserted and the intent is refined on each decompose call. It is sealed only when the topic changes or the session ends (see Section 10).

| Sub-component | Status | Notes |
|---|---|---|
| Data structures (Gap, Evidence, Blackboard) | `IMPLEMENTED` | `blackboard.rs` — `GapState`, `GapType`, `Gap`, `EvidenceStatus`, `Evidence`, `Blackboard` with private fields and `pub(crate)` getters |
| Insert/mutate operations | `IMPLEMENTED` | `insert_gap`, `set_gap_state`, `append_evidence`, `insert_gate`, `set_intent` |
| Dependency resolution (auto-unblock) | `IMPLEMENTED` | `promote_unblocked`, `drain_ready`, `all_closed`, `all_gated_or_closed` — unit tested |
| Change notification (for HUD) | `PLANNED` | `tokio::broadcast` or watch channel |

**L2 — Compiler, Executor & Runner** `PARTIAL`
The Compiler takes a Gap description and emits an executable artifact. The Executor runs it and posts Evidence back to the Blackboard. The Runner drives the full execution loop.

| Sub-component | Status | Notes |
|---|---|---|
| Compiler | `IMPLEMENTED` | `compiler.rs` — renders `prompts/compiler.md`, calls LLM, deserializes into `Artifact` (Script or Agent). Language-agnostic. |
| Executor — script runner | `IMPLEMENTED` | `executor.rs` — zero-size unit struct. Writes code to `NamedTempFile`, spawns interpreter via `tokio::process::Command`, bounded by `tokio::time::timeout`. Writes Evidence to Blackboard. |
| Runner — execution loop | `IMPLEMENTED` | `runner.rs` — `JoinSet` fan-out, retry up to `MAX_RETRIES=3`, deadlock detection, gap promotion. |
| Executor — Micro-Agent host (ReAct loop) | `PLANNED` | Stub in `executor.rs` — writes Failure evidence. Phase 7. |
| Sandbox / isolation | `PLANNED` | — |

**L1 — Memory** `PLANNED`
Three-tier memory hierarchy for context across and within sessions.

| Tier | Store | Purpose | Status |
|---|---|---|---|
| M1 | In-process session context | Cross-board awareness: sealed board summaries, key entities | `PLANNED — design open` |
| M2 | Sled (embedded KV) | User preferences, audit trail | `PLANNED` — not in `Cargo.toml` |
| M3 | Qdrant (vector DB) | Knowledge Crystals — compressed outcomes from past sessions | `PLANNED` — not in `Cargo.toml` |

**L0 — Infrastructure** `PARTIAL`

| Sub-component | Status | Notes |
|---|---|---|
| Provider trait + OpenRouter impl | `IMPLEMENTED` | `providers/` — working against OpenRouter API |
| Local mock provider | `IMPLEMENTED` | `providers/local/mod.rs` |
| MCP client (tool bridge) | `PLANNED` | See Section 7 |
| DefenseClaw (pre-exec scanner) | `PLANNED` | See Section 8 |

---

## 3. Core Runtime Loop

This is the central execution flow that ties L4, L3, and L2 together. It is called once per user message. The same Blackboard may pass through this loop many times across follow-up messages.

`PARTIAL` — `MossKernel::handle_input` → `Orchestrator::decompose` → gap insertion → `Runner::execute` → `Orchestrator::synthesize`. Decompose and synthesize are implemented; Compiler, Executor, and Runner are planned.

### 3.1 Sequence

```
User input
    |
    v
[1] MossKernel: is there an active Blackboard?
    |
    YES                             NO
    |                               |
    v                               v
[2] Serialize board state       Create new Blackboard
    (to_planner_view)
    |                               |
    +---------------+---------------+
                    |
                    v
[3] Orchestrator.decompose(query, board_state)
    LLM returns: { intent, gaps[] }
                    |
                    v
[4] MossKernel: follow-up or new topic? (see Section 10.5)
    |
    Follow-up                   New topic
    |                           |
    Update intent               Seal current board → M3
    on current board            Create new board, set intent
    |                           |
    +---------------+-----------+
                    |
                    v
[5] Insert new Gaps into Blackboard
    - New Gaps with deps on existing Closed Gaps → Ready immediately
    - New Gaps with deps on other new Gaps → Blocked
    promote_unblocked()
                    |
                    v
[6] EXECUTION LOOP (Runner — runs until all *new* Gaps are Closed):
    |
    |   6a. Poll Blackboard for all Ready Gaps (drain_ready)
    |   6b. For each Ready Gap, spawn into tokio::JoinSet:
    |       6b-i.   Mark Gap as Assigned
    |       6b-ii.  Send Gap to Compiler (LLM call)
    |       6b-iii. Compiler returns artifact (Script or AgentSpec)
    |       6b-iv.  DefenseClaw scans artifact
    |       6b-v.   If high-risk: Gap → Gated, post Gate, skip execution
    |       6b-vi.  Executor runs artifact
    |       6b-vii. Executor posts Evidence to Blackboard
    |       6b-viii. Gap → Closed
    |   6c. After each JoinSet completion:
    |       - promote_unblocked() — check Blocked Gaps
    |       - Terminal check:
    |           all Gaps Closed                  → done
    |           all remaining Gaps Gated/Closed  → yield to user (print Gates)
    |           else deadlock
    |   6d. If terminal: break
    |
    v
[7] Orchestrator.synthesize() — reads latest intent + all Evidence
    |
    v
[8] Return response to L5. Blackboard enters Idle state.
    (Board stays in memory — ready for follow-up on next input)
```

### 3.2 Concurrency constraints

- **Fan-out limit.** A `tokio::Semaphore` caps the number of concurrently executing Gaps (default: 4). This bounds LLM call parallelism and subprocess count.
- **No mutable aliasing.** The `Blackboard` is behind `Arc` and uses `DashMap` internally, so concurrent readers/writers do not require a mutex. Gap state transitions are atomic per-entry.
- **Deadlock detection.** If the JoinSet drains to empty but Blocked gaps remain, the loop returns a `Deadlock` error rather than hanging. This can happen if the Orchestrator produces a DAG with a cycle or an unresolvable dependency.

---

## 4. Component Specifications

### 4.1 Orchestrator `PARTIAL`

**Responsibility:** Translate user intent into new Gaps; refine the Blackboard's intent on follow-ups; synthesize the final response from Evidence.

**Current state:** `decompose` and `synthesize` are separate methods. `decompose` renders `prompts/decompose.md` via `minijinja`, calls the LLM, and deserializes the response into a `Decomposition` struct. `synthesize` renders `prompts/synthesize.md` and calls the LLM. Gap insertion into the Blackboard happens in `MossKernel` after `decompose` returns.

**Current interface (as-built):**

```rust
pub(crate) struct Orchestrator {
    provider: Arc<dyn Provider>,
}

impl Orchestrator {
    pub(crate) fn new(provider: Arc<dyn Provider>) -> Self;
    pub(crate) async fn decompose(&self, query: &str, blackboard: &Blackboard) -> Result<Decomposition, MossError>;
    pub(crate) async fn synthesize(&self, blackboard: &Blackboard) -> Result<String, MossError>;
}
```

**Target interface:** `decompose` will receive a rich Blackboard view (intent + all Gaps with states + Evidence summaries) instead of just the intent string. This is what allows the Orchestrator to decide whether the query extends the current board or starts a new topic, and to reference existing Closed Gaps in new Gap dependencies.

```rust
impl Orchestrator {
    pub(crate) async fn decompose(
        &self,
        query: &str,
        board_view: &Value,  // Blackboard::to_planner_view() — rich JSON
    ) -> Result<Decomposition, MossError>;
}
```

**Decompose output contract:**

The LLM always returns `{ intent, gaps[] }`. There is no explicit mode or continuation flag.

- `intent` — the current goal of the Blackboard. On the first message this is the original intent. On follow-ups the Orchestrator refines it to capture the evolved scope (e.g., "Book a flight to Tokyo" → "Book a business class flight to Tokyo"). Always present.
- `gaps[]` — only the **new** Gaps needed for this query. On a follow-up, these may declare dependencies on existing Closed Gaps by name. On a new topic, these will have no references to the current board.

MossKernel uses the output to infer follow-up vs. new topic (see Section 10.5).

```json
{
  "intent": "string — the current/updated goal",
  "gaps": [
    {
      "name": "snake_case_identifier (unique across board lifetime)",
      "description": "what this gap resolves",
      "gap_type": "Proactive | Reactive",
      "dependencies": ["may reference existing Closed gaps or new gaps"],
      "constraints": null,
      "expected_output": "what a correct result looks like"
    }
  ]
}
```

**Rich Blackboard state (input to decompose):**

`Blackboard::to_planner_view()` serializes the board into a JSON structure the LLM can reason over:

```json
{
  "intent": "Book a flight to Tokyo",
  "gaps": [
    {
      "name": "search_flights",
      "state": "Closed",
      "description": "Search for available flights to Tokyo",
      "evidence_summary": { "found": 12, "cheapest": "$450" }
    }
  ]
}
```

For the first message in a session (no board exists), this is `{}`.

**Prompt contract:**
- `prompts/decompose.md` — Markdown instructions + XML-tagged input (`{{ user_query }}`, `{{ blackboard_state }}`). The prompt instructs the LLM to refine the intent on follow-ups, return only new Gaps, and avoid reusing names already on the board. See Section 10 for the full lifecycle.
- `prompts/synthesize.md` — Markdown instructions + XML-tagged input (`{{ intent }}`, `{{ evidence }}`). The `intent` is always the latest (refined) version. LLM returns a plain text response.

### 4.2 Blackboard `IMPLEMENTED`

**Responsibility:** Living workspace for the current conversation thread. Holds the intent (mutable), the Gap DAG (append-only), Evidence map, and HITL Gates. A Blackboard stays open across follow-up messages — the Orchestrator inserts new Gaps and updates the intent on each decompose call. It is sealed only when the topic changes or the session ends (see Section 10).

**Current state:** Core data structures, insert/mutate operations, dependency resolution, and ready-gap polling are implemented and unit tested. Pending: `to_planner_view()` method for rich serialization to the Orchestrator, change notification for HUD streaming.

**Implemented interface:**

```rust
impl Blackboard {
    /// Return and atomically mark as Assigned all gaps currently in Ready state.
    pub(crate) fn drain_ready(&self) -> Vec<Gap>;

    /// For every Blocked gap whose dependencies are all Closed, promote to Ready.
    pub(crate) fn promote_unblocked(&self);

    /// True when every gap is in Closed state.
    pub(crate) fn all_closed(&self) -> bool;

    /// True when every gap is in Closed or Gated state.
    pub(crate) fn all_gated_or_closed(&self) -> bool;

    /// Retrieve a gap by ID (cloned for send across await).
    pub(crate) fn get_gap(&self, id: &Uuid) -> Option<Gap>;

    /// Retrieve a gap UUID by name slug. Used by promote_unblocked and dependency resolution.
    pub fn get_gap_id_by_name(&self, name: &str) -> Option<Uuid> { ... }

    /// Subscribe to state changes (for HUD streaming).
    pub fn subscribe(&self) -> broadcast::Receiver<BlackboardDelta> { ... }
}
```

**Name→UUID reverse index:**
`Gap.dependencies` stores names (`Vec<Box<str>>`), but the gap map is keyed by `Uuid`. A secondary index `name_index: DashMap<Box<str>, Uuid>` is populated atomically in `insert_gap` alongside the primary map. This makes `promote_unblocked` O(D) per gap (D = dependency count) instead of O(N·D) with a scan. The index is append-only — gap names are immutable after insertion.

**Data structures — as implemented:**

`blackboard.rs` implements the full target design. All struct fields are private; access is via `pub(crate)` getters. Types are `pub(crate)`. The structs are:

```rust
// All fields private; access via pub(crate) getters only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Gap {
    gap_id: Uuid,
    name: Box<str>,              // snake_case slug from the plan
    state: GapState,
    description: Box<str>,       // consumed by the Compiler
    gap_type: GapType,           // Proactive or Reactive
    dependencies: Vec<Box<str>>, // names of gaps this depends on
    constraints: Option<Value>,
    expected_output: Option<Box<str>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum GapType {
    Proactive,
    Reactive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Evidence {
    gap_id: Uuid,
    attempt: u32,            // 1-based attempt number (for retry history)
    content: Value,
    status: EvidenceStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum EvidenceStatus {
    Success,
    Failure { reason: String },
    Partial,                     // Micro-Agent hit iteration cap before goal was met
}
```

`Blackboard.evidences` is `DashMap<Uuid, Vec<Evidence>>` — an ordered attempt log per gap. The Compiler for retry attempt N receives the `Vec<Evidence>` slice `[0..N-1]` so it can see prior errors and adapt.

`Blackboard` includes `name_index: DashMap<Box<str>, Uuid>` for O(1) name-to-ID resolution. Written once in `insert_gap`, never mutated after that. `intent` is stored as `Mutex<Option<Box<str>>>` for safe mutation through a shared `&self` reference.

**Thread-safety model:**
`DashMap` provides per-shard read/write locks internally. Individual Gap state transitions (Ready -> Assigned) must be atomic. Use `DashMap::get_mut` which holds a write lock on the shard for the duration of the returned `RefMut`. The `drain_ready` method should iterate and CAS (compare-and-swap) in a single pass to avoid TOCTOU races where two threads both see the same gap as Ready.

### 4.3 Compiler `PLANNED`

**Responsibility:** Accept a Gap description and emit an executable artifact — either a self-contained script (Proactive) or a Micro-Agent specification (Reactive).

**Interface:**

```rust
pub struct Compiler {
    provider: Arc<dyn Provider>,
}

pub enum Artifact {
    Script {
        language: ScriptLanguage,  // Python, Bash, etc.
        code: String,
        timeout: Duration,
    },
    AgentSpec {
        role: String,
        goal: String,
        tools: Vec<String>,       // MCP tool names
        instructions: String,
        max_iterations: u32,
        timeout: Duration,
    },
}

impl Compiler {
    /// Compile a Gap into an executable Artifact.
    /// `prior_attempts` contains all Evidence records from previous failed attempts
    /// at this Gap (empty on first attempt). The Compiler uses them to adapt.
    pub async fn compile(&self, gap: &Gap, prior_attempts: &[Evidence]) -> Result<Artifact>;
}
```

**Prompt contract (`compiler.xml`):**
The Compiler prompt receives `{gap}` (the Gap description, type, and constraints) and `{resolved_data_from_dependencies}` (serialized Evidence from this Gap's dependencies). It returns a JSON object with `execution_mode` (SCRIPT or AGENT) and a `payload` containing either `python_code` or an `agent_spec`.

**Design decisions:**
- The Compiler must not have access to the full Blackboard — only the specific Gap description and its resolved dependency Evidence. This enforces the principle of least privilege and keeps the LLM context window focused.
- Script artifacts are self-contained: they must include all imports, accept input via stdin or environment variables, and write output to stdout as JSON.
- Agent specs are declarative: they describe *what* the agent should achieve, not the exact steps. The Executor's agent runtime interprets the spec.

### 4.4 Executor `PLANNED`

**Responsibility:** Run artifacts in isolation and produce Evidence.

**Interface:**

```rust
pub struct Executor {
    mcp: Arc<McpBridge>,
    sandbox_config: SandboxConfig,
}

impl Executor {
    /// Run an artifact and return the resulting Evidence.
    pub async fn run(&self, gap_id: Uuid, artifact: Artifact) -> Result<Evidence>;
}
```

**Script execution model:**
1. Write the script to a temporary file inside a sandbox directory.
2. Spawn a child process (`tokio::process::Command`) with restricted environment: no network access for Proactive scripts (they receive all data via stdin), bounded CPU time via `timeout`, bounded memory via cgroups or ulimit.
3. Capture stdout as JSON. Parse into `Evidence.content`.
4. If the process exits non-zero or times out, return an error Evidence with the stderr content, and let the Orchestrator decide whether to retry or fail the gap.

**Micro-Agent execution model:**
1. The `Compiler` returns `Artifact::AgentSpec { goal, tools, instructions, max_iterations, timeout }`.
2. `Executor::run()` constructs `MicroAgent { goal, tools, max_iterations, provider, context }` where `context` is the read-only dependency Evidence passed in. No sub-Blackboard is created.
3. `MicroAgent::run()` executes a ReAct loop using an internal `Vec<Message>` as local scratch memory. This history never touches the parent Blackboard.
4. Each iteration: LLM call with tool definitions scoped to `tools` → LLM returns tool call or final answer → if tool call, invoke via `McpBridge` → append observation to history → check if goal is met.
5. On exit (goal met or `max_iterations` exhausted): serialize the final answer and key observations into a single `Evidence` record. Internal history is discarded. Evidence is returned to the Executor, which posts it to the parent Blackboard and marks the Gap Closed.

```rust
pub struct MicroAgent {
    goal: String,
    tools: Vec<String>,          // permitted MCP tool names only — least privilege
    max_iterations: u32,
    provider: Arc<dyn Provider>, // same provider pool, no new Orchestrator
    context: Vec<Evidence>,      // dependency Evidence — read-only input
    history: Vec<Message>,       // internal scratch — never written to Blackboard
}

impl MicroAgent {
    pub async fn run(mut self, mcp: &McpBridge) -> Result<Evidence>;
}
```

### 4.5 DAG Scheduler

The scheduler is not a separate component — it is the execution loop inside `Runner` (Section 3). This is a deliberate simplification: an external scheduler would add an inter-component communication layer without clear benefit at this scale.

**Scheduling strategy:** Non-preemptive, event-driven. Gaps are not assigned on a timer; they are spawned into the JoinSet when (a) they become Ready and (b) a semaphore permit is available. When a gap completes and posts Evidence, the `promote_unblocked` sweep runs synchronously before the next iteration, ensuring newly-unblocked gaps are immediately eligible.

**Failure policy:**

| Failure type | Behavior |
|---|---|
| Script exits non-zero | Retry up to N times (default 2). The error stderr is stored as `EvidenceStatus::Failure { reason }`. On each retry, `compiler.compile(gap, prior_attempts)` receives all prior failure records so it can adapt the generated code. After N failures, mark Gap as Closed with `EvidenceStatus::Failure` and propagate to dependents. |
| Micro-Agent exceeds iteration cap | Serialize partial history as a summary. Mark Closed with `EvidenceStatus::Partial`. |
| Micro-Agent exceeds timeout | Abort MicroAgent ReAct loop, collect partial history as Evidence. Mark Closed with `EvidenceStatus::Partial`. |
| LLM provider error (rate limit, timeout) | Exponential backoff with jitter, up to 3 retries. |
| Deadlock (Blocked gaps remain, no Ready/Assigned/Gated) | Return `MossError::Deadlock`. Log full DAG state. |

---

## 5. Memory Hierarchy `PLANNED`

### 5.1 M1 — Session Context `PLANNED — design open`

M1 provides session-level awareness across sealed Blackboards. When a Blackboard is sealed (topic change), the Orchestrator needs a lightweight summary of what happened on prior boards — not the full Evidence, but enough to know the session history.

The exact structure is TBD. Candidates include a list of per-board summaries (intent + outcome + key entities), a running entity map (names, preferences, references discovered during the session), or Crystal IDs for same-session boosting in M3 retrieval.

Note: within a single Blackboard, M1 is not needed — the Blackboard itself holds all Gaps, Evidence, and the current intent. M1 only matters for context that spans sealed Blackboard boundaries within the same session.

### 5.2 M2 — Sled (Local Preferences & Audit)

An embedded key-value store for data that must survive across sessions but does not need semantic search.

Contents: user preferences (default model, concurrency limits, tool permissions), an append-only audit log of all executed artifacts (for security review), and session metadata (start time, gap count, outcome).

**Dependency:** `sled` crate (to be added to `Cargo.toml`).

### 5.3 M3 — Qdrant (Knowledge Crystals)

A vector database for semantic retrieval of compressed past session outcomes.

**Crystallization trigger:** When a Blackboard is sealed (topic change or session end) and contains at least one Closed Gap with `EvidenceStatus::Success`, MossKernel generates a Knowledge Crystal:

```rust
pub struct Crystal {
    session_id: Uuid,
    intent: String,
    outcome_summary: String, // LLM-compressed summary of all Evidence
    embedding: Vec<f32>,     // embedding of intent + outcome
    timestamp: DateTime<Utc>,
    tags: Vec<String>,       // extracted entities, tool names used
}
```

**Retrieval:** Before the decomposition step, the Orchestrator embeds the new query and retrieves the top-K (default 5) most similar Crystals from Qdrant. These are injected into the planning prompt as prior context, giving the system "memory" of how it solved similar problems before.

**Dependency:** `qdrant-client` crate (to be added to `Cargo.toml`).

---

## 6. Provider Abstraction `IMPLEMENTED`

The `Provider` trait abstracts LLM access behind a single async method:

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    async fn complete_chat(&self, messages: Vec<Message>) -> Result<String, ProviderError>;

    /// Default: returns `Err(ProviderError::NotSupported)`.
    /// Override in providers that support function/tool calling.
    async fn complete_with_tools(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
    ) -> Result<ToolCallOrText, ProviderError> {
        Err(ProviderError::NotSupported)
    }
}
```

**Current implementations:**

| Provider | Status | Notes |
|---|---|---|
| OpenRouter | `IMPLEMENTED` | Supports any model available via OpenRouter API |
| LocalMock | `IMPLEMENTED` | Echo-back mock for testing |
| Local vLLM | `PLANNED` | Direct inference on local GPU via vLLM's OpenAI-compatible API |

**Remaining work:**
- **Streaming.** `PLANNED` — For the HUD to stream partial responses, add `complete_chat_stream` returning a `Stream<Item = Result<String>>`.
- **Tool calling.** `PLANNED` — `complete_with_tools` stub exists; full implementation is required for the Micro-Agent's ReAct loop.

---

## 7. MCP Integration `PLANNED`

MCP (Model Context Protocol) is the standardized bridge between the LLM and external tools (filesystem, browser, APIs, databases).

**Design:**

```rust
pub struct McpBridge {
    servers: Vec<McpServerHandle>,
    tool_registry: HashMap<String, ToolDefinition>,
}

impl McpBridge {
    /// Discover all tools from connected MCP servers.
    pub async fn discover(&mut self) -> Result<()>;

    /// Invoke a tool by name with JSON arguments.
    pub async fn call(&self, tool_name: &str, args: Value) -> Result<Value>;

    /// Return tool definitions formatted for LLM function-calling.
    pub fn tool_definitions(&self) -> Vec<ToolDefinition>;
}
```

**Transport:** JSON-RPC 2.0 over stdio (spawn MCP server as a child process and communicate via stdin/stdout). This is the standard MCP transport.

**Dependency:** `mcp-rust-sdk` or manual JSON-RPC implementation over `tokio::process::Command`.

**Tool scoping:** The Executor provides each Micro-Agent only the tools listed in its `AgentSpec.tools` field. This prevents a web-browsing agent from accessing the filesystem, and a file-management agent from making network calls.

---

## 8. Security: DefenseClaw `PLANNED`

DefenseClaw is a pre-execution scanner that inspects every artifact before the Executor runs it. It operates as a pipeline of checks, any of which can reject the artifact.

**Scan pipeline:**

| Stage | What it checks | Method |
|---|---|---|
| 1. Static analysis | Forbidden imports (`os.system`, `subprocess`, `shutil.rmtree`), network calls in Proactive scripts, filesystem writes outside sandbox | AST parsing (Python `ast` module via a small Python helper, or `tree-sitter` from Rust) |
| 2. Capability check | Does the artifact require capabilities beyond what the Gap's constraints allow? | Compare requested tool names against the Gap's permitted tool list |
| 3. Resource bounds | Are timeout and memory limits set? | Config validation |
| 4. HITL gate | Is this a high-risk action (e.g., sending email, deleting files, making purchases)? | Pattern match against a configurable action blocklist; if matched, pause and prompt user for confirmation via a Gate on the Blackboard |

**Interface:**

```rust
pub struct DefenseClaw {
    blocklist: Vec<Pattern>,
    max_script_size: usize,
}

/// A single scan pass produces one of three verdicts — never ambiguous.
pub enum ScanVerdict {
    /// Artifact is clean. Proceed to execution.
    Approved,
    /// High-risk action detected. Pause Gap, surface Gate to user.
    Gated { reason: String },
    /// Hard violation (forbidden import, oversized script, etc.). Do not execute.
    Rejected { reason: String },
}

impl DefenseClaw {
    /// Run all scan stages in one pass and return a single verdict.
    /// Callers dispatch on the variant — no two-method TOCTOU window.
    pub fn scan(&self, artifact: &Artifact, constraints: &Value) -> ScanVerdict;
}
```

**Non-goals:** DefenseClaw is not a sandbox. It is a static pre-flight check. Runtime isolation is the Executor's responsibility (subprocess with restricted env, cgroups, etc.). Defense in depth means both layers exist.

---

## 9. Session Lifecycle

A **Session** is the lifetime of the running Moss process. It holds at most one active Blackboard at any time, plus references to Crystals produced from previously sealed Blackboards. A single session typically has few Blackboards — the active one stays open across follow-ups and is only sealed when the topic changes.

```
[Moss starts]
      |
      v
  Create Session (new Uuid)
      |
      v
  Wait for user input ◄──────────────────────────────────────┐
      |                                                       │
      v                                                       │
  Is there an active Blackboard?                              │
      |                                                       │
   NO |          YES                                          │
      |           |                                           │
      v           v                                           │
  Create new    Orchestrator.decompose()                      │
  Blackboard    with full board state                         │
      |           |                                           │
      |      +---------+                                      │
      |      |         |                                      │
      |  Follow-up  New topic                                 │
      |      |         |                                      │
      |      |     Seal current board → Crystal → M3          │
      |      |     Create new Blackboard                      │
      |      |         |                                      │
      +------+---------+                                      │
      |                                                       │
      v                                                       │
  Update intent, insert new Gaps                              │
  Runner.execute() (Active state)                             │
      |                                                       │
    +-+-----------------------------------+                   │
    |                                     |                   │
    v                                     v                   │
 All Gaps Closed                   Gated Gaps remain          │
    |                              (user approval needed)     │
    |                                     |                   │
    |                         Surface Gates; await input      │
    |                              approve / reject           │
    |                                     |                   │
    |                         Gap → Ready / Closed            │
    |                                     |                   │
    +<------------------------------------+                   │
    |                                                         │
    v                                                         │
  Orchestrator.synthesize() → response (Idle state)           │
  Return response to user                                     │
      |                                                       │
      └───────────────────────────────────────────────────────┘

[Session ends only on user exit or process crash → seal active board]
```

**Key invariants:**
- At most one Blackboard is active (Created/Active/Idle) per session at any time.
- A Blackboard stays open across follow-up messages. It is sealed only on topic change or session end.
- Gated interactions happen within the Blackboard's Active state — the board is never sealed while Gates are pending.
- A Sealed Blackboard is an immutable historical record, compressed into a Crystal in M3.
- The session has no idle timeout. It lives until the user exits or the process crashes.

**Crystallization** happens when a Blackboard is sealed: MossKernel compresses the board's outcomes into a Knowledge Crystal saved to M3. Only Blackboards with at least one Closed Gap with `EvidenceStatus::Success` produce a Crystal.

---

## 10. Blackboard Lifecycle

A Blackboard is a **workspace**, not a transaction. It stays open across multiple user messages as long as the conversation remains related. The Orchestrator appends new Gaps on each follow-up, and the intent evolves to capture the growing scope. A new Blackboard is created only when the Orchestrator determines the user has moved to an unrelated topic, or when the session ends.

### 10.1 Lifecycle States

```
Created ──> Active ──> Idle ──> Active  (follow-up adds new Gaps)
                         │
                         └──> Sealed    (new topic, or session ends)
```

| State | Description |
|---|---|
| **Created** | MossKernel instantiates a new `Blackboard` (fresh `Uuid`). Intent is not yet set; Gap DAG is empty. |
| **Active** | Gaps are in flight. The Runner is executing. The Blackboard accepts writes: Gap state changes, Evidence appends, Gate insertions. |
| **Idle** | All current Gaps have reached a terminal state (`Closed`). Synthesis has returned a response to the user. **The Blackboard remains writable** — new Gaps can be inserted on the next user message. It is waiting for input. |
| **Sealed** | Crystallized and immutable. The Blackboard has been compressed into a Knowledge Crystal (M3) and is never written to again. |

### 10.2 Lifecycle Transitions

**Created → Active:** The first `orchestrator.decompose()` call sets the intent and inserts the initial Gaps. The Runner begins execution.

**Active → Idle:** `blackboard.all_closed()` returns `true`. The Runner exits. Synthesis runs and the response is returned to the user. The Blackboard stays in memory, holding all Gaps and Evidence, waiting for the next message.

**Active → Active (HITL loop):** When `blackboard.all_gated_or_closed()` is `true` but `all_closed()` is `false`, the Blackboard stays Active. Moss surfaces pending Gates to the user, waits for `approve <name>` / `reject <name>`, updates the affected Gaps, and resumes the Runner.

**Idle → Active (follow-up):** A new user message arrives. MossKernel calls `orchestrator.decompose()` with the new query and the full Blackboard state (all existing Gaps, Evidence, and current intent). The Orchestrator returns an updated intent and new Gaps. MossKernel updates the intent, inserts the new Gaps, and kicks off the Runner again. New Gaps may declare dependencies on existing Closed Gaps — those dependencies are already satisfied, so the new Gaps promote to Ready immediately.

**Idle → Sealed (new topic):** A new user message arrives, but the Orchestrator's decompose output signals it is unrelated — the returned intent shares no continuity with the existing board, and no new Gaps reference existing ones. MossKernel seals the current Blackboard (crystallize → M3), creates a fresh Blackboard, and runs the decompose output against it.

**Idle → Sealed (session end):** The user exits or the process crashes. MossKernel seals the Blackboard and crystallizes it.

### 10.3 Intent Evolution

The Blackboard's `intent` is **mutable**. Each decompose call may refine it to reflect the user's evolving goal.

```
Round 1: "Book me a flight to Tokyo"
  → intent: "Book a flight to Tokyo"

Round 2: "Make it business class"
  → intent: "Book a business class flight to Tokyo"

Round 3: "Also find a hotel near Shibuya for 3 nights"
  → intent: "Book a business class flight to Tokyo and a hotel near Shibuya for 3 nights"
```

The intent is a living summary of what the user is trying to accomplish on this Blackboard. The Orchestrator updates it on every decompose call. The synthesis step reads the current intent (not the original) to produce the final response.

### 10.4 Growable Gap DAG

Gaps are append-only. Once inserted, a Gap is never removed from the Blackboard. New Gaps are added on each follow-up, and they can reference any existing Gap by name in their `dependencies` field.

```
Round 1 inserts:
  search_flights (Closed) ──> select_best_flight (Closed)

Round 2 inserts:
  upgrade_to_business (depends on select_best_flight — already Closed, so immediately Ready)

Round 3 inserts:
  search_hotels (no deps — immediately Ready)
  book_hotel (depends on search_hotels)
```

The Gap DAG grows monotonically. Closed Gaps from prior rounds are inert — the Runner skips them. `promote_unblocked()` and `drain_ready()` naturally handle the mix of old Closed Gaps and new Ready/Blocked Gaps without any changes to the scheduling logic.

**Name uniqueness:** Gap names must be unique across the entire Blackboard lifetime. The `name_index` enforces this. The decompose prompt instructs the LLM not to reuse names already on the board.

### 10.5 New-Topic Detection

There is no separate classifier or explicit mode flag. The Orchestrator's decompose call absorbs this decision because it already receives the full Blackboard state (intent + all Gaps + Evidence summaries).

The decompose output always contains an `intent` and a `gaps` array. MossKernel infers new-topic from the output:

- **Follow-up:** The returned intent refines or extends the existing one. New Gaps reference existing Closed Gaps in their dependencies. MossKernel updates the intent and inserts the Gaps on the current Blackboard.
- **New topic:** The returned intent has no relationship to the existing one. No new Gaps reference existing ones. MossKernel seals the current Blackboard and creates a fresh one.

This detection is lightweight — MossKernel checks whether any new Gap names an existing Gap as a dependency. If at least one does, it's a follow-up. If none do and the intent diverges, it's a new topic. No LLM call beyond the decompose that was already happening.

### 10.6 Ownership and Creation

MossKernel is the sole owner of the Blackboard lifecycle. The Orchestrator and Runner receive `Arc<Blackboard>` and may read/write Gaps and Evidence, but they never create or seal a Blackboard. Creation and sealing are the kernel's responsibility.

```rust
// Pseudocode — MossKernel::handle_input
async fn handle_input(&mut self, query: &str) -> Result<String, MossError> {
    // Get current board state for the Orchestrator
    let board_view = match &self.active_board {
        Some(bb) => bb.to_planner_view(),   // rich JSON: intent + gaps + evidence
        None => json!({}),                  // first message in session
    };

    // Decompose — Orchestrator sees full board state, decides intent + new gaps
    let plan = self.orchestrator.decompose(query, &board_view).await?;

    // Determine: follow-up or new topic?
    let blackboard = if self.is_follow_up(&plan) {
        // Same board — update intent, insert new gaps
        let bb = self.active_board.clone().unwrap();
        if let Some(ref intent) = plan.intent {
            bb.set_intent(intent.as_str());
        }
        bb
    } else {
        // New topic — seal old board, create fresh one
        if let Some(old) = self.active_board.take() {
            self.memory.crystallize(&old).await?;  // Sealed
        }
        let bb = Arc::new(Blackboard::new());
        bb.set_intent(plan.intent.as_deref().unwrap_or("unknown"));
        bb
    };

    // Insert new gaps (works for both follow-up and fresh board)
    if let Some(gaps) = plan.gaps {
        for spec in gaps {
            blackboard.insert_gap(Gap::from_spec(spec))?;
        }
    }
    blackboard.promote_unblocked();

    // Execute + synthesize
    self.runner.execute(blackboard.clone()).await?;   // Active → Idle on return
    let response = self.orchestrator.synthesize(&blackboard).await?;

    // Park in Idle
    self.active_board = Some(blackboard);
    Ok(response)
}
```

### 10.7 Invariants

- A Blackboard's Gap array only grows. Gaps are never removed or replaced.
- The intent is mutable — updated by the Orchestrator on each decompose call. The synthesis step always reads the latest intent.
- A Sealed Blackboard is immutable. No code path writes to it after crystallization.
- There is at most one active (Created/Active/Idle) Blackboard per session at any time.
- Sealing happens in two cases only: the Orchestrator signals a new topic, or the session ends.

---

## 11. Gap Lifecycle

```
Blocked ──> Ready ──> Assigned ──> Gated ──> Ready  (on user approval)
                                 │
                                 └─────────> Closed  (on user rejection)
                    Assigned ──────────────> Closed  (normal completion)
```

This is a one-directional state machine. The only backward arc is `Gated → Ready`, which requires explicit user action.

| State | Entry condition | Exit condition |
|---|---|---|
| **Blocked** | Gap has dependencies that are not yet Closed | All dependencies reach Closed state; auto-promoted to Ready by `promote_unblocked()` |
| **Ready** | No unresolved dependencies; eligible for scheduling | Picked up by the Runner and marked Assigned |
| **Assigned** | Compiler has been invoked; Executor is running | Executor posts Evidence and marks the gap Closed, OR DefenseClaw flags high-risk → Gated |
| **Gated** | DefenseClaw detected a high-risk action requiring user approval | User runs `approve <name>` → back to Ready; user runs `reject <name>` → Closed with terminal failure |
| **Closed** | Terminal. The gap is resolved (success, terminal failure, or user rejection) | — |

**Gaps with no dependencies** skip Blocked and are inserted directly as Ready.

**Terminal failure:** A gap can be Closed with `Evidence.status = EvidenceStatus::Failure { reason }`. Downstream gaps that depend on a terminally-failed gap are also marked as terminally failed without execution — the Orchestrator propagates failure through the DAG.

---

## 12. Error Handling Strategy `PLANNED`

The current codebase uses `.expect()` and `panic!()` pervasively. For a daemon process, panics are fatal. The error handling strategy going forward:

**Crate-level error type:**

```rust
#[derive(Debug, thiserror::Error)]
pub enum MossError {
    #[error("provider error: {0}")]
    Provider(#[from] ProviderError),

    #[error("compiler error for gap {gap_id}: {reason}")]
    Compiler { gap_id: Uuid, reason: String },

    #[error("executor error for gap {gap_id}: {reason}")]
    Executor { gap_id: Uuid, reason: String },

    #[error("defense scan rejected artifact: {reason}")]
    DefenseRejection { reason: String },

    #[error("blackboard error: {0}")]
    Blackboard(String),

    #[error("deadlock: blocked gaps remain but no gaps are ready or assigned")]
    Deadlock,

    #[error("MCP tool error: {tool} — {reason}")]
    Mcp { tool: String, reason: String },

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
```

**Policy:** Every function that can fail returns `Result<T, MossError>`. The top-level `main.rs` loop catches errors and prints them to stderr without crashing. Individual Gap failures are isolated — they do not bring down the session.

---

## 13. Architecture Decisions

### ADR-001: Blackboard Pattern over Message-Passing Agents

**Status:** Accepted

**Context:** The system needs to coordinate multiple specialist tasks (web search, file operations, code generation) that operate on shared context. Two common patterns: (a) Blackboard — shared memory with a central coordinator reading/writing, (b) Actor/message-passing — each agent has private state and communicates via async channels.

**Decision:** Blackboard pattern, implemented with `DashMap` for concurrent access.

**Rationale:** The Orchestrator needs a global view of all Gaps and Evidence to make scheduling decisions and detect deadlocks. With message-passing, this global view requires either a centralized broker (which is functionally a Blackboard) or expensive all-to-all communication. The Blackboard makes the shared state explicit and inspectable, which simplifies debugging and enables the HUD to stream deltas directly from the data structure.

**Trade-offs:** Blackboard contention under very high parallelism (mitigated by DashMap's per-shard locking). Less isolation between components than pure message-passing. The DashMap approach means we cannot trivially distribute across processes — this is acceptable for a single-machine AIOS.

### ADR-002: Rust as Implementation Language

**Status:** Accepted

**Context:** The system is a local daemon with hard latency requirements (sub-second response to scheduling decisions) and concurrent execution of LLM calls, subprocesses, and tool invocations.

**Decision:** Rust with Tokio async runtime.

**Rationale:** Zero-cost async, no GC pauses, strong type system for modeling state machines (Gap lifecycle), and excellent subprocess management. The `DashMap` + `tokio::JoinSet` combination gives us concurrent DAG execution without manual thread management.

**Trade-offs:** Slower iteration speed than Python. Smaller ecosystem for LLM tooling (though `async-openai` and the MCP Rust SDK exist). Higher learning curve for contributors.

### ADR-003: LLM-Generated Code as the Execution Primitive

**Status:** Accepted

**Context:** Gaps need to be resolved by "doing something" — calling APIs, transforming data, navigating websites. Options: (a) a fixed toolkit of Rust-native functions the LLM selects from, (b) LLM generates executable code (scripts) on the fly.

**Decision:** LLM generates code. The Compiler produces Python scripts or agent specs.

**Rationale:** A fixed toolkit scales linearly with development effort and suffers from selection errors as it grows (the LLM must choose from an ever-larger menu). Code generation scales with the LLM's capability: as models improve, the range of solvable Gaps expands without code changes to Moss. Scripts are also inspectable and auditable (logged to M2).

**Trade-offs:** Security risk from executing LLM-generated code (mitigated by DefenseClaw + sandboxing). Latency overhead of an extra LLM call per Gap (mitigated by parallelism). Debugging is harder when the execution logic is generated at runtime.

### ADR-004: Micro-Agent = ReAct Loop, Not Recursive Orchestrator

**Status:** Accepted

**Context:** Reactive Gaps require non-deterministic real-world interaction (web browsing, API discovery, multi-step tool use). The initial design proposed spawning a recursive Orchestrator + Blackboard pair for each Reactive Gap.

**Decision:** A Reactive Gap is executed by a `MicroAgent` running a ReAct (Reason → Act → Observe) loop. It does not instantiate a new Orchestrator, does not have a sub-Blackboard, and does not call the Compiler. The only output is a single `Evidence` record posted to the parent Blackboard.

**Rationale:** The Blackboard pattern exists to coordinate parallel planning across multiple independent tasks. A ReAct loop is inherently sequential and self-contained. Giving it a full Orchestrator adds two extra LLM calls (decompose + synthesize), a sub-Blackboard that has no observability from the parent, and unbounded recursion risk. The MicroAgent struct is simpler, faster to implement, and its entire execution is scoped to one Gap.

**Trade-offs:** A MicroAgent cannot itself spawn parallel sub-tasks. If a Reactive Gap is genuinely complex enough to warrant parallel decomposition, it should be decomposed at planning time by the Orchestrator into multiple Gaps — not at runtime inside a MicroAgent.

### ADR-005: Human-in-the-Loop via `GapState::Gated`

**Status:** Accepted

**Context:** Some Gap artifacts generated by the Compiler represent high-risk actions (deleting files, sending email, making purchases). Executing these without user confirmation is unsafe. The architecture needs a pause mechanism.

**Decision:** When DefenseClaw flags a high-risk artifact, the Gap transitions to `Gated` state. The Runner's execution loop skips Gated Gaps. The CLI surfaces all pending Gates. The user runs `approve <name>` to allow execution (Gap transitions back to Ready) or `reject <name>` to abort it (Gap transitions to Closed with `EvidenceStatus::Failure`).

**Rationale:** `Gated` is a first-class state in the Gap lifecycle — not an error, not a special case. The execution loop already handles "skip non-Ready gaps" semantics for Blocked gaps; Gated reuses the same pattern. The Gate is stored on the Blackboard, making it observable by the HUD. No background timer or side channel is needed.

**Trade-offs:** A Gated Gap blocks all downstream Gaps that depend on it, since they cannot promote from Blocked until their dependency is Closed. This is correct behaviour — downstream tasks that depend on a human-gated action cannot proceed until that action is confirmed.

### ADR-006: Living Blackboard with Mutable Intent and Growable DAG

**Status:** Accepted — supersedes the "round-scoped immutable Blackboard" design from v0.4.

**Context:** The original design created a fresh Blackboard for every user message and sealed it immediately after synthesis. Follow-ups required reconstructing context from M1 summaries or M3 Crystals — a lossy process that threw away the rich Evidence the system just produced. The sealed-per-round model also introduced an artificial lifecycle boundary that didn't match how users actually interact: a follow-up like "make it business class" after "book a flight" is clearly the same conversation thread, not a new one.

**Decision:** A Blackboard is a living workspace. It stays open across follow-up messages. On each user input, the Orchestrator receives the full Blackboard state (intent + Gaps + Evidence summaries), returns an updated intent and new Gaps. New Gaps are appended — the Gap array only grows. The intent is mutable and evolves to reflect the user's expanding scope. The Blackboard is sealed only when the Orchestrator's decompose output signals a new, unrelated topic, or when the session ends.

**Rationale:**
- Follow-ups get full-fidelity access to prior Evidence — no information loss from summarization or crystallization.
- The Runner and DAG scheduler require zero changes: `drain_ready()` skips Closed Gaps, `promote_unblocked()` handles dependencies on already-Closed Gaps naturally, `insert_gap()` works on a board with existing Closed Gaps.
- New-topic detection is absorbed into the decompose call — no separate classifier, no extra LLM call, no explicit mode flag. MossKernel infers it from whether new Gaps reference existing ones.
- The Orchestrator already receives the Blackboard state for planning. Asking it to also refine the intent and decide topic continuity adds zero cost.

**Trade-offs:**
- A long-running Blackboard accumulates many Gaps and Evidence records. The `to_planner_view()` serialization sent to the Orchestrator could grow large. Mitigation: summarize Evidence in the planner view rather than including raw content; cap the number of Gap entries shown to the LLM.
- Crystallization timing changes: Crystals are now produced less frequently (on topic change rather than every message). Each Crystal covers more ground, which may be better or worse for M3 retrieval precision. This is an open question to evaluate once M3 is implemented.
- The "new-topic" inference heuristic (no new Gaps reference existing ones + intent diverges) may have edge cases. If it proves unreliable, a fallback is an explicit user command (`/new`) to force a board seal.

---

## 14. Open Questions

These are unresolved design decisions that need answers before or during implementation.

1. ~~**Re-planning.**~~ **Closed — Decision:** No re-planning in v1. Terminal failure propagates through the DAG downstream. Dependent Gaps are marked Closed with `EvidenceStatus::Failure`. Re-planning is deferred to v2 and requires explicit plan versioning and a `replace_subgraph` API on the Blackboard.

2. **Streaming vs. batch Evidence.** Should the Executor post Evidence incrementally as a script produces output (streaming), or only after the script completes (batch)? Streaming enables the HUD to show progress, but complicates the "done" semantics on Evidence and the dependency resolution logic.

3. **Embedding model for M3.** Which embedding model for Knowledge Crystal vectors? Options: a local model (e.g., `nomic-embed-text` on the RTX 4090), or a remote API (e.g., OpenAI embeddings via OpenRouter). Local keeps it offline; remote is simpler to start with.

4. **Multi-user / multi-session.** The current design is single-user, single-session. If Moss ever serves multiple concurrent sessions (e.g., as a daemon handling multiple terminal windows), the Blackboard needs session-scoped namespacing and the Memory tiers need per-user isolation.

5. ~~**Micro-Agent recursion depth.**~~ **Closed — Decision:** Micro-Agents are flat ReAct loops. They do not instantiate a new Orchestrator, do not have a sub-Blackboard, and do not call the Compiler. Recursion depth is 0. Not applicable.

---

## 15. Implementation Status Matrix

| Component | Layer | Status | Notes |
|---|---|---|---|
| CLI input loop | L5 | `IMPLEMENTED` | `main.rs` — reads stdin, calls `Moss::run`, prints response |
| HUD delta streamer | L5 | `PLANNED` | Requires Blackboard change notifications |
| Orchestrator decompose | L4 | `IMPLEMENTED` | `orchestrator.rs` — minijinja template, LLM call, JSON deserialization; gaps inserted into Blackboard in `Moss::run` |
| Orchestrator synthesize | L4 | `PARTIAL` | Code exists; Evidence is still placeholder until Runner fills it |
| Orchestrator execution loop | L4 | `PLANNED` | Runner not yet wired |
| Blackboard data structures | L3 | `IMPLEMENTED` | All types, private fields, `pub(crate)` getters |
| Blackboard insert/mutate | L3 | `IMPLEMENTED` | `insert_gap`, `set_gap_state`, `append_evidence`, `insert_gate`, `set_intent` |
| Blackboard dependency resolution | L3 | `IMPLEMENTED` | `promote_unblocked`, `drain_ready`, `all_closed`, `all_gated_or_closed` — unit tested |
| Blackboard change notifications | L3 | `PLANNED` | `tokio::broadcast` for HUD |
| Compiler | L2 | `PLANNED` | — |
| Executor (script) | L2 | `PLANNED` | — |
| Executor (Micro-Agent) | L2 | `PLANNED` | Requires MCP bridge and Provider tool-calling |
| M1 session context | L1 | `PLANNED — design open` | Cross-board awareness; structure TBD |
| M2 Sled store | L1 | `PLANNED` | `sled` not in Cargo.toml |
| M3 Qdrant integration | L1 | `PLANNED` | `qdrant-client` not in Cargo.toml |
| Provider trait | L0 | `IMPLEMENTED` | Returns `Result<String, ProviderError>` |
| OpenRouter provider | L0 | `IMPLEMENTED` | No `.expect()` — all errors through `ProviderError` |
| Provider error handling | L0 | `IMPLEMENTED` | `thiserror` in Cargo.toml; `MossError` + `ProviderError` defined |
| Provider streaming | L0 | `PLANNED` | — |
| Provider tool calling | L0 | `PLANNED` | `complete_with_tools` stub returns `Err(ProviderError::NotSupported)` |
| MCP bridge | L0 | `PLANNED` | — |
| DefenseClaw | L0 | `PLANNED` | — |

**Recommended implementation order** (each phase is independently testable):

1. ~~**Error handling foundation.**~~ ✅ Done — `thiserror`, `MossError`, `ProviderError`, all `.expect()` removed.
2. ~~**Blackboard.**~~ ✅ Done — All data structures, `drain_ready`, `promote_unblocked`, `all_closed`, unit tested.
3. ~~**Orchestrator decompose + synthesize.**~~ ✅ Done — `minijinja` templates, LLM call, JSON parse, gap insertion in `Moss::run`.
4. **Compiler.** Load `prompts/compiler.md`, call Provider, parse response into `Artifact`. Testable with LocalMock.
5. **Executor (script path).** Subprocess runner with timeout. Testable with hand-written Python scripts.
6. **Runner (execution loop).** Wire Orchestrator → Blackboard → Compiler → Executor → Evidence. First real end-to-end test.
7. **DefenseClaw.** Static scan pipeline. Middleware slot in the execution loop between Compiler and Executor.
8. **MCP bridge.** Connect to at least one MCP server (filesystem). Enables real-world test scenarios.
9. **Memory (M1).** Session context layer. Enables cross-board awareness after topic changes.
10. **Memory (M2/M3).** Sled + Qdrant. Enables cross-session learning.
11. **HUD.** Blackboard change notifications + delta streaming. Polish layer.
