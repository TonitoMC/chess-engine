# Chess Engine

A chess engine written in Rust, built on top of [Reckless](https://github.com/codedeliveryservice/Reckless) as a base for board representation, move generation, UCI handling, and threading. The project serves as an experimental testbed for comparing different search algorithms and evaluation functions, with each version tracked on its own branch.

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

## License

This project is licensed under the **GNU AFFERO GENERAL PUBLIC LICENSE v3.0**. It incorporates code from the [Reckless Engine](https://github.com/codedeliveryservice/Reckless).
