# Moss AIOS — Architecture Specification

**Version:** 0.4.0-draft
**Date:** 2026-04-03
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

1. **Session-scoped reasoning.** Each session starts with a clean Blackboard. Reasoning is never cluttered by stale context from prior sessions.
2. **Code as the universal solver.** Every Gap is resolved by generating and executing code (a deterministic script or a reactive agent loop), not by prompting the LLM to "think harder."
3. **Failure containment.** A failing Gap does not corrupt the global Blackboard. Reactive tasks run inside encapsulated Micro-Agent instances running an isolated ReAct loop.
4. **Concurrency by default.** Independent Gaps execute in parallel via `tokio::JoinSet`. The DAG structure — not a global lock — determines ordering.
5. **Defense in depth.** All generated artifacts pass through a security scanner (DefenseClaw) before execution.

---

## 2. System Layers

```
L5  Interface          CLI daemon, HUD delta streamer
L4  Orchestrator       Intent decomposition, DAG management, response synthesis
L3  Blackboard         Shared per-session state: Gaps, Evidence, Gates
L2  Compiler/Executor  Gap-to-artifact compilation, sandboxed execution
L1  Memory             Session ring buffer (M1), local DB (M2), vector store (M3)
L0  Infrastructure     LLM providers, MCP bridge, DefenseClaw scanner
```

### Layer responsibilities

**L5 — Interface** `PARTIAL`
The user-facing surface. Currently a minimal async CLI (`tokio::io::BufReader` over stdin). The planned HUD component will stream Blackboard deltas (Gap state transitions, Evidence arrivals) to the terminal in real time.

| Sub-component | Status | Notes |
|---|---|---|
| CLI input loop | `PARTIAL` | `main.rs` — reads lines, passes to Orchestrator. Response output is commented out (`// println!`); loop runs but does not display results to the user. |
| HUD delta streamer | `PLANNED` | Requires Blackboard change notification (see Section 5.2) |

**L4 — Orchestrator** `PARTIAL`
The strategic coordinator. Receives user intent, queries Memory for relevant context, and decomposes intent into a Gap DAG via an LLM call. Once decomposition is complete, it hands the plan to the **Runner** (L2) to drive execution. When all Gaps are Closed, the Orchestrator synthesizes the final response from all Evidence.

| Sub-component | Status | Notes |
|---|---|---|
| Intent-to-DAG decomposition (single LLM call) | `PARTIAL` | `orchestrator.rs` — the `synthesize` method sends the query + Blackboard state to the LLM and parses the JSON response, but there is no separate `decompose` method yet. The result is written to `output.json` but not fed back into the Blackboard. |
| Execution loop (poll, dispatch, evidence, synthesis) | `PLANNED` | Core runtime — see Section 3 |
| Context injection (M1/M3 retrieval before planning) | `PLANNED` | — |

**L3 — Blackboard** `PARTIAL`
Per-session shared memory using `DashMap` for lock-free concurrent access. Holds the intent, the Gap DAG, accumulated Evidence, and human-in-the-loop Gates.

| Sub-component | Status | Notes |
|---|---|---|
| Data structures (Gap, Evidence, Blackboard) | `IMPLEMENTED` | `blackboard.rs` |
| Insert/mutate operations | `IMPLEMENTED` | `insert_gap`, `set_gap_state`, `insert_evidence` |
| Dependency resolution (auto-unblock) | `PLANNED` | Scheduler needs to watch for resolved deps |
| Change notification (for HUD) | `PLANNED` | `tokio::broadcast` or watch channel |

**L2 — Compiler & Executor** `PLANNED`
The Compiler takes a Gap description and emits an executable artifact. The Executor runs it in a sandboxed environment and posts Evidence back to the Blackboard.

| Sub-component | Status | Notes |
|---|---|---|
| Compiler (LLM call using `compiler.xml`) | `PLANNED` | Prompt template exists but is not called from Rust |
| Executor — script runner (Python subprocess) | `PLANNED` | — |
| Executor — Micro-Agent host (ReAct loop) | `PLANNED` | — |
| Sandbox / isolation | `PLANNED` | — |

**L1 — Memory** `PLANNED`
Three-tier memory hierarchy for context across and within sessions.

| Tier | Store | Purpose | Status |
|---|---|---|---|
| M1 | In-process ring buffer | Current session context (recent messages, recent Evidence) | `PLANNED` |
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

This is the central execution flow that ties L4, L3, and L2 together. It does not exist in code yet (`PLANNED`) and is the highest-priority implementation target.

### 3.1 Sequence

```
User input
    |
    v
[1] Orchestrator receives intent
    |
    v
[2] Context retrieval (M1 session buffer + M3 semantic search)
    |
    v
[3] LLM call: intent + context + blackboard state  -->  Gap DAG (JSON)
    |
    v
[4] Parse Gap DAG, insert Gaps into Blackboard
    |    - Gaps with no dependencies start as Ready
    |    - Gaps with dependencies start as Blocked
    |
    v
[5] EXECUTION LOOP (runs until all Gaps are Closed or a fatal error):
    |
    |   5a. Poll Blackboard for all Ready Gaps
    |   5b. For each Ready Gap, spawn into tokio::JoinSet:
    |       5b-i.   Mark Gap as Assigned
    |       5b-ii.  Send Gap description to Compiler (LLM call)
    |       5b-iii. Compiler returns artifact (Script or AgentSpec)
    |       5b-iv.  DefenseClaw scans artifact
    |       5b-v.   If DefenseClaw flags high-risk action: transition Gap to Gated,
    |               post Gate to Blackboard, skip execution — await user approval
    |       5b-vi.  Executor runs artifact
    |       5b-vii. Executor posts Evidence to Blackboard
    |       5b-viii.Mark Gap as Closed
    |   5c. After each JoinSet completion:
    |       - Check if any Blocked Gaps have all deps satisfied -> promote to Ready
    |       - Check terminal condition:
    |           all Gaps Closed                  -> done, proceed to synthesis
    |           all remaining Gaps Gated/Closed  -> yield to user (print pending Gates)
    |           otherwise deadlock
    |   5d. If terminal: break
    |
    v
[6] Response synthesis: Orchestrator reads all Evidence, makes final LLM call
    |
    v
[7] Crystallization: compress session outcomes into Knowledge Crystal -> M3
    |
    v
[8] Return response to L5
```

### 3.2 Pseudocode (Rust-flavored)

The three participants — `Orchestrator`, `Runner`, and `Blackboard` — each own their slice of the pipeline.

```rust
// Entry point in main.rs
async fn handle_query(
    query: &str,
    orchestrator: &Orchestrator,
    runner: &Runner,
    blackboard: Arc<Blackboard>,
    memory: &MemoryManager,
) -> Result<String> {
    // [2] Context retrieval
    let session_ctx = memory.m1_recent(query);
    let crystals = memory.m3_search(query, 5).await?;

    // [3] Decompose intent into Gap DAG (Orchestrator's only job here)
    let plan = orchestrator.decompose(query, &session_ctx, &crystals, &blackboard).await?;

    // [5] Execute all Gaps to completion (Runner's job)
    runner.execute(plan, blackboard.clone()).await?;

    // [6] Synthesize final response from closed Evidence (Orchestrator again)
    let response = orchestrator.synthesize(&blackboard).await?;

    // [7] Crystallize session outcomes into M3
    memory.crystallize(&blackboard).await?;

    Ok(response)
}

// Inside Runner::execute
pub async fn execute(&self, plan: Plan, blackboard: Arc<Blackboard>) -> Result<()> {
    for gap in plan.gaps {
        blackboard.insert_gap(gap);
    }
    blackboard.promote_unblocked();

    let mut join_set = JoinSet::new();
    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_GAPS));

    loop {
        // 5a. Spawn all Ready gaps
        for gap_id in blackboard.drain_ready() {
            let permit = semaphore.clone().acquire_owned().await?;
            let bb = blackboard.clone();
            let compiler = self.compiler.clone();
            let executor = self.executor.clone();
            let defense_claw = self.defense_claw.clone();

            join_set.spawn(async move {
                let _permit = permit;
                let gap = bb.get_gap(&gap_id).expect("gap vanished");
                bb.set_gap_state(&gap_id, GapState::Assigned);

                let prior = bb.get_evidence(&gap_id); // &[Evidence], may be empty
                let artifact = compiler.compile(&gap, &prior).await?;

                match defense_claw.scan(&artifact, &gap.constraints) {
                    ScanVerdict::Approved => {}
                    ScanVerdict::Gated { reason } => {
                        bb.set_gap_state(&gap_id, GapState::Gated);
                        bb.insert_gate(gap_id, artifact, reason);
                        return Ok(gap_id);
                    }
                    ScanVerdict::Rejected { reason } => {
                        return Err(MossError::DefenseRejection { reason });
                    }
                }

                let evidence = executor.run(gap_id, artifact).await?;
                bb.append_evidence(evidence);
                bb.set_gap_state(&gap_id, GapState::Closed);
                Ok::<_, MossError>(gap_id)
            });
        }

        // 5c. Await any completion, then re-evaluate
        match join_set.join_next().await {
            Some(Ok(Ok(_gap_id))) => {
                blackboard.promote_unblocked();
            }
            Some(Ok(Err(e))) => {
                // Gap-level failure: log, mark terminal, propagate to dependents
                eprintln!("gap error: {e}");
            }
            Some(Err(join_err)) => {
                eprintln!("task panic: {join_err}");
            }
            None => {
                if blackboard.all_closed() {
                    break;
                }
                if blackboard.all_gated_or_closed() {
                    cli::print_pending_gates(&blackboard);
                    break;
                }
                return Err(MossError::Deadlock);
            }
        }
    }

    Ok(())
}
```

### 3.3 Concurrency constraints

- **Fan-out limit.** A `tokio::Semaphore` caps the number of concurrently executing Gaps (default: 4). This bounds LLM call parallelism and subprocess count.
- **No mutable aliasing.** The `Blackboard` is behind `Arc` and uses `DashMap` internally, so concurrent readers/writers do not require a mutex. Gap state transitions are atomic per-entry.
- **Deadlock detection.** If the JoinSet drains to empty but Blocked gaps remain, the loop returns a `Deadlock` error rather than hanging. This can happen if the Orchestrator produces a DAG with a cycle or an unresolvable dependency.

---

## 4. Component Specifications

### 4.1 Orchestrator `PARTIAL`

**Responsibility:** Translate user intent into a Gap DAG; drive the execution loop; synthesize the final response.

**Current state:** A single method `synthesize` exists in `orchestrator.rs`. It loads the `orchestrator.xml` prompt, substitutes `{user_query}` and `{blackboard_state}` via `str::replace`, sends one LLM call, parses the JSON response, and writes it to `output.json`. It does not insert Gaps into the Blackboard, does not run an execution loop, and does not produce a user-facing response (the method returns a `serde_json::Value` that is currently unused in `main.rs`). There is no separate `decompose` or `synthesize` method — the current code conflates both into one function.

**Current interface (as-built):**

```rust
pub struct Orchestrator {
    provider: DynProvider, // Box<dyn Provider>, not Arc
}

impl Orchestrator {
    pub fn new(provider: DynProvider) -> Self;
    pub async fn synthesize(&self, query: &str, blackboard: &Blackboard) -> Value;
}
```

**Target interface (to be implemented):**

```rust
/// Plans only. Translates intent into a Gap DAG and synthesizes the final response.
/// Does not own or drive the execution loop.
pub struct Orchestrator {
    provider: Arc<dyn Provider>,
    memory: Arc<MemoryManager>,
}

impl Orchestrator {
    /// Step 3 only: intent -> Gap DAG
    async fn decompose(
        &self,
        query: &str,
        session_ctx: &[Message],
        crystals: &[Crystal],
        blackboard: &Blackboard,
    ) -> Result<Plan>;

    /// Step 6 only: all Evidence -> final answer.
    /// Only reads the latest Evidence record per gap. For gaps with
    /// EvidenceStatus::Success, that is the canonical result.
    /// For gaps with EvidenceStatus::Partial or Failure, the summary
    /// is included so the synthesizing LLM can acknowledge partial or
    /// failed information rather than silently omitting it.
    async fn synthesize(&self, blackboard: &Blackboard) -> Result<String>;
}

/// Owns the JoinSet execution loop. Drives Gaps from Ready to Closed.
pub struct Runner {
    compiler: Arc<Compiler>,
    executor: Arc<Executor>,
    defense_claw: Arc<DefenseClaw>,
}

impl Runner {
    /// Insert plan into Blackboard, then execute all Gaps to completion.
    pub async fn execute(&self, plan: Plan, blackboard: Arc<Blackboard>) -> Result<()>;
}
```

The entry point in `main.rs` constructs both and calls them in sequence:

```rust
let plan = orchestrator.decompose(query, &ctx, &crystals, &blackboard).await?;
runner.execute(plan, blackboard.clone()).await?;
let response = orchestrator.synthesize(&blackboard).await?;
```

**Prompt contract (`orchestrator.xml`):**
The Orchestrator sends the user query and serialized Blackboard state to the LLM. The LLM returns a JSON object with `intent` (string) and `gaps` (array). Each gap has: `name` (snake_case slug, unique), `question` (the atomic task description), `type` (PROACTIVE or REACTIVE), and `dependencies` (list of gap names from the same plan).

**Constraints the prompt enforces:**
- No meta-tasks (no gaps for "understand the query" or "plan the approach").
- If the Blackboard already contains enough information, return `"gaps": []`.
- If the query is nonsensical, return `"intent": null, "gaps": null`.

**Issue to resolve:** The current prompt uses `{user_query}` and `{blackboard_state}` template placeholders replaced via `str::replace`. This is fragile — a user query containing the literal string `{blackboard_state}` would break rendering. Replace with a proper templating approach (e.g., `minijinja` or structured XML injection with escaping).

### 4.2 Blackboard `PARTIAL`

**Responsibility:** Per-session shared memory. Holds the Gap DAG, Evidence map, and HITL Gates.

**Current state:** Data structures and basic insert/mutate operations are implemented. Missing: dependency resolution, ready-gap polling, change notification, and serialization round-tripping.

**Required additions:**

```rust
impl Blackboard {
    /// Return and atomically mark as Assigned all gaps currently in Ready state.
    pub fn drain_ready(&self) -> Vec<Uuid> { ... }

    /// For every Blocked gap, check if all dependencies are Closed.
    /// If so, promote to Ready. Returns the count of newly promoted gaps.
    /// Uses the name_index to resolve dependency names to UUIDs in O(1).
    pub fn promote_unblocked(&self) -> usize { ... }

    /// True when every gap is in Closed state.
    pub fn all_closed(&self) -> bool { ... }

    /// True when every gap is in Closed or Gated state (no Blocked/Ready/Assigned remain).
    /// Used to detect the "all work paused for human approval" condition
    /// without triggering a false Deadlock error.
    pub fn all_gated_or_closed(&self) -> bool { ... }

    /// Retrieve a gap by ID (clone for send across await).
    pub fn get_gap(&self, id: &Uuid) -> Option<Gap> { ... }

    /// Retrieve a gap UUID by name slug. Used by promote_unblocked and dependency resolution.
    pub fn get_gap_id_by_name(&self, name: &str) -> Option<Uuid> { ... }

    /// Subscribe to state changes (for HUD streaming).
    pub fn subscribe(&self) -> broadcast::Receiver<BlackboardDelta> { ... }
}
```

**Name→UUID reverse index:**
`Gap.dependencies` stores names (`Vec<Box<str>>`), but the gap map is keyed by `Uuid`. A secondary index `name_index: DashMap<Box<str>, Uuid>` is populated atomically in `insert_gap` alongside the primary map. This makes `promote_unblocked` O(D) per gap (D = dependency count) instead of O(N·D) with a scan. The index is append-only — gap names are immutable after insertion.

**Data structures — current vs. target:**

The current `Gap` struct in `blackboard.rs` is minimal and uses a legacy `Pulse` enum (Network/Machine/Other) that is still referenced in the `Gap` constructor but has no role in the target architecture. The struct is missing `name`, `gap_type`, `dependencies`, `constraints`, and `expected_output` — all required for DAG execution. The `Evidence` struct replaces the ambiguous `done: bool` flag with a typed `EvidenceStatus`, and the `evidences` map changes from one-per-gap to an attempt log to support retry history. The target structs are:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gap {
    pub gap_id: Uuid,
    pub name: Box<str>,              // snake_case slug from the plan
    pub state: GapState,
    pub description: Box<str>,       // consumed by the Compiler
    pub gap_type: GapType,           // Proactive or Reactive
    pub dependencies: Vec<Box<str>>, // names of gaps this depends on
    pub constraints: Option<Value>,
    pub expected_output: Option<Box<str>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GapType {
    Proactive,
    Reactive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub gap_id: Uuid,
    pub attempt: u32,            // 1-based attempt number (for retry history)
    pub content: Value,
    pub status: EvidenceStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EvidenceStatus {
    Success,
    Failure { reason: String },
    Partial,                     // Micro-Agent hit iteration cap before goal was met
}
```

The `Blackboard.evidences` field changes from `DashMap<Uuid, Evidence>` to `DashMap<Uuid, Vec<Evidence>>` — an ordered attempt log per gap. The Compiler for retry attempt N receives the `Vec<Evidence>` slice `[0..N-1]` so it can see prior errors and adapt.

The `Blackboard` struct gains a `name_index: DashMap<Box<str>, Uuid>` field for O(1) name-to-ID resolution. It is written once in `insert_gap` and never mutated after that.

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

### 5.1 M1 — Session Ring Buffer

An in-process, bounded ring buffer holding the most recent N messages and Evidence summaries from the current session. Provides the Orchestrator with short-term context for multi-turn conversations within a session.

```rust
pub struct SessionBuffer {
    entries: VecDeque<SessionEntry>,
    capacity: usize, // default: 50 entries
}

pub enum SessionEntry {
    UserMessage { content: String, timestamp: Instant },
    AssistantResponse { content: String, timestamp: Instant },
    EvidenceSummary { gap_name: String, summary: String },
}
```

**Session expiry:** An `Instant` tracks the last interaction. If `Instant::elapsed() > idle_timeout` (default 30 minutes), the Orchestrator clears the buffer and starts a fresh session. The check runs at the start of each new user input, not on a background timer.

### 5.2 M2 — Sled (Local Preferences & Audit)

An embedded key-value store for data that must survive across sessions but does not need semantic search.

Contents: user preferences (default model, concurrency limits, tool permissions), an append-only audit log of all executed artifacts (for security review), and session metadata (start time, gap count, outcome).

**Dependency:** `sled` crate (to be added to `Cargo.toml`).

### 5.3 M3 — Qdrant (Knowledge Crystals)

A vector database for semantic retrieval of compressed past session outcomes.

**Crystallization trigger:** At the end of every session that produced at least one successful Evidence record, the Orchestrator generates a Knowledge Crystal:

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
    async fn complete_chat(&self, messages: Vec<Message>) -> String;
}
```

**Current implementations:**

| Provider | Status | Notes |
|---|---|---|
| OpenRouter | `IMPLEMENTED` | Supports any model available via OpenRouter API |
| LocalMock | `IMPLEMENTED` | Echo-back mock for testing |
| Local vLLM | `PLANNED` | Direct inference on local GPU via vLLM's OpenAI-compatible API |

**Issues to address:**
1. **Error propagation.** `complete_chat` currently returns `String` and panics on failure. It must return `Result<String, ProviderError>` so callers can retry or fail gracefully.
2. **Streaming.** The current trait is request/response only. For the HUD to stream partial responses, add a `complete_chat_stream` method returning a `tokio::sync::mpsc::Receiver<String>` or a `Stream<Item = Result<String>>`.
3. **Tool calling.** The OpenAI-compatible API supports function/tool calling. The trait should be extended with a `complete_with_tools` method that accepts tool definitions and returns either a text response or a tool-call request. This is essential for the Micro-Agent's ReAct loop.

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

```
[User sends first message]
        |
        v
  Create Session (new Uuid, new Blackboard, empty M1 buffer)
        |
        v
  Run Orchestrator.run() --> produce response
        |
        v
  Update M1 buffer with query + response summary
        |
        v
  Wait for next input
        |
    +---+---+
    |       |
    v       v
  Input     Idle > 30 min
  arrives   (checked on next input)
    |       |
    v       v
  Same      Clear Blackboard, clear M1
  session   Start new session
```

**Important:** The idle timeout is not enforced by a background timer. It is checked when the next user input arrives. If 30+ minutes have elapsed since the last interaction, the Blackboard and M1 buffer are cleared, and the input is treated as the start of a new session. This avoids unnecessary background tasks for what is fundamentally a single-user CLI application.

**Crystallization** happens at session end: either when the user explicitly exits, or when an idle timeout triggers a new session. Only sessions with at least one Closed gap with successful Evidence are crystallized.

---

## 10. Gap Lifecycle

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

## 11. Error Handling Strategy `PLANNED`

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

    #[error("session expired")]
    SessionExpired,

    #[error("MCP tool error: {tool} — {reason}")]
    Mcp { tool: String, reason: String },

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
```

**Dependency:** `thiserror` crate (to be added to `Cargo.toml`).

**Policy:** Every function that can fail returns `Result<T, MossError>`. The top-level `main.rs` loop catches errors and prints them to stderr without crashing. Individual Gap failures are isolated — they do not bring down the session.

---

## 12. Architecture Decisions

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

---

## 13. Open Questions

These are unresolved design decisions that need answers before or during implementation.

1. ~~**Re-planning.**~~ **Closed — Decision:** No re-planning in v1. Terminal failure propagates through the DAG downstream. Dependent Gaps are marked Closed with `EvidenceStatus::Failure`. Re-planning is deferred to v2 and requires explicit plan versioning and a `replace_subgraph` API on the Blackboard.

2. **Streaming vs. batch Evidence.** Should the Executor post Evidence incrementally as a script produces output (streaming), or only after the script completes (batch)? Streaming enables the HUD to show progress, but complicates the "done" semantics on Evidence and the dependency resolution logic.

3. **Embedding model for M3.** Which embedding model for Knowledge Crystal vectors? Options: a local model (e.g., `nomic-embed-text` on the RTX 4090), or a remote API (e.g., OpenAI embeddings via OpenRouter). Local keeps it offline; remote is simpler to start with.

4. **Multi-user / multi-session.** The current design is single-user, single-session. If Moss ever serves multiple concurrent sessions (e.g., as a daemon handling multiple terminal windows), the Blackboard needs session-scoped namespacing and the Memory tiers need per-user isolation.

5. ~~**Micro-Agent recursion depth.**~~ **Closed — Decision:** Micro-Agents are flat ReAct loops. They do not instantiate a new Orchestrator, do not have a sub-Blackboard, and do not call the Compiler. Recursion depth is 0. Not applicable.

---

## 14. Implementation Status Matrix

| Component | Layer | Status | Blocking dependencies |
|---|---|---|---|
| CLI input loop | L5 | `PARTIAL` | Output commented out; loop runs but doesn't print responses |
| HUD delta streamer | L5 | `PLANNED` | Blackboard change notifications |
| Orchestrator decompose | L4 | `PARTIAL` | LLM call works but result not fed back into Blackboard |
| Orchestrator execution loop | L4 | `PLANNED` | Compiler, Executor, Blackboard extensions |
| Orchestrator synthesis | L4 | `PLANNED` | Execution loop |
| Blackboard data structures | L3 | `IMPLEMENTED` | — |
| Blackboard dependency resolution | L3 | `PLANNED` | — |
| Blackboard change notifications | L3 | `PLANNED` | — |
| Compiler | L2 | `PLANNED` | Provider error handling |
| Executor (script) | L2 | `PLANNED` | Sandbox design |
| Executor (Micro-Agent) | L2 | `PLANNED` | MCP bridge, Provider tool-calling |
| M1 session buffer | L1 | `PLANNED` | — |
| M2 Sled store | L1 | `PLANNED` | `sled` dependency |
| M3 Qdrant integration | L1 | `PLANNED` | `qdrant-client` dependency |
| Provider trait | L0 | `IMPLEMENTED` | — |
| OpenRouter provider | L0 | `IMPLEMENTED` | — |
| Provider error handling | L0 | `PLANNED` | `thiserror` dependency |
| Provider streaming | L0 | `PLANNED` | — |
| Provider tool calling | L0 | `PLANNED` | — |
| MCP bridge | L0 | `PLANNED` | MCP Rust SDK |
| DefenseClaw | L0 | `PLANNED` | — |

**Recommended implementation order** (each phase is independently testable):

1. **Error handling foundation.** Add `thiserror`, define `MossError`, convert Provider to return `Result`. Touches every file but is mechanical.
2. **Blackboard extensions.** Add `drain_ready`, `promote_unblocked`, `all_closed`. Unit-testable in isolation.
3. **Compiler.** Load `compiler.xml`, call Provider, parse response into `Artifact`. Testable with LocalMock.
4. **Executor (script path).** Subprocess runner with timeout. Testable with hand-written Python scripts.
5. **Execution loop.** Wire Orchestrator -> Blackboard -> Compiler -> Executor -> Evidence. First end-to-end test.
6. **DefenseClaw.** Static scan pipeline. Can be added as a middleware in the execution loop.
7. **MCP bridge.** Connect to at least one MCP server (filesystem). Enables real-world test scenarios.
8. **Memory (M1).** Session buffer. Enables multi-turn conversations.
9. **Memory (M2/M3).** Sled + Qdrant. Enables cross-session learning.
10. **HUD.** Delta streaming. Polish layer.
