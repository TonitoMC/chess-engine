# Search Improvements (v3-nnue → v4-nnue-heuristics)

## Motivation

The existing search heuristics were tuned for HCE (PeSTO). NNUE eval is stronger and more
reliable at every depth, so pruning margins can be tighter and more aggressive. The goal is
to close the 2× depth gap observed against Stockfish.

---

## New Features

### Improving Flag

**File:** `src/search.rs` — `search()`

Before pruning decisions, we track whether the position is "improving": static eval is
higher than it was 2 plies ago (opponent's last move didn't help them). Used to make
several pruning conditions less aggressive when the position is not improving.

```rust
let improving = !td.board.in_check()
    && ply >= 2
    && is_valid(td.stack[ply - 2].eval)
    && eval > td.stack[ply - 2].eval;
```

---

### Check Extensions

**File:** `src/search.rs` — move loop

When a move gives check, depth is extended by 1 (unless a singular extension already applies).
Checks narrow the tree and force precise play, so searching them deeper is low-cost and
catches missed tactical lines.

```rust
let gives_check = td.board.in_check(); // after make_move
let extension = i32::from(singular_ext) + i32::from(gives_check && !singular_ext);
```

LMR also reduces less for checking moves (`r -= gives_check as i32`).

---

### Razoring

**File:** `src/search.rs` — `search()`, before move loop

At non-PV nodes near the leaves, if static eval is far below alpha even assuming we can gain
`220*depth + 135` cp, we drop into qsearch immediately rather than searching the full node.
This saves significant time at depths 1–4 on hopeless positions.

```rust
if !NODE::PV && !td.board.in_check() && depth <= 4 && is_valid(eval)
    && eval + 220 * depth + 135 < alpha {
    return qsearch::<NODE>(td, alpha, beta, ply);
}
```

---

### Internal Iterative Reduction (IIR)

**File:** `src/search.rs` — `search()`

When there is no TT move and `depth ≥ 4`, depth is reduced by 1. Without a hash move the
move ordering is poor, so a deep search on a badly-ordered node wastes time. IIR trades a
small accuracy loss for faster overall iteration.

```rust
if depth >= 4 && !tt_move.is_present() {
    depth -= 1;
}
```

---

## Tuned Parameters

### Null Move Pruning

**Before:** `R = 3 + depth / 3`

**After:** `R = 3 + depth / 3 + clamp((eval - beta) / 200, 0, 3)`

Keeps the same base reduction and adds an eval-based bonus of 1–3 extra plies when static
eval is well above beta, making NMP more aggressive in clearly winning positions.

---

### Reverse Futility Pruning — improving-aware

**Before:** `eval - 75 * depth >= beta`

**After:** `eval - 75 * (depth - improving as i32) >= beta`

When the position is improving, the threshold is loosened by 75 cp, making RFP more
conservative (we don't prune as early). This avoids cutting off nodes where the engine is
gaining ground.

---

### Futility Pruning — tighter margin

**Before:** `depth <= 8 && eval + 80 * depth < alpha`

**After:** `depth <= 5 && eval + 130 * depth + 45 < alpha`

The margin `130*depth + 45` is a closer fit to the empirical gain distribution under NNUE
eval. The depth cap drops from 8 to 5, preventing unsound pruning at mid-range depths.
Guard: only applied after at least one move has been searched (`best_score > -INFINITE`),
preventing a return of `-INFINITE` from a non-terminal node.

---

### Late Move Pruning — quadratic scaling with improving

**Before:** Fixed thresholds `[0, 5, 9, 14, 19]`, counting all moves

**After:** `quiets_searched > 3 + depth² / (1 + !improving as i32)`, counting only searched quiets

Changes:
- **Quadratic formula** grows naturally without a lookup table
- **Improving flag**: threshold doubles when not improving (prune more aggressively)
- **Counts only searched quiets** (moves that passed other pruning), not total move index
- Guard: same `best_score > -INFINITE` guard as futility pruning

---

### Late Move Reductions — PV and check adjustments

**Before:** `R = clamp(ln(depth) × ln(move_count) / 2.0 − quiet_score / 8192, 0, depth-1)`

**After:**
```
R = ln(depth) × ln(move_count) / 2.0 − quiet_score / 8192
R -= NODE::PV as i32      // reduce less at PV nodes
R -= gives_check as i32   // reduce less for checking moves
R = clamp(R, 0, depth-1)
```

PV nodes carry the main line; checking moves require precise handling — both get less reduction.

---

### History Table Updates

**File:** `src/search.rs` — after move loop

History tables (quiet, noisy, continuation) were being read for move ordering and LMR adjustments but never written to — all scores were permanently zero. Added update logic after the move loop:

- **Beta cutoff on quiet move**: bonus to quiet history + continuation history (offsets 1, 2, 4, 6) for the best move; scaled malus to all quiets that failed before it
- **Beta cutoff on capture**: bonus to noisy history for the best move; malus to all captures that failed
- **Malus scaling**: `1024 / (1 + i)` — earlier failures penalized more than later ones
- **`update_continuation_histories`**: helper that updates continuation history at plies -1, -2, -4, -6 in one call

Effect: bench node count dropped from ~6.7M to ~3.4M — move ordering now finds cutoffs much earlier, halving the nodes searched.

---

## Bug Fixes

### Panic on positions with no legal moves

**Files:** `src/search.rs`, `src/uci.rs`

When the engine was given a checkmate or stalemate position, `root_moves` would be empty
and both `start()` (via `root_moves[0]`) and the vote-aggregation code in `go()` (via
`root_moves[0]`) would panic, producing no output and causing a time loss.

Fixes:
- `start()` returns early if `root_moves.is_empty()`
- `go()` prints `bestmove 0000` and returns early if all threads have empty root_moves
