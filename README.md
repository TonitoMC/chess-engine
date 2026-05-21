# Chess Engine

A chess engine written in Rust, built on top of [Reckless](https://github.com/codedeliveryservice/Reckless) as a base for board representation, move generation, UCI handling, and threading. The project serves as an experimental testbed for comparing different search algorithms and evaluation functions, with each version tracked on its own branch.

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

| Branch             | Description                                                       |
| ------------------ | ----------------------------------------------------------------- |
| `v1-base`          | Baseline: HCE (Michniewski PSTs) + alpha-beta, no move ordering  |
| `v1.5-move-ordering` | Same eval, adds staged move ordering with history heuristics    |
| `v2-tapered-eval`  | PeSTO tapered eval (MG/EG PSTs) + full move ordering             |

## License

This project is licensed under the **GNU AFFERO GENERAL PUBLIC LICENSE v3.0**. It incorporates code from the [Reckless Engine](https://github.com/codedeliveryservice/Reckless).
