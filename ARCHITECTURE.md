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
The user-facing surface. `Cli` struct in `src/cli.rs` drives a `tokio::select!` loop over stdin + signal bus. The planned HUD component subscribes to the same signal bus for real-time delta streaming.

| Sub-component | Status | Notes |
|---|---|---|
| CLI input loop | `IMPLEMENTED` | `src/cli.rs` — `Cli` struct, async stdin reader, calls `Moss::run`, prints response. |
| CLI signal handling | `IMPLEMENTED` | `src/cli.rs` — inner `tokio::select!` over stdin + signal receiver. Surfaces `ApprovalRequested` events inline, prompts `[y/N]`, calls `Moss::approve()`. |
| HUD delta streamer | `PLANNED` | Another signal bus consumer. Phase 11. |

**L4 — Orchestrator** `IMPLEMENTED`
The strategic coordinator. `Orchestrator::run` is the single entry point: decompose → insert Gaps → `drive_gaps` (private JoinSet loop) → synthesize. Owns the current `Arc<Blackboard>` and the broadcast sender.

| Sub-component | Status | Notes |
|---|---|---|
| Intent-to-DAG decomposition (single LLM call) | `IMPLEMENTED` | `orchestrator.rs` — `decompose` renders `prompts/decompose.md` via `minijinja`, calls LLM, deserializes into `Decomposition` (includes `is_follow_up` flag). |
| Response synthesis (Evidence → answer) | `IMPLEMENTED` | `orchestrator.rs` — `synthesize` renders `prompts/synthesize.md`, passes real evidence from `blackboard.all_evidence()`. |
| Execution loop (poll, dispatch, evidence, synthesis) | `IMPLEMENTED` | `orchestrator.rs` — `Orchestrator::drive_gaps()` private method. `runner.rs` dissolved. JoinSet fan-out, `MAX_RETRIES=3`, deadlock detection. |
| Context injection (M1/M3 retrieval before planning) | `PLANNED` | — |

**L3 — Blackboard** `PARTIAL`
Living workspace using `DashMap` for lock-free concurrent access. Holds the intent (mutable), the Gap DAG (append-only), accumulated Evidence, and human-in-the-loop Gates. A Blackboard stays open across follow-up messages — new Gaps are inserted and the intent is refined on each decompose call. It is sealed only when the topic changes or the session ends (see Section 9).

| Sub-component | Status | Notes |
|---|---|---|
| Data structures (Gap, Evidence, Blackboard) | `IMPLEMENTED` | `blackboard.rs` — `GapState`, `GapType`, `Gap`, `EvidenceStatus`, `Evidence`, `Blackboard` with private fields and `pub(crate)` getters |
| Insert/mutate operations | `IMPLEMENTED` | `insert_gap`, `set_gap_state`, `append_evidence`, `set_intent`, `register_approval`, `approve` |
| Dependency resolution (auto-unblock) | `IMPLEMENTED` | `promote_unblocked`, `drain_ready`, `all_closed` — unit tested. `all_gated_or_closed` removed (ADR-007). |
| Signal Bus integration | `IMPLEMENTED` | Every `Blackboard` mutation emits `Event::Snapshot` via broadcast. `Orchestrator` emits `Event::ApprovalRequested` on HITL gate. `register_approval()`/`approve()` manage the `oneshot` pair. |

**L2 — Solver** `PLANNED`
The Solver is the unified execution component that replaces the former Compiler + Executor + Artifact split (see ADR-009). For every Gap, the Solver runs a bounded loop: render `solver.md` with the gap context and current working memory, call the LLM, parse the response into one of three step types (`Code`, `Ask`, `Done`), and act accordingly. Simple Gaps finish in one iteration; complex Gaps iterate up to a ceiling derived from `GapType`.

| Sub-component | Status | Notes |
|---|---|---|
| Solver loop + step parser | `PLANNED` | `solver.rs` — struct, loop, `Code`/`Ask`/`Done` parser, scratch extraction |
| Solver prompt | `PLANNED` | `prompts/solver.md` — fixed frame with gap context + working memory + last output slots |
| Sandbox / isolation | `PLANNED` | subprocess with restricted env, cgroups/ulimit |

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
| ArtifactGuard (pre-exec scanner) | `IMPLEMENTED` | `artifact_guard.rs` — zero-field struct, 4-stage scan pipeline, `HITL_PATTERNS` const. See Section 8. |

---

## 3. Core Runtime Loop

This is the central execution flow that ties L4, L3, and L2 together. It is called once per user message. The same Blackboard may pass through this loop many times across follow-up messages.

`IMPLEMENTED` (Compiler/Executor path, to be replaced) / `PLANNED` (Solver) — `Moss::run` → `Orchestrator::run` → `decompose` → gap insertion → `drive_gaps` (Solver per gap, including HITL round-trip) → `synthesize`.

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
    |                  render solver.md (fixed frame + working memory + last output)
    |                  provider.complete_chat
    |                  parse step (Code | Ask | Done)
    |                  Code → ArtifactGuard.scan → execute subprocess → append stdout/stderr → loop
    |                  Ask  → register_approval (fires ApprovalRequested broadcast)
    |                         await oneshot → append answer → loop
    |                         (gap task stays alive on JoinSet; other gaps proceed)
    |                  Done → post Evidence to Blackboard, exit loop
    |                  If scan verdict is Gated: same flow as Ask above
    |                  If scan verdict is Rejected: post Failure evidence, exit loop
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

- **Fan-out limit.** A `tokio::Semaphore` caps the number of concurrently executing Gaps (default: 4). This bounds LLM call parallelism and subprocess count.
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
    blackboard: Mutex<Arc<Blackboard>>,
    tx: broadcast::Sender<signal::Payload>,
}

impl Orchestrator {
    pub(crate) fn new(provider: Arc<dyn Provider>, tx: broadcast::Sender<signal::Payload>) -> Self;
    pub(crate) fn approve(&self, gap_id: Uuid, approved: bool);
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
  "intent": "string — the current/updated goal",
  "is_follow_up": true,
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

`Blackboard::snapshot()` serializes the board into a `BlackboardSnapshot` struct (intent + all Gaps + all Evidence) that is rendered into the planning prompt via minijinja:

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
    Partial,                     // Solver hit iteration ceiling before gap was resolved
}
```

`Blackboard.evidences` is `DashMap<Uuid, Vec<Evidence>>` — an ordered attempt log per gap. The Solver on retry iteration N sees the prior `Evidence` records in context so it can adapt.

`Blackboard` includes `name_index: DashMap<Box<str>, Uuid>` for O(1) name-to-ID resolution. Written once in `insert_gap`, never mutated after that. `intent` is stored as `Mutex<Option<Box<str>>>` for safe mutation through a shared `&self` reference. `pending_approvals: DashMap<Uuid, oneshot::Sender<bool>>` stores the sender half of each HITL oneshot channel until `approve()` is called.

**Thread-safety model:**
`DashMap` provides per-shard read/write locks internally. Individual Gap state transitions (Ready -> Assigned) must be atomic. Use `DashMap::get_mut` which holds a write lock on the shard for the duration of the returned `RefMut`. The `drain_ready` method should iterate and CAS (compare-and-swap) in a single pass to avoid TOCTOU races where two threads both see the same gap as Ready.

### 4.3 Solver `PLANNED`

**Responsibility:** Execute a single Gap from the Blackboard to a final Evidence record. Replaces the former Compiler + Executor + Artifact split (ADR-009). The Solver owns the complete execution lifecycle for one Gap: it renders the prompt, calls the LLM, parses the step, acts, and iterates until the LLM emits `Done` or the iteration ceiling is reached.

**Lifecycle:** The Orchestrator does not hold a Solver as a field. `drive_gaps` constructs one Solver per ready Gap and spawns it into the JoinSet. A Solver lives for exactly one Gap and is dropped when the loop exits. The Orchestrator's `provider` and `guard` are cloned (`Arc`) into each Solver.

**Interface:**

```rust
pub(crate) struct Solver {
    provider: Arc<dyn Provider>,
    guard: Arc<ArtifactGuard>,
}

impl Solver {
    pub(crate) fn new(provider: Arc<dyn Provider>, guard: Arc<ArtifactGuard>) -> Self;

    /// Run the unified execution loop for `gap`.
    /// Posts Evidence to `blackboard` and marks the Gap Closed on exit.
    /// The iteration ceiling is derived from `gap.gap_type` (Proactive → lower, Reactive → higher).
    pub(crate) async fn run(&self, gap: &Gap, blackboard: &Blackboard) -> Result<(), MossError>;
}
```

**Step model — exactly three variants:**

| Step | Parser match | Loop behavior |
|------|-------------|---------------|
| `Code` | fenced code block (`` ```lang ... ``` ``) | `guard.scan` → execute subprocess → append stdout/stderr to next prompt → loop |
| `Ask`  | `~~~ask ... ~~~` block | `blackboard.register_approval(question)` → await oneshot → append answer → loop |
| `Done` | JSON object containing `"done"` key | post Evidence using the `done` value (or last stdout), mark Gap Closed, return |

An optional `~~~scratch ... ~~~` block appended to any response replaces the `working_memory` slot in the next iteration's prompt. The LLM programs its own context compression this way.

**Prompt contract (`solver.md`):**
Three slots, fixed frame, no LLM mutation allowed:
1. **Fixed frame** — environment description, output contract, parser rules (never changes).
2. **Working memory** — owned by the LLM, rewritten by the `scratch` side-channel. Carries strategy, progress, remaining steps.
3. **Last execution output** — only the most recent Code step's stdout/stderr. Prior outputs drop from context.

**Execution model:**
1. Render `solver.md` template with gap context, current `working_memory`, and `last_output`.
2. Call `provider.complete_chat`.
3. Parse response into `(step, Option<new_working_memory>)`.
4. If `Code`: `guard.scan(code)` → on `Approved` or `Gated` (after user approves), write to `NamedTempFile`, spawn `tokio::process::Command`, capture stdout/stderr, update `last_output`.
5. If `Ask`: register approval gate, await human response, update `last_output`.
6. If `Done`: serialize evidence from the `done` value (or last stdout), call `blackboard.append_evidence`, return.
7. On iteration ceiling: post `EvidenceStatus::Partial` and return.

**Design note:** The Solver receives only the specific Gap description and dependency Evidence — not the full Blackboard. Principle of least privilege is preserved.

### 4.5 DAG Scheduler

The scheduler is not a separate component — it is `Orchestrator::drive_gaps` (Section 3). `runner.rs` was dissolved into the Orchestrator (Phase 7). This is a deliberate simplification: an external scheduler would add an inter-component communication layer without clear benefit at this scale.

**Scheduling strategy:** Non-preemptive, event-driven. Gaps are not assigned on a timer; they are spawned into the JoinSet when (a) they become Ready and (b) a semaphore permit is available. When a gap completes and posts Evidence, the `promote_unblocked` sweep runs synchronously before the next iteration, ensuring newly-unblocked gaps are immediately eligible.

**Failure policy:**

| Failure type | Behavior |
|---|---|
| Code block exits non-zero (within Solver) | The error output is appended to the next iteration's context. The Solver loop continues — the LLM sees the error and can adapt. After `max_iterations` (derived from `GapType`), mark Gap Closed with `EvidenceStatus::Failure` and propagate to dependents. |
| Solver hits iteration ceiling | Serialize last working memory + last output as partial Evidence. Mark Closed with `EvidenceStatus::Partial`. |
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
| 1. Static analysis | Forbidden imports (`os.system`, `subprocess`, `shutil.rmtree`), network calls in Proactive scripts, filesystem writes outside sandbox | AST parsing (Python `ast` module via a small Python helper, or `tree-sitter` from Rust) |
| 2. Capability check | Does the artifact require capabilities beyond what the Gap's constraints allow? | Compare requested tool names against the Gap's permitted tool list |
| 3. Resource bounds | Are timeout and memory limits set? | Config validation |
| 4. HITL gate | Is this a high-risk action (e.g., sending email, deleting files, making purchases)? | Pattern match against `HITL_PATTERNS` const (pattern + category tuples); if matched, emit `Event::ApprovalRequested` and await `oneshot` response |

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
    pub(crate) fn scan(&self, artifact: &Artifact, constraints: Option<&Value>) -> ScanVerdict;
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

**Active → Active (HITL):** When one or more gaps are Gated, those gap tasks are still alive on the `drive_gaps` JoinSet, awaiting human I/O. The Blackboard stays Active. The CLI receives `ApprovalRequested` events via broadcast and surfaces them inline. The user enters `y` or `N` in the `[y/N]` prompt, which sends on the gap's `oneshot` channel. The gap task resumes (or closes), the loop picks up the completion, promotes dependents, and continues. No special HITL loop — this is just normal async I/O within the execution loop.

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
Blocked ──> Ready ──> Assigned ──> Gated ──> Ready  (on user approval)
                                 │
                                 └─────────> Closed  (on user rejection)
                    Assigned ──────────────> Closed  (normal completion)
```

This is a one-directional state machine. The only backward arc is `Gated → Ready`, which requires explicit user action.

| State | Entry condition | Exit condition |
|---|---|---|
| **Blocked** | Gap has dependencies that are not yet Closed | All dependencies reach Closed state; auto-promoted to Ready by `promote_unblocked()` |
| **Ready** | No unresolved dependencies; eligible for scheduling | Picked up by `drive_gaps` and marked Assigned |
| **Assigned** | Solver::run has been invoked | Solver posts Evidence and marks the gap Closed, OR ArtifactGuard gates a code block → Gated |
| **Gated** | Gap needs human action (security approval, user input, judgment call, physical action). The gap task stays alive on the JoinSet awaiting a `oneshot` response — this is just async I/O. `drive_gaps` has no gate-specific logic. | User enters `y` at `[y/N]` prompt → task resumes execution; user enters `N` → Closed with terminal failure |
| **Closed** | Terminal. The gap is resolved (success, terminal failure, or user rejection) | — |

**Gaps with no dependencies** skip Blocked and are inserted directly as Ready.

**Terminal failure:** A gap can be Closed with `Evidence.status = EvidenceStatus::Failure { reason }`. Downstream gaps that depend on a terminally-failed gap are also marked as terminally failed without execution — the Orchestrator propagates failure through the DAG.

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
| CLI signal handling | L5 | `IMPLEMENTED` | `src/cli.rs` — `tokio::select!` + signal receiver, surfaces `ApprovalRequested` inline, `[y/N]` prompt, calls `Moss::approve()` |
| HUD delta streamer | L5 | `PLANNED` | Requires Blackboard change notifications |
| Orchestrator decompose | L4 | `IMPLEMENTED` | `orchestrator.rs` — minijinja template, LLM call, JSON deserialization; `Decomposition` includes `is_follow_up` flag |
| Orchestrator synthesize | L4 | `IMPLEMENTED` | Full Evidence from Blackboard via `all_evidence()` |
| Orchestrator execution loop | L4 | `IMPLEMENTED` | `drive_gaps()` private method; `runner.rs` dissolved |
| Blackboard data structures | L3 | `IMPLEMENTED` | All types, private fields, `pub(crate)` getters |
| Blackboard insert/mutate | L3 | `IMPLEMENTED` | `insert_gap`, `set_gap_state`, `append_evidence`, `set_intent`, `register_approval`, `approve` |
| Blackboard dependency resolution | L3 | `IMPLEMENTED` | `promote_unblocked`, `drain_ready`, `all_closed` — unit tested. `all_gated_or_closed` removed (ADR-007). |
| Signal Bus integration | L3 | `IMPLEMENTED` | Every mutation emits `Event::Snapshot`. `register_approval`/`approve` for HITL oneshot. `signal_tx()` for external emission. |
| Solver (unified loop + step parser) | L2 | `PLANNED` | `solver.rs` — replaces Compiler + Executor (ADR-009) |
| Solver prompt | L2 | `PLANNED` | `prompts/solver.md` — fixed frame, working memory, last output |
| ~~Compiler~~ | L2 | `DELETED` | Removed by ADR-009; `compiler.rs` and `prompts/compiler.md` to be deleted |
| ~~Executor~~ | L2 | `DELETED` | Removed by ADR-009; `executor.rs` to be deleted |
| M1 session context | L1 | `PLANNED — design open` | Cross-board awareness; structure TBD |
| M2 Sled store | L1 | `PLANNED` | `sled` not in Cargo.toml |
| M3 Qdrant integration | L1 | `PLANNED` | `qdrant-client` not in Cargo.toml |
| Provider trait | L0 | `IMPLEMENTED` | Returns `Result<String, ProviderError>` |
| OpenRouter provider | L0 | `IMPLEMENTED` | No `.expect()` — all errors through `ProviderError` |
| Provider error handling | L0 | `IMPLEMENTED` | `thiserror` in Cargo.toml; `MossError` + `ProviderError` defined |
| Provider streaming | L0 | `PLANNED` | — |
| Provider tool calling | L0 | `PLANNED` | `complete_with_tools` stub returns `Err(ProviderError::NotSupported)` |
| MCP bridge | L0 | `PLANNED` | — |
| DefenseClaw | L0 | `IMPLEMENTED` | `artifact_guard.rs` — `ArtifactGuard` zero-field struct, 4-stage scan, `HITL_PATTERNS` const |

**Recommended implementation order** (each phase is independently testable):

1. ~~**Error handling foundation.**~~ ✅ Done — `thiserror`, `MossError`, `ProviderError`, all `.expect()` removed.
2. ~~**Blackboard.**~~ ✅ Done — All data structures, `drain_ready`, `promote_unblocked`, `all_closed`, unit tested.
3. ~~**Orchestrator decompose + synthesize.**~~ ✅ Done — `minijinja` templates, LLM call, JSON parse, gap insertion in `Moss::run`.
4. ~~**Compiler.**~~ ✅ Done — `prompts/compiler.md`, Provider call, `Artifact` (Script/Agent), tested with LocalMock.
5. ~~**Executor (script path).**~~ ✅ Done — subprocess runner with timeout, Evidence written to Blackboard.
6. ~~**Signal Bus + Runner rewrite + CLI async loop (ADR-008).**~~ ✅ Done — `signal.rs` broadcast, `drive_gaps` on Orchestrator, `Cli` with `tokio::select!`.
7. ~~**DefenseClaw.**~~ ✅ Done — `ArtifactGuard`: 4-stage scan, HITL oneshot round-trip, `ApprovalRequested` signal.
8. **Solver (Phase 8).** Delete `compiler.rs`, `executor.rs`, `prompts/compiler.md`, `Artifact` enum. Write `solver.rs` (struct, bounded loop, `Code`/`Ask`/`Done` step parser, scratch extraction) and `prompts/solver.md` (fixed frame). Update Orchestrator to call `solver.run(&gap)`. Migrate Compiler tests to Solver tests (mock provider returning each step variant).
9. **Memory (M1).** Session context layer. Enables cross-board awareness after topic changes.
10. **Memory (M2/M3).** Sled + Qdrant. Enables cross-session learning.
11. **HUD.** Subscribes to `SignalBus` from phase 6. Renders `Signal` events as terminal deltas. No new infrastructure — just another consumer.
