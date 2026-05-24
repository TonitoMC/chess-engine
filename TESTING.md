# Testing Guide

## Fastchess CLI

Basic match:
```bash
fastchess \
  -engine cmd=./arena/engines/engine-a name=engine-a \
  -engine cmd=./arena/engines/engine-b name=engine-b \
  -each tc=10+0.1 -rounds 300 -games 2 -concurrency 16 \
  -openings file=UHO_Lichess.epd format=epd order=random \
  -pgnout results.pgn
```

With Syzygy tablebases:
```bash
fastchess \
  -engine cmd=./arena/engines/engine-a name=engine-a option.SyzygyPath=/path/to/syzygy \
  -engine cmd=./arena/engines/engine-b name=engine-b option.SyzygyPath=/path/to/syzygy \
  -each tc=10+0.1 -rounds 300 -games 2 -concurrency 16 \
  -openings file=UHO_Lichess.epd format=epd order=random \
  -pgnout results.pgn
```

Against Stockfish at a fixed ELO:
```bash
fastchess \
  -engine cmd=./arena/engines/my-engine name=my-engine \
  -engine cmd=stockfish name=sf option.UCI_LimitStrength=true option.UCI_Elo=2000 \
  -each tc=10+0.1 -rounds 300 -games 2 -concurrency 16 \
  -openings file=UHO_Lichess.epd format=epd order=random \
  -pgnout results.pgn
```

Resume from config:
```bash
fastchess -config config.json
```

## Changing the Engine Name / Version

The UCI name comes from `Cargo.toml` + git SHA. It prints as:
```
id name Reckless <version>-<git-sha>
```

To change it for a specific build, edit `Cargo.toml`:
```toml
version = "1.0.0"   # shows as "Reckless 1.0.0-<sha>"
```

Or override the name entirely in `src/uci.rs:151`:
```rust
println!("id name MyEngineName {}", env!("ENGINE_VERSION"));
```

To suppress the git SHA (e.g. for a clean release tag), just tag the commit:
```bash
git tag v1.0.0
```
The build script uses `CARGO_PKG_VERSION` from Cargo.toml — changing that is the main lever.

## Potential Elo Upgrades (v4 → v5)

Roughly ordered by expected gain. All are in `src/search.rs` unless noted.

### High priority
- **LMR use continuation history** — currently LMR only uses `quiet_history`. Adding `conthist(ply, 1, mv)` to the reduction formula is consistent with how moves are scored in `movepick.rs` and should tighten reductions on moves with bad continuation scores.
  ```rust
  let r = ... - quiet_score / 8192 - td.conthist(ply, 1, mv) / 16384;
  ```
- **History pruning** — skip quiet moves at low depth when their history score is very negative. Essentially free nodes saved:
  ```rust
  if depth <= 4 && is_quiet && quiet_score < -1024 * depth { continue; }
  ```
- **Fix aspiration delta widening** — `delta = delta / 2 + 10` evaluates to 20 forever. Should actually grow on repeated fails:
  ```rust
  delta += delta / 2;  // or delta *= 2
  ```

### Medium priority
- **Killer moves** — store the quiet move that caused a beta cutoff at each ply and try it early next time the same ply is searched. Currently missing from `movepick.rs`. Adds a new `Stage::Killer` between `GoodNoisy` and `Quiet`.
- **Probcut** — before the main move loop at depth >= 5, try captures with a high SEE threshold. If they beat a raised beta in a reduced search, cut immediately. Uses `MovePicker::new_probcut(threshold)` which already exists in `movepick.rs`.
- **Double extensions** — when SE finds a singular move AND it gives check, extend by 2 instead of 1.
- **Multi-cut reduction** — if the SE search beats `s_beta` by a large margin (implying multiple cutoffs), reduce the TT move rather than extending.

### Lower priority / tuning
- **NMP return `null_score` instead of `beta`** — returning the actual score from the null search can be marginally better than the soft bound.
- **Raise `quiet_moves` / `noisy_moves` cap** — currently `ArrayVec::<Move, 32>`. Moves beyond 32 miss malus updates. Raise to 64 or `MAX_MOVES`.
- **LMP on noisy moves** — apply a similar skip for bad noisy moves beyond a threshold.

## Pending Tests

| Match | TC | Syzygy | Status |
|---|---|---|---|
| v3-test vs Stockfish | 10+0.1 | No | Pending |

## Completed Tests (approximate Elo)

| Match | Elo diff | Notes |
|---|---|---|
| v1-base vs SF2000 | +55 | ~2055 baseline |
| v1.5-move-ordering vs v1-base | +303 | history-ordered move picking |
| v2.4-additional-heuristics vs v2-tapered-eval | +564 | NMP + RFP + SE + LMP |
| v3-test-fixed vs v4-nnue-heuristics | ~-26 | v4 slightly stronger (small sample) |
