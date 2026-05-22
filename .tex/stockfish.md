# Stockfish Test Results

## Test 1 — vs SF 2300 ELO (8+0.08, 1t, 16MB)

- Elo: 401.92 +/- 128.33
- nElo: 524.36 +/- 68.10
- LOS: 100.00 %
- DrawRatio: 18.00 %
- PairsRatio: inf
- Games: 100
- Wins: 91
- Losses: 9
- Draws: 0
- Points: 91.0 (91.00 %)
- Ptnml(0-2): [0, 0, 9, 0, 41]
- WL/DD Ratio: inf
- Total Time: 00:09:35

**Conclusion:** Engine is around 2700+ ELO.

---

## Test 2 — vs SF 2700 ELO (8+0.08, 1t, 16MB)

- Elo: -13.90 +/- 65.35
- nElo: -14.68 +/- 68.10
- LOS: 33.64 %
- DrawRatio: 46.00 %
- PairsRatio: 0.80
- Games: 100
- Wins: 45
- Losses: 49
- Draws: 6
- Points: 48.0 (48.00 %)
- Ptnml(0-2): [11, 4, 23, 2, 10]
- WL/DD Ratio: inf
- Total Time: 00:11:21

---

## Test 3 — vs SF 2700 ELO with Opening Book (10+0.1, 1t, 16MB, UHO_4060_v4.epd)

Command:
```
fastchess \
  -engine cmd=/home/Tono/Documents/chess-engine/target/release/reckless name=Reckless \
  -engine cmd=stockfish name=SF option.UCI_LimitStrength=true option.UCI_Elo=2600 \
  -each tc=10+0.1 -rounds 200 -concurrency 16 \
  -openings file=UHO_4060_v4.epd format=epd order=random \
  -pgnout file=results.pgn
```

- Elo: 14.77 +/- 31.23
- nElo: 16.17 +/- 34.05
- LOS: 82.40 %
- DrawRatio: 48.00 %
- PairsRatio: 1.26
- Games: 400
- Wins: 192
- Losses: 175
- Draws: 33
- Points: 208.5 (52.12 %)
- Ptnml(0-2): [36, 10, 96, 17, 41]
- WL/DD Ratio: 31.00
- Total Time: 00:13:37

**Conclusion:** Engine estimated at ~2615 ELO.
