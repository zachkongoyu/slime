# Concurrent HITL Support — Fix Implementation

**Date:** 2026-04-23  
**Status:** IMPLEMENTED  
**Issue:** Multiple solvers requesting approvals/questions simultaneously would overwrite each other, dropping `oneshot::Sender`s and losing requests.

## Solution

Implemented a **queued menu system** for handling concurrent HITL interactions:

### Changes in `src/cli.rs`

**Data Structure Updates:**
- Changed `attention: Option<Attention>` → `attention_queue: Vec<Attention>` (queue-based)
- Added `attention_idx: usize` to track the currently selected item
- Added helper methods on `UiState`:
  - `current_attention()` — get currently selected item
  - `attention_next()` — move to next item in queue
  - `attention_prev()` — move to previous item in queue
  - `attention_pop_current()` — remove and respond to current item

**Event Handling (`apply()` method):**
- `Event::Approval` and `Event::Question` now **append to queue** instead of replacing

```rust
// Before: overwrites
self.attention = Some(Attention::Approval { .. });

// After: appends
self.attention_queue.push(Attention::Approval { .. });
```

**Key Bindings:**
- `↑/↓` (Up/Down arrows) — navigate between pending items
- `Enter` — submit response to the currently selected item
- Backspace / text input — edit response for current item

**UI Rendering:**
- Shows **numbered menu** of all pending items: `[1] gap_a — approval | [2] gap_b — question`
- Highlights selected item with `►` prefix
- Displays full details (reason/question) for selected item
- Shows help text: `↑/↓ to switch | Enter to submit`
- Status line shows total pending: `2 approvals | 1 question`

### Behavior

**Scenario: Two solvers request simultaneously**

1. Solver A sends Approval → added to queue as `[0]`
2. Solver B sends Question → added to queue as `[1]`, index stays at `[0]`
3. User sees menu: `► [1] gap_a — approval` and `  [2] gap_b — question`
4. User presses ↓ → now viewing `[2] gap_b — question`
5. User types answer, presses Enter
   - Question is answered, Solver B's `oneshot::Sender` receives the response
   - Item is removed from queue
   - Index moves back to `[1]` (which was originally Solver A's)
6. Solver A's request is still in queue and waiting for user response

**No more dropped `oneshot::Sender`s** — all responses are preserved until explicitly answered.

### Blast Radius

- CLI keybinding behavior changed (now responds to arrow keys)
- No changes to the Blackboard, Solver, or Orchestrator
- No changes to the `Event` enum in `signal.rs`
- Fully backward compatible with single-solver flow (queue of size 1 behaves like the old single-item)

## Testing Recommendations

1. **Serial HITL** (baseline): Single solver with one approval → still works as before
2. **Concurrent queue**: Run a query that spawns multiple gaps requiring approvals simultaneously
   - Verify menu appears with all items
   - Verify arrow keys navigate correctly
   - Verify each response goes to the correct solver
3. **Mixed types**: Concurrent approvals and questions together
4. **Edge case**: User answers items out of order (e.g., answer [2] before [1])

## Code Quality

✓ No unwrap()s added  
✓ No breaking changes to public API  
✓ Builds cleanly with 5 minor warnings (pre-existing + unused helpers for future use)  
✓ Minimal diff — only state management and rendering changes
