# Tono Chess Engine

A chess engine written in Rust, built incrementally to study and compare different search and evaluation techniques used in modern engines. Each major version is tracked on its own branch; results are measured empirically via automated tournaments using `fastchess`.

The engine is based on [Reckless](https://github.com/codedeliveryservice/Reckless), reusing its board representation (bitboards), legal move generation, UCI protocol handling, and transposition table. The original neural evaluation was removed and rebuilt from scratch.

📄 [Full project report (PDF)](report.pdf)

---

## Shared Infrastructure (all branches)

All versions share the following components inherited or adapted from Reckless:

| Component | Description |
|---|---|
| **Board representation** | 64-bit bitboards per piece/color; O(1) set operations on squares |
| **Move generation** | Legal moves split into noisy (captures + promotions) and quiet; supports en passant, castling |
| **Iterative deepening** | Searches depth 1, 2, … until time runs out; each iteration seeds the next via the TT |
| **Alpha-beta (negamax/PVS)** | First move searched with full window; rest with null window, re-searched on fail-high |
| **Quiescence search** | At depth 0, continues searching captures until the position is quiet (stand-pat + captures only) |
| **Transposition table** | Zobrist-hashed cache; avoids re-searching positions reached by different move orders |
| **Draw detection** | 50-move rule, threefold repetition, insufficient material |
| **Lazy SMP** | Multiple threads share the TT and search in parallel |

---

## Branch Overview

### `v1-base` — Baseline Alpha-Beta

Minimal working engine. No move ordering — moves are iterated in raw generation order. The TT is read and written but the TT move is never prioritized.

**Evaluation:** fixed material values (P=100, N=320, B=330, R=500, Q=900) plus Michniewski's Simplified Evaluation Function PSTs — one table per piece, phase-independent.

---

### `v1.5-move-ordering` — Staged Move Ordering

Same evaluation as v1. Adds a full staged move picker:

1. TT move (best move from previous search at this position)
2. Good captures filtered by SEE (Static Exchange Evaluation) — sequences that win material go first
3. Quiet moves scored by history heuristics (quiet history, noisy history, continuation history at plies −1/−2/−4/−6)
4. Bad captures (SEE-negative) last

Move ordering does not change the correctness of the search — it only changes how quickly beta cutoffs are found. Better ordering → more pruning → faster search → more depth in the same time.

---

### `v2-tapered-eval` — PeSTO Evaluation

Replaces the single-phase Michniewski PSTs with PeSTO's tuned tables. Introduces tapered evaluation: separate middlegame and endgame scores interpolated by remaining material.

```
score = (mg_score × phase + eg_score × (24 − phase)) / 24
```

Phase is computed from material on the board (max 24: Q×4, R×2, minor×1). As pieces come off the board the evaluation transitions smoothly from middlegame to endgame weights. This is the baseline for all subsequent pruning experiments.

> Intermediate branches `v2.1-nmp`, `v2.2-lmr`, `v2.3-rfp` add Null Move Pruning, Late Move Reductions, and Reverse Futility Pruning respectively — each a single self-contained change on top of the previous.

---

### `v2.4-additional-heuristics` — Full Heuristic Engine

Builds on the full pruning stack (NMP → LMR → RFP) and adds:

- **Late Move Pruning (LMP)** — at depth ≤ 4, if enough quiet moves have already been tried (threshold 8/12/16/20 by depth), skip the rest entirely rather than reducing. More aggressive than LMR for shallow nodes.
- **Singular Extensions** — if the TT move is a lower-bound entry with sufficient depth, search all other moves at half-depth with a window just below the TT score. If nothing beats it, the TT move is "singular" and gets 1 extra ply. Helps the engine see deep into forcing tactical sequences.

This is the strongest purely heuristic version. Estimated ELO: **~2615–2700**.

<details>
<summary>Results vs Stockfish</summary>

| Opponent | Games | W/D/L | ELO diff | LOS |
|---|---|---|---|---|
| Stockfish 2300 | 100 | 91/9/0 | +401.92 ± 128.33 | 100% |
| Stockfish 2700 | 100 | 45/49/6 | −13.90 ± 65.35 | 33.6% |
| Stockfish 2700 + UHO book | 400 | 192/175/33 | +14.77 ± 31.23 | 82.4% |

Time control: 8+0.08s, 1 thread, 16 MB hash.

</details>

---

### `v3-test` — NNUE Evaluation (768 hidden)

Replaces PeSTO with a learned NNUE. The search stack is identical to v2.4.

**Architecture:** `(768 → 768) × 2 → 4 output buckets → 1`

- Input: 768 features — 12 piece types × 64 squares, binary (no king bucketing)
- Two accumulators (side-to-move + opponent perspective) maintained incrementally; only features that change on each move are updated rather than recomputing the full first layer
- Output bucket selected by piece count (4 bands over 2–32 pieces), letting the net specialize for different game phases
- Activation: Clipped ReLU in [0, 1]; weights quantized to i16 (first layer) and i8 (output)
- Forward pass uses AVX2 SIMD intrinsics; falls back to scalar on unsupported hardware
- Trained with [Bullet](https://github.com/jnlt3/bullet) on UHO positions annotated by Stockfish, with a nudging phase on lc0 game outcomes to correct positional biases

> Experimental NNUE variants (different network sizes, datasets, nudging combinations) are on branches `v3.1-512-hidden`, `v3.2-768-hidden`, `v3.3-1024-hidden`, `v3.4-1024-nudge`. The net on `v3-test` is the strongest found across those experiments.

---

### `v4-nnue-heuristics` — NNUE + Refined Search *(main)*

Keeps the v3-test NNUE and adds a layer of search improvements on top:

| Technique | Description |
|---|---|
| **Aspiration windows** | Each depth starts with a narrow window `[prev ± 20]`; widens and retries on fail-high/low. Saves work when the score is stable across depths. |
| **Improving heuristic** | Tracks whether eval improved over 2 plies ago. Tightens pruning margins when the position is deteriorating; loosens them when improving. Applied to RFP and LMP thresholds. |
| **Razoring** | At depth ≤ 4, if static eval is far enough below alpha, drop straight to qsearch instead of a full search. |
| **IIR (Internal Iterative Reduction)** | Without a TT move at depth ≥ 4, reduce depth by 1 — poor move ordering makes a deep search wasteful. |
| **Check extensions** | Moves that give check get 1 extra ply, on top of singular extensions. |
| **Futility pruning** | Skip quiet moves when static eval is far below alpha at low depth (complements RFP which prunes above beta). |
| **SEE pruning in search** | Bad captures skipped at depth ≤ 6, not just deprioritized in move ordering. |
| **Tuned LMR** | Reduction formula incorporates quiet history score and a check bonus for finer control. |

---

## Results Summary

Estimated ELO by version, measured against Stockfish at various levels. All tests: 8+0.08s, 1 thread, 16 MB hash.

| Version | Est. ELO | Notes |
|---|---|---|
| `v1-base` | — | Baseline |
| `v1.5-move-ordering` | — | |
| `v2-tapered-eval` | — | |
| `v2.4-additional-heuristics` | ~2615–2700 | Confirmed vs SF2700 with UHO book (400 games) |
| `v3-test` | — | NNUE 768 hidden |
| `v4-nnue-heuristics` | — | NNUE + refined search |

See [report.pdf](report.pdf) for full tournament results and analysis.

---

## Building

```bash
# Native CPU optimizations are enabled by default via .cargo/config.toml
cargo build --release

# Binary name varies by branch:
#   v1-base               → tono-chess-v1
#   v1.5-move-ordering    → tono-chess-v1_5
#   v2.4-*                → tono-chess-v2_4
#   v3-test               → tono-chess-v3
#   v4-nnue-heuristics    → tono-chess-v4
```

Pre-built native binaries for each main version are in `arena/engines/`:

```
arena/engines/Tono-Chess-V1
arena/engines/Tono-Chess-V1.5
arena/engines/Tono-Chess-V2.4
arena/engines/Tono-Chess-V3
arena/engines/Tono-Chess-V4
```

The engine speaks UCI. Point any UCI-compatible GUI (e.g. [En-Croissant](https://encroissant.org/)) or `fastchess` at the binary.

---

## Tools

| Tool | Role |
|---|---|
| [Reckless](https://github.com/codedeliveryservice/Reckless) | Base engine (board, move gen, UCI, TT) |
| [Stockfish](https://stockfishchess.org/) | Reference opponent + evaluation oracle for NNUE training data |
| [fastchess](https://github.com/Disservin/fastchess) | Automated engine-vs-engine tournaments; reports ELO, nELO, LOS |
| [Bullet](https://github.com/jnlt3/bullet) | NNUE training framework (Rust/CUDA) |
| [En-Croissant](https://encroissant.org/) | GUI for interactive play and debugging |

---

## License

GNU AGPL v3.0. Incorporates code from [Reckless](https://github.com/codedeliveryservice/Reckless).
