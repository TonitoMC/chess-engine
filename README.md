# Chess Engine

A performance-oriented chess engine written in Rust, built on top of [Reckless](https://github.com/codedeliveryservice/Reckless) as a base for move generation and board representation. This project benchmarks the efficiency of classical search heuristics against modern neural network evaluation and MCTS architectures.

## Project Variants (branch-organized)
The repository is structured by branch to measure elo progression across different logic stacks:

* **`v1-base`**: baseline alpha-beta search with simple material/pst evaluation.

## License
This project is licensed under the **GNU AFFERO GENERAL PUBLIC LICENSE v3.0**. It incorporates code from the [Reckless Engine](https://github.com/codedeliveryservice/Reckless).
