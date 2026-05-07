# Chess Engine

A chess engine written in Rust, built on top of [Reckless](https://github.com/codedeliveryservice/Reckless) as a base for board representation, move generation, UCI handling, and threading. The project serves as an experimental testbed for comparing different search algorithms and evaluation functions, with each version tracked on its own branch.

## Branch: `v1-base`

This is the baseline version. The original NNUE neural network evaluation from Reckless was stripped out and replaced with a classical hand-crafted evaluation (HCE), and the search was simplified to a clean alpha-beta implementation.

### Evaluation

- **Material** — fixed centipawn values per piece type (pawn 100, knight 320, bishop 330, rook 500, queen 900)
- **Piece-square tables** — positional bonuses/penalties per square based on Tomasz Michniewski's Simplified Evaluation Function

### Search

- **Iterative deepening** — searches increasing depths until time runs out, always keeping the last completed result
- **Alpha-beta (negamax)** — prunes branches that cannot affect the result
- **Quiescence search** — extends captures at leaf nodes to avoid the horizon effect
- **Transposition table** — caches results by Zobrist hash to avoid re-searching transposed positions
- **Move ordering** — TT move first, then captures (MVV-LVA), then quiet moves (history heuristic)

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

| Branch | Description |
|---|---|
| `v1-base` | Baseline: HCE (material + PSTs) + alpha-beta search |

## License

This project is licensed under the **GNU AFFERO GENERAL PUBLIC LICENSE v3.0**. It incorporates code from the [Reckless Engine](https://github.com/codedeliveryservice/Reckless).
