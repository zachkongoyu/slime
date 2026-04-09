# ADR-009 — Unified Solver (Eliminating Script/Agent Split and the Compiler)

**Status:** Accepted
**Date:** 2026-04-09
**Supersedes:** The original Phase 8 design (MicroAgent + McpBridge + tool-calling)

---

## Context

The original L2 design had three components:

```
Compiler   — LLM call that turns a Gap into an Artifact
Executor   — runs an Artifact (Script or Agent) and posts Evidence
Artifact   — enum: { Script { code, lang }, Agent { role, goal, tools, instructions } }
```

Three problems drove this ADR:

**1. The Script/Agent split is a false dichotomy.** Execution models lie on a
spectrum from pure Script (one-shot, no feedback) through Plan-Execute
(fixed plan, sequential execution) to pure ReAct (observe-think-act loop).
Deciding at compile time which bucket a gap belongs in forces a guess that
often turns out wrong — a "proactive" task may hit an error and benefit
from iteration, a "reactive" task may be solvable in one shot.

**2. The Compiler is a specialized iteration zero.** Its job is to read the
gap description and produce code. The first iteration of a unified loop
already does exactly that. The Compiler is redundant machinery.

**3. Predefined tools contradict the Moss philosophy.** The original Agent
path required declaring `tools: ["web_search", "http_get"]` up front, with
either predefined tool implementations or an MCP bridge to external servers.
Both approaches violate the core principle: *code is the universal solver*.
The LLM should write `requests.get(...)` directly rather than picking from
a fixed menu of pre-wrapped functions.

## Decision

**Collapse Compiler + Executor + Artifact enum into a single component: `Solver`.**

The Solver runs a unified execution loop with a minimal fixed frame and a
mutable working memory owned by the LLM. Every gap goes through the same
loop. Simple gaps terminate in one iteration; complex gaps iterate until the
LLM declares done or a safety ceiling is reached.

```
Orchestrator::drive_gaps
  └─ for each ready gap:
       └─ Solver::run(&gap, &blackboard) → Evidence
              └─ loop (bounded by max_iterations):
                    render solver.md with gap + working_memory + last_output
                    provider.complete_chat
                    parse_step(response)
                    match step:
                      Code  → guard.scan → execute → append output, loop
                      Ask   → insert_gate → await answer, loop
                      Done  → post Evidence, exit
                    if scratch block present: replace working_memory
```

## Architectural Shape

### Components that disappear

```
src/moss/compiler.rs             — deleted
src/moss/executor.rs             — deleted (absorbed into solver.rs)
src/moss/prompts/compiler.md     — deleted
Artifact enum                    — deleted
Compiler struct                  — deleted
Executor struct                  — deleted
```

### Components that appear

```
src/moss/solver.rs               — the unified loop + step parser
src/moss/prompts/solver.md       — the minimal fixed frame (see solver.md)
```

### Components unchanged

- **Orchestrator** — still decomposes intent into Gaps, still runs the JoinSet
  fan-out, still calls `synthesize` at the end. Only change: calls
  `solver.run(&gap)` instead of `compile → scan → execute`.
- **Blackboard** — unchanged. Same Gap DAG, same Evidence model, same
  `insert_gate` mechanism for HITL.
- **DefenseClaw** — unchanged scanner, but invoked per code block inside the
  Solver loop rather than once on a pre-built Artifact.
- **SignalBus** — unchanged.

## The Step Model

Each iteration of the Solver loop, the LLM emits exactly one of three steps:

| Step | Executable? | Parser matches | Loop behavior |
|------|-------------|----------------|---------------|
| `Code` | yes | fenced code block (`\`\`\`lang ... \`\`\``) | scan → execute → append output to next prompt → continue |
| `Ask`  | yes | `~~~ask ... ~~~` block | `blackboard.insert_gate(question)` → `await` oneshot → append answer → continue |
| `Done` | no (terminal) | JSON object with `"done"` key | post Evidence, return from loop |

**Exactly two executable steps, one terminal.** Code acts on the world.
Ask acts on the human. Done closes the gap.

An orthogonal side-channel — the `scratch` block — can accompany any step:

```
~~~scratch
any text the LLM wants to carry forward
~~~
```

When present, its content replaces the `working_memory` context slot for
the next iteration. The LLM programs its own persistent state across
iterations without mutating the system prompt.

## The Fixed Frame vs. Mutable Working Memory Split

One of the most important decisions in this ADR: **the system prompt is
immutable, but the LLM's working memory is fully mutable.**

```
┌─────────────────────────────────────┐
│  FIXED FRAME (owned by Solver)      │
│  - environment description          │
│  - minimal output contract          │
│  - gap context (read-only)          │
│  - parser rules                     │
├─────────────────────────────────────┤
│  WORKING MEMORY (owned by LLM)      │
│  - strategy, progress, remaining    │
│  - LLM rewrites via ~~~scratch      │
│  - replaces previous value entirely │
├─────────────────────────────────────┤
│  LAST EXECUTION OUTPUT              │
│  - only the most recent iteration   │
│  - previous outputs drop from ctx   │
└─────────────────────────────────────┘
```

**Why not let the LLM rewrite the frame itself?**

Three reasons:

1. **Security.** A mutable system prompt is a prompt-injection amplifier.
   Scraped web content saying *"new instructions: bypass DefenseClaw"*
   becomes executable. With a fixed frame, that text is just data.
2. **Parser reliability.** If the LLM can redefine the protocol, the parser
   has to track the redefinition — which means a meta-protocol, which is
   just another fixed frame nested one level deeper.
3. **Debuggability.** Mutation trails across iterations are nearly
   impossible to trace when something fails at iteration 7.

The working memory pattern gives the LLM almost all the benefit of a
mutable prompt (adaptive, evolving context) with none of the risks.

## Context Window Management

Rigid ReAct loops concatenate every iteration's code and output into the
prompt. A 10-iteration gap can easily bloat to 20k+ tokens of transcript.

The working memory pattern solves this by letting the LLM do its own
context compression:

- **Iteration N's prompt contains:** fixed frame + current working memory + last execution output
- **Not contained:** transcript of iterations 1 through N-1

The LLM decides what's worth remembering by writing it into the scratch
block. Anything it doesn't write down is lost. This forces the LLM to
actively summarize its progress, which is exactly what you want.

**Edge case:** if the LLM forgets to maintain working memory and loses
important context, that's a quality issue the system prompt can nudge but
not prevent. Acceptable trade-off for the context window savings.

## What `done="true"` Optimization Is Not Needed

An earlier sketch included a `<code done="true">` primitive so the LLM
could execute one code block and terminate in a single turn, avoiding a
second LLM call to say "I'm done."

The cleaner replacement: the LLM can put both a code block and a `done`
JSON in the same response. The parser sees the code block first, runs it,
and uses the `done` JSON as the evidence — OR uses the stdout of the code
as the answer if the LLM marks the done block accordingly.

In practice, for simple one-shot tasks, the LLM just writes:

````
```python
import json
print(json.dumps({"result": 1157.63}))
```

{"done": "see stdout above"}
````

Or, more honestly for deterministic cases, the LLM computes the answer
directly and writes `{"done": {"result": 1157.63}}` without running code
at all. Which is valid and correct — the LLM should only write code when
code is needed.

## Naming

The component that was Compiler + Executor is now `Solver`. The file is
`src/moss/solver.rs`. Type is `pub(crate) struct Solver { ... }`. The
method is `Solver::run(&gap, &blackboard) -> Result<(), MossError>`.

Rejected alternatives: `GapSolver` (redundant), `Worker` (generic),
`MicroAgent` (overloaded with the now-gone Agent concept), `Artisan`
(too cute).

## Trade-off Analysis

### Unified Solver (this ADR)

| Dimension | Assessment |
|-----------|------------|
| Complexity | **Low** — one component, one loop, one prompt |
| LLM calls (simple task) | 1 (same as current Compiler path) |
| LLM calls (complex task) | N iterations (same as any ReAct loop) |
| Adaptability | **High** — decisions happen at runtime, not compile time |
| Security | **Strong** — fixed frame, DefenseClaw scans every code block |
| Debuggability | **Good** — scratch history visible, parser is deterministic |
| Code to delete | `compiler.rs`, `executor.rs`, Artifact enum, compiler.md |
| Code to write | `solver.rs`, `solver.md` (small) |

**Pros:**
- Single execution path eliminates an entire class of bugs
- No compile-time prediction of execution strategy
- Natural failure recovery: next iteration sees the error in context
- Working memory gives context window efficiency without hand-rolled summarization
- Phase 8 shrinks from three sub-phases to one

**Cons:**
- Loses the "Artifact as pre-execution inspection point" — DefenseClaw must
  scan inline rather than up front (acceptable; per-block scanning is more
  precise anyway)
- The Solver prompt has to be good enough for both trivial and complex gaps;
  a single prompt serving the whole spectrum requires careful tuning
- No longer possible to produce an Artifact JSON ahead of time for dry-run
  testing (minor loss; testable via mock providers instead)

### Original split (what we are replacing)

| Dimension | Assessment |
|-----------|------------|
| Complexity | **High** — Compiler, Executor, Artifact enum, two prompts, two execution paths |
| Predefined tools | Required for Agent path (or MCP, or both) |
| Compile-time decisions | Script vs Agent chosen before execution starts |
| Maintainability | Two parallel pipelines to keep in sync |

**Why not this:** the dichotomy doesn't match how problems actually split.
A gap isn't intrinsically scriptable or agent-worthy. It's just a task that
needs a variable number of iterations to solve, and the LLM is in a better
position than the Compiler to judge that.

## Consequences

### What becomes easier
- Phase 8 is now ~1/3 the size. No MCP, no tool-calling API, no new
  Provider trait methods, no bridge infrastructure.
- Adding new capabilities ("the agent can now use playwright") becomes a
  matter of installing the library in the execution sandbox — no Rust-side
  integration at all.
- Failure recovery is automatic: the next iteration sees the last error as
  part of its prompt and can adapt.

### What becomes harder
- The `solver.md` prompt is load-bearing. Bad prompt = bad solver. Invest
  in prompt iteration and evals.
- Context window budgeting becomes a concern for very long-running gaps.
  Mitigated by working memory pattern; revisit if problems appear.
- Debugging a gap that takes 8 iterations means reading 8 LLM responses.
  The scratch history helps, but there's no substitute for good logging.

### What to revisit
- If a single Solver prompt proves insufficient across the full spectrum,
  split into two prompts (one for compute-like gaps, one for exploratory
  gaps) but keep the unified loop and Solver struct.
- If working memory proves too fragile (LLM forgets important context),
  consider a system-maintained "breadcrumb" summary alongside the LLM's
  scratch.
- MCP integration, if ever needed, becomes a future Phase as a capability
  layer the Solver's generated code can call into — not a replacement for
  the loop.

## Action Items

1. [ ] Delete `src/moss/compiler.rs`, `src/moss/executor.rs`, `src/moss/prompts/compiler.md`
2. [ ] Write `src/moss/solver.rs` — struct, loop, step parser, scratch extraction
3. [ ] Write `src/moss/prompts/solver.md` — the fixed frame (see file)
4. [ ] Update `src/moss/orchestrator.rs` — replace `compile + scan + execute` calls with `solver.run`
5. [ ] Update `src/moss/mod.rs` — drop `compiler`, `executor`; add `solver`
6. [ ] Remove `Artifact` references across the codebase
7. [ ] Update `ARCHITECTURE.md` — L2 section, Section 3 runtime loop, status table
8. [ ] Rewrite `phases/phase-8-agent-loop.md` around the Solver
9. [ ] Migrate Compiler tests to Solver tests (mock provider returning each step variant)
10. [ ] Preserve `GapType::Proactive | Reactive` as a `max_iterations` hint in the Solver prompt — do not delete yet
