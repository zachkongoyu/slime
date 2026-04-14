# Moss AIOS — Architecture Specification

**Version:** 0.9.0
**Date:** 2026-04-09
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
2. **Code as the universal solver.** Every Gap is handled by the Solver — a unified iterative loop where the LLM generates and executes code until the task is complete or a ceiling is reached. No fixed tool menu, no predefined actions. Code is the universal primitive.
3. **Failure containment.** A failing code block within a Solver iteration does not lose context — the error output appears in the next iteration's prompt, enabling automatic recovery. A failing Gap does not corrupt the Blackboard.
4. **Concurrency by default.** Independent Gaps execute in parallel via `tokio::JoinSet`. The DAG structure — not a global lock — determines ordering.
5. **Defense in depth.** All generated artifacts pass through `ArtifactGuard` before execution.

---

## 2. System Layers

```
L5  Interface          CLI daemon, HUD delta streamer
L4  Orchestrator       Intent decomposition, DAG management, drive_gaps, response synthesis
L3  Blackboard         Living workspace: Gaps (append-only), Evidence, mutable intent, HITL approvals
L2  Solver             Unified iterative execution loop: LLM writes code, Solver runs it, iterates until done
L1  Memory             Session context (M1), local DB (M2), vector store (M3)
L0  Infrastructure     LLM providers, MCP bridge, ArtifactGuard scanner
```

### Layer responsibilities

**L5 — Interface** `PARTIAL`
The user-facing surface. `Cli` in `src/cli.rs` drives stdin and, while a query is running, multiplexes solver events from the signal bus. The planned HUD component would subscribe to the same bus for real-time delta streaming.

| Sub-component | Status | Notes |
|---|---|---|
| CLI input loop | `IMPLEMENTED` | `src/cli.rs` — `Cli` struct, async stdin reader, calls `Moss::run`, prints response. |
| CLI signal handling | `PARTIAL` | `src/cli.rs` — handles `ApprovalRequested` inline and calls `Moss::approve()`. `QuestionAsked` exists on the signal bus but is not fully wired through the CLI yet. |
| HUD delta streamer | `PLANNED` | Another signal bus consumer. Phase 11. |

**L4 — Orchestrator** `IMPLEMENTED`
The strategic coordinator. `Orchestrator::run` is the single entry point: decompose → insert Gaps → `drive_gaps` (private JoinSet loop) → synthesize. Owns the current `Arc<Blackboard>` and the broadcast sender.

| Sub-component | Status | Notes |
|---|---|---|
| Intent-to-DAG decomposition (single LLM call) | `IMPLEMENTED` | `orchestrator.rs` — `decompose` renders `prompts/decompose.md` via `minijinja`, calls LLM, deserializes into `Decomposition` (includes `is_follow_up` flag). |
| Response synthesis (Evidence → answer) | `IMPLEMENTED` | `orchestrator.rs` — `synthesize` renders `prompts/synthesize.md`, passes real evidence from `blackboard.all_evidence()`. |
| Execution loop (poll, dispatch, evidence, synthesis) | `IMPLEMENTED` | `orchestrator.rs` — `Orchestrator::drive_gaps()` private method. `runner.rs` dissolved. JoinSet dispatch + deadlock detection. No semaphore fan-out cap yet. |
| Context injection (M1/M3 retrieval before planning) | `PLANNED` | — |

**L3 — Blackboard** `IMPLEMENTED`
Living workspace using `DashMap` for lock-free concurrent access. Holds the intent (mutable), the Gap DAG (append-only), accumulated Evidence, and human-in-the-loop Gates. A Blackboard stays open across follow-up messages — new Gaps are inserted and the intent is refined on each decompose call. It is sealed only when the topic changes or the session ends (see Section 9).

| Sub-component | Status | Notes |
|---|---|---|
| Data structures (Gap, Evidence, Blackboard) | `IMPLEMENTED` | `blackboard.rs` — `GapState`, `Gap`, `EvidenceStatus`, `Evidence`, `Blackboard` with private fields and `pub(crate)` getters |
| Insert/mutate operations | `IMPLEMENTED` | `insert_gap`, `set_gap_state`, `append_evidence`, `set_intent`, `register_approval`, `approve`, `register_question`, `answer_question` |
| Dependency resolution (auto-unblock) | `IMPLEMENTED` | `promote_unblocked`, `drain_ready`, `all_closed` — unit tested. `all_gated_or_closed` removed (ADR-007). |
| Signal Bus integration | `IMPLEMENTED` | Every `Blackboard` mutation emits `Event::Snapshot` via broadcast. The solver emits `ApprovalRequested` and `QuestionAsked`. `register_approval()`/`approve()` and `register_question()`/`answer_question()` manage the `oneshot` pairs. |

**L2 — Solver** `IMPLEMENTED`
The Solver is the active execution component used by `Orchestrator::drive_gaps`. For every Gap, it renders `solver.md` with the gap context, current working memory, the detected local environment, dependency evidence, and the last execution output; calls the LLM; parses a JSON step (`Code`, `Ask`, `Done`); and iterates until completion or a fixed ceiling (`MAX_ITERATIONS = 10`). Legacy `compiler.rs` and `executor.rs` files still exist in the tree, but they are no longer on the active runtime path.

| Sub-component | Status | Notes |
|---|---|---|
| Solver loop + step parser | `IMPLEMENTED` | `solver.rs` — struct, loop, JSON `Code`/`Ask`/`Done` parser, optional `scratch` extraction |
| Solver prompt | `IMPLEMENTED` | `prompts/solver.md` — fixed frame with environment, dependency evidence, working memory, and last output |
| Sandbox / isolation | `PARTIAL` | Executes via subprocess + timeout + temp file. Restricted env / cgroups / ulimit are not implemented yet. |

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
| ArtifactGuard (pre-exec scanner) | `IMPLEMENTED` | `artifact_guard.rs` — zero-field struct, string-pattern static analysis + size limit + HITL patterns. See Section 8. |

---

## 3. Core Runtime Loop

This is the central execution flow that ties L4, L3, and L2 together. It is called once per user message. The same Blackboard may pass through this loop many times across follow-up messages.

`IMPLEMENTED` — `Moss::run` → `Orchestrator::run` → `decompose` → gap insertion → `drive_gaps` (Solver per gap, including approval/question round-trips) → `synthesize`. Legacy Compiler/Executor files remain in the repo but are not the active path.

### 3.1 Sequence

```
User input
    |
    v
[1] Moss: is there an active Blackboard?
    |
    YES                             NO
    |                               |
    v                               v
[2] Serialize board state       Create new Blackboard
    (blackboard.snapshot())
    |                               |
    +---------------+---------------+
                    |
                    v
[3] Orchestrator.decompose(query, blackboard)
    LLM returns: { intent, is_follow_up, gaps[] }
                    |
                    v
[4] Orchestrator: is_follow_up? (from Decomposition struct)
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
[6] EXECUTION LOOP (Orchestrator::drive_gaps — persistent JoinSet, one completion per iteration):
    |
    |   6a. promote_unblocked() — check Blocked Gaps
    |   6b. drain_ready() — poll Blackboard for all Ready Gaps
    |   6c. For each Ready Gap, spawn into tokio::JoinSet:
    |       6c-i.    Mark Gap as Assigned
    |       6c-ii.   Solver::run(gap, blackboard) — unified bounded loop:
    |                  render solver.md (environment + dependency evidence + working memory + last output)
    |                  provider.complete_chat
    |                  parse JSON step (Code | Ask | Done)
    |                  Code → ArtifactGuard.scan_code → execute subprocess → append stdout/stderr → loop
    |                  Ask  → register_question (fires QuestionAsked broadcast)
    |                         await oneshot → append answer → loop
    |                         (gap task stays alive on JoinSet; other gaps proceed)
    |                  Done → post Evidence to Blackboard, exit loop
    |                  If scan verdict is Gated: register_approval / await / continue
    |                  If scan verdict is Rejected: append error to last_output and continue
    |       6c-iii.  Gap → Closed
    |   6d. If JoinSet is empty:
    |       - all Gaps Closed → done
    |       - else → deadlock
    |   6e. Wait for ONE task to complete (join_next), then loop back to 6a
    |
    |   Note: Gated gaps stay alive on the JoinSet awaiting human I/O.
    |   Other gaps complete, promote dependents, and get dispatched
    |   without waiting for the human. drive_gaps has no gate-specific logic.
    |
    v
[7] Orchestrator.synthesize() — reads latest intent + all Evidence
    |
    v
[8] Return response to L5. Blackboard enters Idle state.
    (Board stays in memory — ready for follow-up on next input)
```

### 3.2 Concurrency constraints

- **Fan-out limit.** No `tokio::Semaphore` cap exists in the current implementation. `drive_gaps` spawns every currently ready gap into the `JoinSet`.
- **No mutable aliasing.** The `Blackboard` is behind `Arc` and uses `DashMap` internally, so concurrent readers/writers do not require a mutex. Gap state transitions are atomic per-entry.
- **Deadlock detection.** If the JoinSet drains to empty but Blocked gaps remain, the loop returns a `Deadlock` error rather than hanging. This can happen if the Orchestrator produces a DAG with a cycle or an unresolvable dependency.

---

## 4. Component Specifications

### 4.1 Orchestrator `IMPLEMENTED`

**Responsibility:** Translate user intent into new Gaps; refine the Blackboard's intent on follow-ups; synthesize the final response from Evidence.

**Current state:** Full execution pipeline implemented. `Orchestrator::run` is the single entry point: it calls `decompose`, inserts Gaps, calls `drive_gaps` (private JoinSet execution loop with ArtifactGuard + HITL), and calls `synthesize`. The Orchestrator owns a `Mutex<Arc<Blackboard>>` for follow-up tracking and a `broadcast::Sender` for HITL signals. `Decomposition::is_follow_up` flag drives board reuse vs. fresh creation.

**Current interface (as-built):**

```rust
pub(crate) struct Orchestrator {
    provider: Arc<dyn Provider>,
    guard: Arc<ArtifactGuard>,
    environment: String,
    blackboard: Mutex<Arc<Blackboard>>,
    tx: broadcast::Sender<signal::Payload>,
}

impl Orchestrator {
    pub(crate) fn new(provider: Arc<dyn Provider>, tx: broadcast::Sender<signal::Payload>) -> Self;
    pub(crate) fn approve(&self, gap_id: Uuid, approved: bool);
    pub(crate) fn answer(&self, gap_id: Uuid, answer: String);
    pub(crate) async fn run(&self, query: &str) -> Result<String, MossError>;
    // private:
    async fn drive_gaps(&self, blackboard: Arc<Blackboard>) -> Result<(), MossError>;
    pub(crate) async fn decompose(&self, query: &str, blackboard: &Blackboard) -> Result<Decomposition, MossError>;
    pub(crate) async fn synthesize(&self, blackboard: &Blackboard) -> Result<String, MossError>;
}
```

**Decompose interface:** `decompose` receives `&Blackboard` directly and calls `blackboard.snapshot()` to serialize the current board state into the planning prompt. The `Decomposition` response includes `is_follow_up: bool` which the Orchestrator uses to decide whether to reuse the current board or create a fresh one.

**Decompose output contract:**

The LLM always returns `{ intent, is_follow_up, gaps[] }`. `is_follow_up` is the Orchestrator's decision on whether to extend the current board.

- `intent` — the current goal of the Blackboard. On the first message this is the original intent. On follow-ups the Orchestrator refines it to capture the evolved scope (e.g., "Book a flight to Tokyo" → "Book a business class flight to Tokyo"). Always present.
- `is_follow_up` — `true` if this query extends the current Blackboard; `false` if it starts a new topic. Decided by the LLM in the decompose call.
- `gaps[]` — only the **new** Gaps needed for this query. On a follow-up, these may declare dependencies on existing Closed Gaps by name. On a new topic, these will have no references to the current board.

```json
{
    "intent": "optional string — the current/updated goal",
  "is_follow_up": true,
  "gaps": [
    {
      "name": "snake_case_identifier (unique across board lifetime)",
      "description": "what this gap resolves",
      "dependencies": ["may reference existing Closed gaps or new gaps"],
      "constraints": null,
      "expected_output": "what a correct result looks like"
    }
  ]
}
```

**Rich Blackboard state (input to decompose):**

`Blackboard::snapshot()` serializes the board into a `BlackboardSnapshot` struct (intent + full Gap map + full Evidence map) that is rendered into the planning prompt via minijinja:

```json
{
  "intent": "Book a flight to Tokyo",
    "gaps": {
        "<uuid>": {
            "name": "search_flights",
            "state": "Closed",
            "description": "Search for available flights to Tokyo",
            "dependencies": []
        }
    },
    "evidences": {
        "<uuid>": [
            {
                "content": { "result": "ok" },
                "status": "Success"
            }
        ]
    }
}
```

For the first message in a session (no board exists), this is `{}`.

**Prompt contract:**
- `prompts/decompose.md` — Markdown instructions + XML-tagged input (`{{ user_query }}`, `{{ blackboard_state }}`). The prompt instructs the LLM to refine the intent on follow-ups, return only new Gaps, and avoid reusing names already on the board. See Section 9 for the full lifecycle.
- `prompts/synthesize.md` — Markdown instructions + XML-tagged input (`{{ intent }}`, `{{ evidence }}`). The `intent` is always the latest (refined) version. LLM returns a plain text response.

### 4.2 Blackboard `IMPLEMENTED`

**Responsibility:** Living workspace for the current conversation thread. Holds the intent (mutable), the Gap DAG (append-only), Evidence map, and HITL Gates. A Blackboard stays open across follow-up messages — the Orchestrator inserts new Gaps and updates the intent on each decompose call. It is sealed only when the topic changes or the session ends (see Section 9).

**Current state:** Core data structures, insert/mutate operations, dependency resolution, ready-gap polling, signal bus integration, and HITL approval flow are implemented and unit tested.

**Implemented interface:**

```rust
impl Blackboard {
    /// Return and atomically mark as Assigned all gaps currently in Ready state.
    pub(crate) fn drain_ready(&self) -> Vec<Gap>;

    /// For every Blocked gap whose dependencies are all Closed, promote to Ready.
    pub(crate) fn promote_unblocked(&self);

    /// True when every gap is in Closed state. This is the execution loop's only terminal condition.
    pub(crate) fn all_closed(&self) -> bool;

    /// Retrieve a gap by ID (cloned for send across await).
    pub(crate) fn get_gap(&self, id: &Uuid) -> Option<Gap>;

    /// Retrieve a gap UUID by name slug. Used by promote_unblocked and dependency resolution.
    pub(crate) fn get_gap_id_by_name(&self, name: &str) -> Option<Uuid>;

    /// Store the sender half of a HITL oneshot channel.
    /// The Orchestrator emits ApprovalRequested then awaits the receiver side.
    pub(crate) fn register_approval(&self, gap_id: Uuid, sender: oneshot::Sender<bool>);

    /// Resolve a pending approval. Called by Moss::approve() from the CLI.
    pub(crate) fn approve(&self, gap_id: Uuid, approved: bool);

    /// Store the sender half of a human question oneshot channel.
    pub(crate) fn register_question(&self, gap_id: Uuid, sender: oneshot::Sender<String>);

    /// Resolve a pending human answer. Called by Moss::answer().
    pub(crate) fn answer_question(&self, gap_id: Uuid, answer: String);

    /// Access the broadcast sender to emit events from outside the Blackboard.
    pub(crate) fn signal_tx(&self) -> &broadcast::Sender<signal::Payload>;
}
```

All Blackboard mutation methods (`set_gap_state`, `insert_gap`, `append_evidence`, `set_intent`) emit `Event::Snapshot` via the broadcast channel after each write. Consumers (CLI, HUD, logger) subscribe independently — the Blackboard doesn't know or care who's listening.

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
    description: Box<str>,       // consumed by the Solver
    dependencies: Vec<Box<str>>, // names of gaps this depends on
    constraints: Option<Value>,
    expected_output: Option<Box<str>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Evidence {
    gap_id: Uuid,
    content: Value,
    status: EvidenceStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum EvidenceStatus {
    Success,
    Failure { reason: String },
}
```

`Blackboard.evidences` is `DashMap<Uuid, Vec<Evidence>>` — an ordered evidence log per gap. The active solver path currently gathers successful dependency evidence for prompt context; it does not yet feed a per-gap retry history back into the same gap's prompt.

`Blackboard` includes `name_index: DashMap<Box<str>, Uuid>` for O(1) name-to-ID resolution. Written once in `insert_gap`, never mutated after that. `intent` is stored as `Mutex<Option<Box<str>>>` for safe mutation through a shared `&self` reference. `pending_approvals: DashMap<Uuid, oneshot::Sender<bool>>` stores approval waiters, and `pending_questions: DashMap<Uuid, oneshot::Sender<String>>` stores question waiters.

**Thread-safety model:**
`DashMap` provides per-shard read/write locks internally. Individual Gap state transitions (Ready -> Assigned) must be atomic. Use `DashMap::get_mut` which holds a write lock on the shard for the duration of the returned `RefMut`. The `drain_ready` method should iterate and CAS (compare-and-swap) in a single pass to avoid TOCTOU races where two threads both see the same gap as Ready.

### 4.3 Solver `IMPLEMENTED`

**Responsibility:** Execute a single Gap from the Blackboard to a final Evidence record. Replaces the former Compiler + Executor + Artifact split (ADR-009). The Solver owns the complete execution lifecycle for one Gap: it renders the prompt, calls the LLM, parses the step, acts, and iterates until the LLM emits `Done` or the iteration ceiling is reached.

**Lifecycle:** The Orchestrator does not hold a Solver as a field. `drive_gaps` constructs one Solver per ready Gap and spawns it into the JoinSet. A Solver lives for exactly one Gap and is dropped when the loop exits. The Orchestrator's `provider` and `guard` are cloned (`Arc`) into each Solver.

**Interface:**

```rust
pub(crate) struct Solver {
    provider: Arc<dyn Provider>,
    guard: Arc<ArtifactGuard>,
    environment: String,
}

impl Solver {
    pub(crate) fn new(provider: Arc<dyn Provider>, guard: Arc<ArtifactGuard>, environment: String) -> Self;

    /// Run the unified execution loop for `gap`.
    /// Posts Evidence to `blackboard` and marks the Gap Closed on exit.
    /// The current implementation uses a fixed `MAX_ITERATIONS` ceiling.
    pub(crate) async fn run(&self, gap: &Gap, blackboard: &Blackboard) -> Result<(), MossError>;
}
```

**Step model — exactly three variants:**

| Step | Parser match | Loop behavior |
|------|-------------|---------------|
| `Code` | JSON object with `{"step":"code","interpreter","ext","code"}` | `guard.scan_code` → execute subprocess → append stdout/stderr to next prompt → loop |
| `Ask`  | JSON object with `{"step":"ask","question"}` | `blackboard.register_question(question)` → await oneshot → append answer → loop |
| `Done` | JSON object with `{"step":"done","value"}` | post success Evidence using `value`, mark Gap Closed, return |

An optional `scratch` string field appended to any JSON response is appended to `working_memory` for the next iteration.

**Prompt contract (`solver.md`):**
Three slots, fixed frame, no LLM mutation allowed:
1. **Fixed frame** — environment description, output contract, parser rules (never changes).
2. **Working memory** — owned by the LLM, rewritten by the `scratch` side-channel. Carries strategy, progress, remaining steps.
3. **Last execution output** — only the most recent Code step's stdout/stderr. Prior outputs drop from context.

**Execution model:**
1. Render `solver.md` template with gap context, current `working_memory`, and `last_output`.
2. Call `provider.complete_chat`.
3. Parse response into `(step, Option<new_working_memory>)`.
4. If `Code`: `guard.scan_code(code)` → on `Approved`, write to `NamedTempFile`, spawn `tokio::process::Command`, capture stdout/stderr, update `last_output`. On `Gated`, emit `ApprovalRequested`, await human approval, then continue.
5. If `Ask`: emit `QuestionAsked`, await human response, update `last_output`.
6. If `Done`: serialize evidence from the `done` value (or last stdout), call `blackboard.append_evidence`, return.
7. On iteration ceiling: post `EvidenceStatus::Failure` and return.

**Design note:** The Solver receives only the specific Gap description and dependency Evidence — not the full Blackboard. Principle of least privilege is preserved.

### 4.5 DAG Scheduler

The scheduler is not a separate component — it is `Orchestrator::drive_gaps` (Section 3). `runner.rs` was dissolved into the Orchestrator (Phase 7). This is a deliberate simplification: an external scheduler would add an inter-component communication layer without clear benefit at this scale.

**Scheduling strategy:** Non-preemptive, event-driven. Gaps are not assigned on a timer; they are spawned into the JoinSet when they become Ready. When a gap completes and posts Evidence, the `promote_unblocked` sweep runs synchronously before the next iteration, ensuring newly-unblocked gaps are immediately eligible.

**Failure policy:**

| Failure type | Behavior |
|---|---|
| Code block exits non-zero (within Solver) | The stdout/stderr payload is appended to the next iteration's context. The Solver loop continues and the LLM can adapt. |
| Guard rejects code | The rejection reason is appended to `last_output`, and the Solver continues with another iteration. |
| Solver hits iteration ceiling | Append `EvidenceStatus::Failure { reason }` for the gap. |
| LLM provider error (rate limit, timeout) | Propagates as `MossError::Provider`; retry/backoff is not implemented in the current runtime path. |
| Deadlock (Blocked gaps remain, JoinSet empty) | Return `MossError::Deadlock`. |

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

**Crystallization trigger:** When a Blackboard is sealed (topic change or session end) and contains at least one Closed Gap with `EvidenceStatus::Success`, the system generates a Knowledge Crystal:

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
    ) -> Result<String, ProviderError> {
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
- **Tool calling.** `PLANNED` — `complete_with_tools` stub exists; not required for the Solver (which generates code directly rather than selecting from a tool menu). Reserved for future MCP integration if that phase is ever pursued.

---

## 7. MCP Integration `DEFERRED`

MCP (Model Context Protocol) is the standardized bridge between the LLM and external tools (filesystem, browser, APIs, databases).

Deferred beyond Phase 8. Per ADR-009, the Solver's code-generation approach makes MCP unnecessary for the core execution loop — the LLM writes `requests.get(...)` directly rather than picking from a fixed tool menu. MCP integration, if ever needed, becomes a future capability layer that the Solver's generated code calls into, scoped per-gap for least privilege.

---

## 8. Security: ArtifactGuard `IMPLEMENTED`

`ArtifactGuard` (`src/moss/artifact_guard.rs`) is the pre-execution scanner that inspects every code block before the Solver runs it. It is a zero-field unit struct with all policy encoded as constants. The conceptual security layer is still called "DefenseClaw" in design discussions; the struct name in code is `ArtifactGuard`. It operates as a pipeline of checks, any of which can reject the block.

**Scan pipeline:**

| Stage | What it checks | Method |
|---|---|---|
| 1. Static analysis | Forbidden imports / shell patterns (`import os`, `subprocess`, `curl`, `rm -rf`, etc.) | String-pattern matching |
| 2. Resource bounds | Is the script too large? | `MAX_SCRIPT_SIZE` check |
| 3. HITL gate | Is this a high-risk action (e.g., sending email, deleting files, making purchases)? | Pattern match against `HITL_PATTERNS` const (pattern + category tuples) |

**Interface:**

```rust
/// Zero-field unit struct — all policy encoded as constants.
pub(crate) struct ArtifactGuard;

const MAX_SCRIPT_SIZE: usize = 65_536;

/// Pattern + category pairs for HITL gating. Category surfaces in the approval prompt.
/// Examples: ("> /dev/tcp", "network exfil"), ("stripe.charge(", "financial")
const HITL_PATTERNS: &[(&str, &str)] = &[ /* see artifact_guard.rs */ ];

/// A single scan pass produces one of three verdicts — never ambiguous.
pub(crate) enum ScanVerdict {
    /// Artifact is clean. Proceed to execution.
    Approved,
    /// High-risk action detected. Pause Gap, surface approval request to user.
    Gated   { reason: Box<str> },
    /// Hard violation (forbidden import, oversized script, etc.). Do not execute.
    Rejected { reason: Box<str> },
}

impl ArtifactGuard {
    pub(crate) fn new() -> ArtifactGuard;
    /// Run all 4 stages in one pass and return a single verdict.
    /// Callers dispatch on the variant — no two-method TOCTOU window.
    pub(crate) fn scan_code(&self, code: &str) -> ScanVerdict;
}
```

**Non-goals:** ArtifactGuard is not a sandbox. It is a static pre-flight check per code block. Runtime isolation is the Solver's subprocess responsibility (restricted env, cgroups, etc.). Defense in depth means both layers exist.

---

## 9. Session & Blackboard Lifecycle

A **Session** is the lifetime of the running Moss process. It holds at most one active Blackboard at any time. A Blackboard is a **workspace**, not a transaction — it stays open across follow-up messages. The Orchestrator appends new Gaps on each follow-up, and the intent evolves. A new Blackboard is created only when the Orchestrator determines the user has moved to an unrelated topic, or when the session ends. Sealed Blackboards are compressed into Knowledge Crystals (M3). The session has no idle timeout.

### 9.1 Lifecycle States

```
Created ──> Active ──> Idle ──> Active  (follow-up adds new Gaps)
                         │
                         └──> Sealed    (new topic, or session ends)
```

| State | Description |
|---|---|
| **Created** | `Orchestrator::run` instantiates a new `Blackboard` (fresh `Uuid`). Intent is not yet set; Gap DAG is empty. |
| **Active** | Gaps are in flight. `drive_gaps` is executing. The Blackboard accepts writes: Gap state changes, Evidence appends, approval registrations. |
| **Idle** | All current Gaps have reached a terminal state (`Closed`). Synthesis has returned a response to the user. **The Blackboard remains writable** — new Gaps can be inserted on the next user message. It is waiting for input. |
| **Sealed** | Crystallized and immutable. The Blackboard has been compressed into a Knowledge Crystal (M3) and is never written to again. |

### 9.2 Lifecycle Transitions

**Created → Active:** The first `orchestrator.decompose()` call sets the intent and inserts the initial Gaps. `drive_gaps` begins execution.

**Active → Idle:** `blackboard.all_closed()` returns `true`. `drive_gaps` exits. Synthesis runs and the response is returned to the user. The Blackboard stays in memory, holding all Gaps and Evidence, waiting for the next message.

**Active → Active (HITL):** When a solver step is gated, that gap task stays alive on the `drive_gaps` JoinSet awaiting human I/O. The Blackboard stays Active. The CLI currently surfaces `ApprovalRequested` events inline and sends the result on the gap's `oneshot` channel. `QuestionAsked` events also exist in the runtime model, but the CLI wiring is not complete yet.

**Idle → Active (follow-up):** A new user message arrives. `Orchestrator::run` calls `decompose()` with the new query and the full Blackboard state. The `Decomposition.is_follow_up` flag is `true`. The Orchestrator updates the intent, inserts the new Gaps, and calls `drive_gaps` again. New Gaps may declare dependencies on existing Closed Gaps — those dependencies are already satisfied, so the new Gaps promote to Ready immediately.

**Idle → Sealed (new topic):** A new user message arrives, but `Decomposition.is_follow_up` is `false`. `Orchestrator::run` creates a fresh Blackboard and runs the decompose output against it. (Sealing + crystallization is a TODO — currently only the fresh board is created.).

**Idle → Sealed (session end):** The user exits or the process crashes. The Blackboard is crystallized. (Sealing on exit is a TODO.)

### 9.3 Growable Gap DAG

The Blackboard's `intent` is **mutable** — each decompose call refines it. The synthesis step reads the current intent (not the original).

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

**Name uniqueness:** Gap names must be unique across the entire Blackboard lifetime. The `name_index` enforces this.

### 9.4 Ownership

The Orchestrator is the sole creator of Blackboards. The Solver receives `Arc<Blackboard>` to read gap context and write Evidence.

### 9.5 Invariants

- A Blackboard's Gap array only grows. Gaps are never removed or replaced.
- The intent is mutable — updated by the Orchestrator on each decompose call. The synthesis step always reads the latest intent.
- A Sealed Blackboard is immutable. No code path writes to it after crystallization.
- There is at most one active (Created/Active/Idle) Blackboard per session at any time.
- Sealing happens in two cases only: the Orchestrator signals a new topic, or the session ends.

---

## 10. Gap Lifecycle

```
Blocked ──> Ready ──> Assigned ───────────> Closed
```

`GapState` currently has four persisted states: `Blocked`, `Ready`, `Assigned`, and `Closed`. Approval / question waits happen inside a live solver task and are not modeled as a separate persisted gap state.

| State | Entry condition | Exit condition |
|---|---|---|
| **Blocked** | Gap has dependencies that are not yet Closed | All dependencies reach Closed state; auto-promoted to Ready by `promote_unblocked()` |
| **Ready** | No unresolved dependencies; eligible for scheduling | Picked up by `drive_gaps` and marked Assigned |
| **Assigned** | Solver::run has been invoked | Solver posts Evidence and marks the gap Closed. While assigned, the task may pause on approval or question `oneshot` channels without changing persisted `GapState`. |
| **Closed** | Terminal. The gap is resolved (success, terminal failure, or user rejection) | — |

**Gaps with no dependencies** skip Blocked and are inserted directly as Ready.

**Terminal failure:** A gap can be Closed with `Evidence.status = EvidenceStatus::Failure { reason }`. Automatic failure propagation to downstream gaps is not implemented in the current orchestrator.

---

## 11. Error Handling Strategy `IMPLEMENTED`

**Crate-level error type:**

```rust
#[derive(Debug, thiserror::Error)]
pub enum MossError {
    #[error("provider error: {0}")]
    Provider(#[from] ProviderError),

    #[error("solver error for gap {gap_id}: {reason}")]
    Solver { gap_id: Uuid, reason: String },

    #[error("defense scan rejected artifact: {reason}")]
    DefenseRejection { reason: String },

    #[error("blackboard error: {0}")]
    Blackboard(String),

    #[error("deadlock: blocked gaps remain but no gaps are ready or assigned")]
    Deadlock,

    #[error("session expired")]
    SessionExpired,

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
```

**Policy:** Every function that can fail returns `Result<T, MossError>`. The top-level `main.rs` loop catches errors and prints them to stderr without crashing. Individual Gap failures are isolated — they do not bring down the session.

---

## 12. Architecture Decisions

All ADRs live in `docs/ADR-*.md`. Key decisions:

| ADR | Title | Status |
|-----|-------|--------|
| [001](docs/ADR-001-blackboard-pattern.md) | Blackboard pattern over message-passing agents | Accepted |
| [002](docs/ADR-002-rust.md) | Rust as implementation language | Accepted |
| [003](docs/ADR-003-code-as-solver.md) | LLM-generated code as the execution primitive | Accepted |
| [004](docs/ADR-004-flat-agent-loops.md) | Flat agent loops, not recursive Orchestrators | Superseded by 009 |
| [005](docs/ADR-005-hitl-gated.md) | Human-in-the-loop via `GapState::Gated` | Accepted (updated by 007) |
| [006](docs/ADR-006-living-blackboard.md) | Living Blackboard with mutable intent and growable DAG | Accepted |
| [007](docs/ADR-007-defenseclaw-and-hitl-gating.md) | DefenseClaw and HITL gating | Accepted |
| [008](docs/ADR-008-broadcast-foundation.md) | Broadcast foundation | Accepted |
| [009](docs/ADR-009-unified-solver.md) | Unified Solver — eliminating the Compiler/Executor split | Accepted |
| [010](docs/ADR-010-solver-state-observability.md) | Solver state observability via Blackboard | Proposed |

---

## 13. Open Questions

These are unresolved design decisions.

1. **Streaming vs. batch Evidence.** Should the Solver post Evidence incrementally (streaming) or only after completion (batch)? Streaming enables HUD progress but complicates "done" semantics on Evidence.

2. **Embedding model for M3.** Local model (e.g., `nomic-embed-text` on GPU) vs. remote API (e.g., OpenAI embeddings via OpenRouter). Local keeps it offline; remote is simpler.

3. **Multi-user / multi-session.** Current design is single-user, single-session. Multiple concurrent sessions would require session-scoped namespacing and per-user Memory isolation.

---

## 14. Implementation Status Matrix

| Component | Layer | Status | Notes |
|---|---|---|---|
| CLI input loop | L5 | `IMPLEMENTED` | `src/cli.rs` — `Cli` struct, async stdin reader, calls `Moss::run`, prints response |
| CLI signal handling | L5 | `PARTIAL` | `src/cli.rs` — handles `ApprovalRequested` inline. `QuestionAsked` exists in the runtime model but is not fully wired through the CLI yet. |
| HUD delta streamer | L5 | `PLANNED` | Requires Blackboard change notifications |
| Orchestrator decompose | L4 | `IMPLEMENTED` | `orchestrator.rs` — minijinja template, LLM call, JSON deserialization; `Decomposition` includes `is_follow_up` flag |
| Orchestrator synthesize | L4 | `IMPLEMENTED` | Full Evidence from Blackboard via `all_evidence()` |
| Orchestrator execution loop | L4 | `IMPLEMENTED` | `drive_gaps()` private method; `runner.rs` dissolved |
| Blackboard data structures | L3 | `IMPLEMENTED` | All types, private fields, `pub(crate)` getters |
| Blackboard insert/mutate | L3 | `IMPLEMENTED` | `insert_gap`, `set_gap_state`, `append_evidence`, `set_intent`, `register_approval`, `approve`, `register_question`, `answer_question` |
| Blackboard dependency resolution | L3 | `IMPLEMENTED` | `promote_unblocked`, `drain_ready`, `all_closed` — unit tested. `all_gated_or_closed` removed (ADR-007). |
| Signal Bus integration | L3 | `IMPLEMENTED` | Every mutation emits `Event::Snapshot`. Solver emits `ApprovalRequested` and `QuestionAsked`; Blackboard manages the corresponding `oneshot` channels. |
| Solver (unified loop + step parser) | L2 | `IMPLEMENTED` | `solver.rs` — active runtime path used by `Orchestrator::drive_gaps()` |
| Solver prompt | L2 | `IMPLEMENTED` | `prompts/solver.md` — fixed frame with environment, working memory, dependency evidence, and last output |
| Legacy Compiler | L2 | `PARTIAL` | `compiler.rs` and `prompts/compiler.md` still exist in the repo but are no longer on the active runtime path |
| Legacy Executor | L2 | `PARTIAL` | `executor.rs` still exists in the repo but is no longer on the active runtime path |
| M1 session context | L1 | `PLANNED — design open` | Cross-board awareness; structure TBD |
| M2 Sled store | L1 | `PLANNED` | `sled` not in Cargo.toml |
| M3 Qdrant integration | L1 | `PLANNED` | `qdrant-client` not in Cargo.toml |
| Provider trait | L0 | `IMPLEMENTED` | Returns `Result<String, ProviderError>` |
| OpenRouter provider | L0 | `IMPLEMENTED` | No `.expect()` — all errors through `ProviderError` |
| Provider error handling | L0 | `IMPLEMENTED` | `thiserror` in Cargo.toml; `MossError` + `ProviderError` defined |
| Provider streaming | L0 | `PLANNED` | — |
| Provider tool calling | L0 | `PLANNED` | `complete_with_tools(messages)` stub returns `Err(ProviderError::NotSupported)` |
| MCP bridge | L0 | `PLANNED` | — |
| DefenseClaw | L0 | `IMPLEMENTED` | `artifact_guard.rs` — `ArtifactGuard` zero-field struct, string-pattern scan + size limit + `HITL_PATTERNS` gate |

**Recommended implementation order** (each phase is independently testable):

1. ~~**Error handling foundation.**~~ ✅ Done — `thiserror`, `MossError`, `ProviderError`, all `.expect()` removed.
2. ~~**Blackboard.**~~ ✅ Done — All data structures, `drain_ready`, `promote_unblocked`, `all_closed`, unit tested.
3. ~~**Orchestrator decompose + synthesize.**~~ ✅ Done — `minijinja` templates, LLM call, JSON parse, gap insertion in `Moss::run`.
4. ~~**Compiler.**~~ ✅ Done — `prompts/compiler.md`, Provider call, `Artifact` (Script/Agent), tested with LocalMock.
5. ~~**Executor (script path).**~~ ✅ Done — subprocess runner with timeout, Evidence written to Blackboard.
6. ~~**Signal Bus + Runner rewrite + CLI async loop (ADR-008).**~~ ✅ Done — `signal.rs` broadcast, `drive_gaps` on Orchestrator, `Cli` with `tokio::select!`.
7. ~~**DefenseClaw.**~~ ✅ Done — `ArtifactGuard`: 4-stage scan, HITL oneshot round-trip, `ApprovalRequested` signal.
8. **Solver cleanup.** The unified Solver path is in place. Remaining work is to remove legacy `compiler.rs`, `executor.rs`, and `prompts/compiler.md`, and finish the human-question CLI flow that matches the existing `QuestionAsked` runtime event.
9. **Memory (M1).** Session context layer. Enables cross-board awareness after topic changes.
10. **Memory (M2/M3).** Sled + Qdrant. Enables cross-session learning.
11. **HUD.** Subscribes to `SignalBus` from phase 6. Renders `Signal` events as terminal deltas. No new infrastructure — just another consumer.
