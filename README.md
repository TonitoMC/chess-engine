# Chess Engine

A chess engine written in Rust, built on top of [Reckless](https://github.com/codedeliveryservice/Reckless) as a base for board representation, move generation, UCI handling, and threading. The project serves as an experimental testbed for comparing different search algorithms and evaluation functions, with each version tracked on its own branch.

## Branch: `v3-nnue`

Builds on `v2.4-additional-heuristics`. Replaces the PeSTO HCE with a learned NNUE evaluation.

### Evaluation

- **Architecture** — 768→384x2→1 with 4 output buckets. Inputs are all 12 piece types × 64 squares from both perspectives (no king bucketing). Two accumulators (side-to-move and opponent) are maintained incrementally and concatenated before the output layer. Output bucket is selected by piece count, splitting the range 2–32 into 4 equal bands (opening → endgame).
- **SIMD** — the forward pass uses handwritten AVX2 intrinsics (falls back to scalar on unsupported hardware). Build with `RUSTFLAGS="-C target-feature=+avx2"` to enable.
- **Training** — trained with [Bullet](https://github.com/jnlt3/bullet) on the `nodes5000pv2_UHO.binpack` dataset. Data was generated using Stockfish at a fixed node budget per position (rather than fixed depth), which produces stronger nets on UHO opening books and is at least on par with depth-9 datasets on standard books.
- **Network file** — `networks/384net.nnue`, embedded at compile time via `include_bytes!` and transmuted directly into the `Parameters` struct. The file must match the struct layout exactly; a size mismatch is caught at compile time.

---

## Branch: `v2.4-additional-heuristics`

Builds on `v2.3-rfp`. Adds Late Move Pruning and Singular Extensions.

### Search additions

- **Late Move Pruning (LMP)** — at depth <= 4, if we've already tried enough quiet moves (threshold scales with depth: 8/12/16/20), skip the rest entirely rather than reducing. More aggressive than LMR for shallow nodes.
- **Singular Extensions** — before the move loop, if the TT move has sufficient depth and is a lower bound, search all *other* moves at reduced depth with a window just below the TT score. If nothing beats it, the TT move is "singular" — extend its search by 1 ply. Helps find tactics at the search horizon.

---

## Branch: `v2.3-rfp`

Builds on `v2.2-lmr`. Adds Reverse Futility Pruning.

### Search additions

- **Reverse Futility Pruning (RFP)** — in non-PV nodes where depth <= 8 and static eval beats beta by a margin of `75 * depth`, return the eval early. The intuition is that if we're already this far above beta, any move we make is unlikely to drop below it.

---

## Branch: `v2.2-lmr`

Builds on `v2.1-nmp`. Adds Late Move Reductions.

### Search additions

- **Late Move Reductions (LMR)** — quiet moves tried after the first 3 are searched at reduced depth `R = ln(depth) * ln(move_count) / 2`. If the reduced search still beats alpha, re-search at full depth to confirm. Not applied on the first move, captures, or when depth < 3.

---

## Branch: `v2.1-nmp`

Builds on `v2-tapered-eval`. Adds Null Move Pruning to the search.

### Search additions

- **Null Move Pruning (NMP)** — in non-PV nodes where static eval >= beta and the side to move has non-pawn material (avoiding zugzwang), make a null move and search at reduced depth (R = 3 + depth/3). If the reduced search still beats beta, prune the node. Disabled in check and when the previous move was also a null move.

---

## Branch: `v2-tapered-eval`

Builds on `v1.5-move-ordering`. Replaces the single-phase Michniewski PSTs with PeSTO's tuned middlegame/endgame tables, interpolated by remaining material so the engine transitions smoothly into endgame play.

### Evaluation

- **Material** — separate middlegame and endgame centipawn values per piece type (Rofchade-tuned)
- **Piece-square tables** — PeSTO's tuned MG/EG PSTs, interpolated by game phase
- **Tapered eval** — game phase computed from remaining material (max 24: queens×4, rooks×2, minors×1); score is a weighted blend of MG and EG scores
- **Insufficient material detection** — recognizes theoretical draws (KvK, KBvK, KNvK, same-colored bishops)

### Search

- **Iterative deepening** — searches increasing depths until time runs out, always keeping the last completed result
- **Alpha-beta (negamax)** — prunes branches that cannot affect the result
- **Quiescence search** — extends captures at leaf nodes to avoid the horizon effect
- **Transposition table** — caches results by Zobrist hash to avoid re-searching transposed positions
- **Move ordering** — TT move first, then captures (SEE-filtered), then quiet moves (history heuristic)

### Tooling

- **[En Croissant](https://encroissant.org/)** — GUI for interactive play and visual verification
- **[Fastchess](https://github.com/Disservin/fastchess)** — CLI for automated engine-vs-engine matches and Elo measurement

### Running

```bash
cargo build --release
./target/release/reckless
```

The engine communicates over UCI. Point any UCI-compatible GUI or `fastchess` at the binary.

## Project Variants (branch-organized)

| Branch               | Description                                                       |
| -------------------- | ----------------------------------------------------------------- |
| `v1-base`            | Baseline: HCE (Michniewski PSTs) + alpha-beta, no move ordering  |
| `v1.5-move-ordering` | Same eval, adds staged move ordering with history heuristics      |
| `v2-tapered-eval`    | PeSTO tapered eval (MG/EG PSTs) + full move ordering             |
| `v2.1-nmp`           | Adds Null Move Pruning                                            |
| `v2.2-lmr`           | Adds Late Move Reductions                                         |
| `v2.3-rfp`           | Adds Reverse Futility Pruning                                     |
| `v2.4-additional-heuristics` | Adds Late Move Pruning + Singular Extensions            |
| `v3-nnue`            | Replaces HCE with a trained 768→384x2→1 NNUE (4 output buckets) |

## License

This project is licensed under the **GNU AFFERO GENERAL PUBLIC LICENSE v3.0**. It incorporates code from the [Reckless Engine](https://github.com/codedeliveryservice/Reckless).
