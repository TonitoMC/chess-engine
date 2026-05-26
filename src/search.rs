use std::sync::atomic::Ordering;

use crate::{
    evaluation::correct_eval,
    movepick::{MovePicker, Stage},
    stack::Stack,
    thread::{RootMove, Status, ThreadData},
    time::Limits,
    transposition::{Bound, TtDepth},
    types::{MAX_PLY, Move, Score, draw, is_valid, mate_in, mated_in},
};

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum Report {
    None,
    Minimal,
    Full,
}

// NodeType is a compile-time tag that lets the search specialize behavior for three cases:
//   Root  — the top-level call; tracks per-move node counts and PV lines
//   PV    — any node inside the principal variation; maintains the PV table and uses full windows
//   NonPV — all other nodes; uses null-window searches and can apply aggressive pruning
pub trait NodeType {
    const PV: bool;
    const ROOT: bool;
}

struct Root;
impl NodeType for Root {
    const PV: bool = true;
    const ROOT: bool = true;
}

struct PV;
impl NodeType for PV {
    const PV: bool = true;
    const ROOT: bool = false;
}

struct NonPV;
impl NodeType for NonPV {
    const PV: bool = false;
    const ROOT: bool = false;
}

// Entry point for iterative deepening. Called once per move decision.
// Searches depth 1, 2, 3, … until time runs out or the depth limit is hit.
// Each completed iteration seeds the next via the TT and updates root move scores.
pub fn start(td: &mut ThreadData, report: Report, _thread_count: usize) {
    td.completed_depth = 0;
    td.pv_table.clear(0);
    td.multi_pv = 1;
    td.pv_index = 0;
    td.pv_start = 0;
    td.pv_end = td.root_moves.len();

    if td.root_moves.is_empty() {
        return;
    }

    for depth in 1..MAX_PLY as i32 {
        if td.id == 0
            && let Limits::Depth(maximum) = td.time_manager.limits()
            && depth > maximum
        {
            td.shared.status.set(Status::STOPPED);
            break;
        }

        td.sel_depth = 0;
        td.root_depth = depth;
        td.stack = Stack::new();

        for rm in &mut td.root_moves {
            rm.previous_score = rm.score;
        }

        // Aspiration windows: start with a narrow band around the previous score.
        // If the search result falls outside [alpha, beta], widen and retry.
        // At low depths or with no prior score, use a full infinite window instead.
        let prev = td.root_moves.first().map_or(Score::NONE, |rm| rm.previous_score);

        let (mut alpha, mut beta, mut delta) = if depth > 4 && is_valid(prev) && prev.abs() < 1500 {
            let d = 20;
            (prev - d, prev + d, d)
        } else {
            (-Score::INFINITE, Score::INFINITE, Score::INFINITE)
        };

        let _score = loop {
            let score = search::<Root>(td, alpha, beta, depth, 0);

            if td.shared.status.get() == Status::STOPPED {
                break score;
            }

            if score <= alpha {
                // Fail-low: result came in below our lower bound — widen downward and retry.
                beta = (alpha + beta) / 2;
                alpha = (score - delta).max(-Score::INFINITE);
                delta = delta / 2 + 10;
            } else if score >= beta {
                // Fail-high: result came in above our upper bound — widen upward and retry.
                beta = (score + delta).min(Score::INFINITE);
                delta = delta / 2 + 10;
            } else {
                break score;
            }
        };

        if td.shared.status.get() == Status::STOPPED {
            break;
        }

        td.root_moves.sort_by_key(|rm| std::cmp::Reverse(rm.score));
        td.completed_depth = depth;

        if report == Report::Full {
            td.print_uci_info(depth);
        }

        if td.time_manager.soft_limit(td, || 1.0) {
            td.shared.status.set(Status::STOPPED);
            break;
        }
    }

    if report == Report::Minimal {
        td.print_uci_info(td.completed_depth.max(1));
    }

    if !td.root_moves.is_empty() {
        td.previous_best_score = td.root_moves[0].score;
    }
}

// Core alpha-beta search (negamax formulation with PVS).
//
// Returns the best score achievable in the current position within [alpha, beta].
// alpha = best score the current side is guaranteed regardless of opponent play.
// beta  = best score the opponent is guaranteed; if we exceed it they won't allow this line.
//
// PVS (Principal Variation Search): the first move is searched with a full [alpha, beta] window.
// All subsequent moves are first tried with a null window [-alpha-1, -alpha]. If one of them
// unexpectedly beats alpha, it is re-searched with the full window to get the exact score.
// This avoids the cost of full-window searches for moves that are unlikely to be best.
fn search<NODE: NodeType>(td: &mut ThreadData, mut alpha: i32, mut beta: i32, mut depth: i32, ply: isize) -> i32 {
    debug_assert!(ply as usize <= MAX_PLY);

    if !NODE::ROOT && NODE::PV {
        td.pv_table.clear(ply as usize);
    }

    if td.shared.status.get() == Status::STOPPED {
        return Score::ZERO;
    }

    // Drop into qsearch at depth 0
    if depth <= 0 {
        return qsearch::<NODE>(td, alpha, beta, ply);
    }

    // Check time periodically on the main thread
    if td.id == 0 && td.time_manager.check_time(td) {
        td.shared.status.set(Status::STOPPED);
        return Score::ZERO;
    }

    if !NODE::ROOT {
        if td.board.is_draw(ply) {
            return draw(td);
        }

        if ply as usize >= MAX_PLY - 1 {
            return td.network.evaluate(td.board.side_to_move(), td.board.occupancies().popcount());
        }

        // Mate distance pruning: tighten the window to only beats we can actually achieve
        // given the number of plies already played. If the window collapses, return immediately.
        alpha = alpha.max(mated_in(ply));
        beta = beta.min(mate_in(ply + 1));
        if alpha >= beta {
            return alpha;
        }
    }

    // Transposition table lookup: check if we've already searched this position.
    // The TT stores: the best move found, the score, and whether it was an exact
    // score or only a bound (upper = all moves failed low, lower = caused a cutoff).
    let hash = td.board.hash();
    let entry = td.shared.tt.read(hash, td.board.halfmove_clock(), ply);

    let mut tt_move = Move::NULL;
    let mut tt_score = Score::NONE;
    let mut tt_bound = Bound::None;
    let tt_pv = NODE::PV;

    if let Some(entry) = &entry {
        tt_move = entry.mv;
        tt_score = entry.score;
        tt_bound = entry.bound;

        // TT cutoff: if the stored result was searched at sufficient depth and the bound
        // is compatible with the current window, we can return without re-searching.
        // Only allowed in non-PV nodes to preserve the accuracy of the principal variation.
        if !NODE::PV
            && entry.depth >= depth
            && is_valid(tt_score)
            && match tt_bound {
                Bound::Upper => tt_score <= alpha,
                Bound::Lower => tt_score >= beta,
                _ => true,
            }
        {
            return tt_score;
        }
    }

    // Internal Iterative Reduction: without a TT move the search is poorly ordered,
    // so shave a ply rather than searching deeply on bad move ordering.
    if depth >= 4 && !tt_move.is_present() {
        depth -= 1;
    }

    // Compute static evaluation. This is the NNUE score corrected by history.
    // Correction history tracks how often the raw static eval under- or over-shot
    // the final search result, and adjusts accordingly.
    // When in check, static eval is meaningless, so we leave it as NONE.
    let raw_eval;
    let correction_value = eval_correction(td, ply);

    if td.board.in_check() {
        raw_eval = Score::NONE;
    } else if let Some(entry) = &entry {
        raw_eval = if is_valid(entry.raw_eval) { entry.raw_eval } else { td.network.evaluate(td.board.side_to_move(), td.board.occupancies().popcount()) };
    } else {
        raw_eval = td.network.evaluate(td.board.side_to_move(), td.board.occupancies().popcount());
        td.shared.tt.write(hash, TtDepth::SOME, raw_eval, Score::NONE, Bound::None, Move::NULL, ply, tt_pv, false);
    }

    td.stack[ply].eval = if td.board.in_check() {
        Score::NONE
    } else {
        correct_eval(raw_eval, correction_value, td.board.halfmove_clock())
    };
    td.stack[ply].tt_move = tt_move;
    td.stack[ply].tt_pv = tt_pv;
    td.stack[ply].reduction = 0;
    td.stack[ply].move_count = 0;
    td.stack[ply + 2].cutoff_count = 0;

    let eval = td.stack[ply].eval;

    // Improving: true when our static eval is better than it was 2 plies ago (our last turn).
    // When improving, the position is getting better so we can afford to be more aggressive.
    // When not improving, the position is getting worse so we tighten pruning thresholds.
    let improving = !td.board.in_check()
        && ply >= 2
        && is_valid(td.stack[ply - 2].eval)
        && eval > td.stack[ply - 2].eval;

    // Reverse Futility Pruning (RFP): if our static eval exceeds beta by a large enough
    // margin at low depth, assume the position is so good we'll get a cutoff anyway.
    // "Reverse" because it prunes based on being too good (vs futility which prunes too bad).
    if !NODE::PV
        && !td.board.in_check()
        && depth <= 8
        && eval - 75 * (depth - improving as i32) >= beta
    {
        return eval;
    }

    // Null Move Pruning (NMP): try passing our turn. If the opponent still can't beat beta
    // even with a free move, our position is strong enough to prune this node.
    // Not used when: in check (illegal), no non-pawn material (zugzwang risk), or after a prior null move.
    if !NODE::PV
        && !td.board.in_check()
        && depth >= 3
        && eval >= beta
        && td.board.plies_from_null() > 0
        && td.board.has_non_pawn_material(td.board.side_to_move())
    {
        let reduction = 3 + depth / 3 + ((eval - beta) / 200).clamp(0, 3);
        td.board.make_null_move();
        let null_score = -search::<NonPV>(td, -beta, -beta + 1, depth - reduction, ply + 1);
        td.board.undo_null_move();

        if null_score >= beta {
            return beta;
        }
    }

    // Razoring: if static eval is far below alpha at low depth, the position is likely
    // too bad to recover with a full search — drop directly into qsearch instead.
    if !NODE::PV
        && !td.board.in_check()
        && depth <= 4
        && is_valid(eval)
        && eval + 220 * depth + 135 < alpha
    {
        return qsearch::<NODE>(td, alpha, beta, ply);
    }

    // Singular Extensions: check whether the TT move is "singular" — so much better than
    // all alternatives that it deserves an extra ply of search depth.
    // We verify by searching all moves *except* the TT move at (depth/2) with a window
    // just below the TT score. If nothing exceeds that bar, the TT move is singular.
    let singular_extension = if !NODE::ROOT
        && depth >= 6
        && tt_move.is_present()
        && tt_bound != Bound::Upper
        && is_valid(tt_score)
        && entry.as_ref().map_or(0, |e| e.depth) >= depth - 3
        && td.stack[ply].excluded.is_null()
    {
        let s_beta = tt_score - depth * 2;
        td.stack[ply].excluded = tt_move;
        let s_score = search::<NonPV>(td, s_beta - 1, s_beta, depth / 2, ply);
        td.stack[ply].excluded = Move::NULL;
        s_score < s_beta
    } else {
        false
    };

    let mut best_score = -Score::INFINITE;
    let mut best_move = Move::NULL;
    let mut bound = Bound::Upper;
    let mut move_count = 0;
    let mut quiets_searched: i32 = 0;

    // Track moves tried that did not cause a beta cutoff.
    // Used after the loop to apply history malus (penalty) to non-best moves.
    let mut quiet_moves = crate::types::ArrayVec::<Move, 32>::new();
    let mut noisy_moves = crate::types::ArrayVec::<Move, 32>::new();

    // The move picker yields moves in priority order:
    // TT move → good captures (SEE+) → quiet moves (history-scored) → bad captures (SEE-)
    let mut move_picker = MovePicker::new(tt_move);

    while let Some(mv) = move_picker.next::<NODE>(td, false, ply) {
        // At root, only consider moves in the current PV window (used for multi-PV support).
        if NODE::ROOT && !td.root_moves[td.pv_start..td.pv_end].iter().any(|rm| rm.mv == mv) {
            continue;
        }

        // Skip the move excluded during singular extension probing.
        if mv == td.stack[ply].excluded {
            continue;
        }

        move_count += 1;
        td.stack[ply].move_count = move_count;

        let initial_nodes = td.nodes();
        let is_quiet = !mv.is_noisy();

        let is_bad_noisy = !is_quiet && move_picker.stage() == Stage::BadNoisy;

        // Move pruning: skip moves that are unlikely to be useful.
        // Only applies in non-PV nodes where we already have a decent lower bound.
        if !NODE::PV && best_score > -Score::INFINITE {
            if is_quiet {
                // Late Move Pruning (LMP): after trying enough quiet moves, skip the rest.
                // The threshold is higher when improving (we can afford to search more).
                if depth <= 4
                    && quiets_searched > 3 + depth * depth / (1 + !improving as i32)
                {
                    continue;
                }

                // Futility Pruning: skip quiet moves when static eval is so far below alpha
                // that even an optimistic bonus won't bring the score up to alpha.
                if !td.board.in_check()
                    && depth <= 5
                    && is_valid(eval)
                    && eval + 130 * depth + 45 < alpha
                {
                    continue;
                }
            }

            // SEE pruning: skip bad captures at low depth rather than just deprioritizing them.
            // At low depth the cost of searching them is not worth the rare cases where they matter.
            if is_bad_noisy && depth <= 6 {
                continue;
            }
        }

        // Quiet history score for this move — used to tune the LMR reduction below.
        // Moves the engine has found good in the past get smaller reductions.
        let quiet_score = if is_quiet {
            td.quiet_history.get(td.board.all_threats(), td.board.side_to_move(), mv)
        } else {
            0
        };

        let singular_ext = mv == tt_move && singular_extension;

        make_move(td, ply, mv);

        // Check extension: moves that give check enter forcing sequences with low branching,
        // so we extend them by 1 ply to avoid missing tactics at the horizon.
        let gives_check = td.board.in_check();
        let extension = i32::from(singular_ext) + i32::from(gives_check && !singular_ext);

        let score = if NODE::PV && move_count == 1 {
            // First move in a PV node: search with full [alpha, beta] window.
            -search::<PV>(td, -beta, -alpha, depth - 1 + extension, ply + 1)
        } else {
            // Late Move Reductions (LMR): later moves in the order are less likely to be good.
            // Reduce their search depth proportional to how late they appear and how deep we are.
            // History score and whether the move gives check modulate the reduction.
            let reduction = if depth >= 3 && move_count > 2 && is_quiet {
                let r = (depth as f32).ln() * (move_count as f32).ln() / 2.0;
                let mut r = r as i32 - quiet_score / 8192;
                r -= NODE::PV as i32;
                r -= gives_check as i32;
                r.clamp(0, depth - 1)
            } else {
                0
            };

            // Try with null window and reduced depth first.
            let s = -search::<NonPV>(td, -alpha - 1, -alpha, depth - 1 + extension - reduction, ply + 1);

            // If it beat alpha with a reduction, research at full depth to confirm the result.
            let s = if s > alpha && reduction > 0 {
                -search::<NonPV>(td, -alpha - 1, -alpha, depth - 1 + extension, ply + 1)
            } else {
                s
            };

            // If it still beats alpha in a PV node, research with full window to get exact score.
            if s > alpha && NODE::PV {
                -search::<PV>(td, -beta, -alpha, depth - 1 + extension, ply + 1)
            } else {
                s
            }
        };

        if is_quiet {
            quiets_searched += 1;
        }

        undo_move(td, mv);

        if td.shared.status.get() == Status::STOPPED {
            return Score::ZERO;
        }

        // At root, track per-move node counts and update the root move table.
        if NODE::ROOT {
            let current_nodes = td.nodes();
            let rm = td.root_moves.iter_mut().find(|v| v.mv == mv).unwrap();
            rm.nodes += current_nodes - initial_nodes;

            if move_count == 1 || score > alpha {
                rm.score = score;
                rm.display_score = score;
                rm.upperbound = score <= alpha;
                rm.lowerbound = score >= beta;
                rm.sel_depth = td.sel_depth;
                rm.pv.commit_full_root_pv(&td.pv_table, 1);
            } else {
                rm.score = -Score::INFINITE;
            }
        }

        if score > best_score {
            best_score = score;

            if score > alpha {
                bound = Bound::Exact;
                best_move = mv;

                if NODE::PV && !NODE::ROOT {
                    td.pv_table.update(ply as usize, mv);
                }

                if score >= beta {
                    // Beta cutoff: this move is too good — the opponent won't allow this line.
                    // Record as a lower bound and break; remaining moves don't need to be searched.
                    bound = Bound::Lower;
                    break;
                }

                alpha = score;
            }
        }

        if mv != best_move && move_count <= 32 {
            if is_quiet {
                quiet_moves.push(mv);
            } else {
                noisy_moves.push(mv);
            }
        }
    }

    // No legal moves: either checkmate or stalemate.
    if move_count == 0 {
        return if td.board.in_check() { mated_in(ply) } else { draw(td) };
    }

    // History update: reward the move that caused a beta cutoff, penalize the moves that didn't.
    // This teaches the engine to try similar moves earlier in future searches.
    // The bonus/malus scale linearly with depth; earlier moves in the tried list get full malus,
    // later moves get a scaled-down penalty (they had less chance to cause a cutoff).
    if best_move.is_present() {
        let stm = td.board.side_to_move();
        let quiet_bonus = (185 * depth).min(1648);
        let quiet_malus = (162 * depth).min(1198);
        let cont_bonus = (107 * depth).min(1051);
        let cont_malus = (399 * depth).min(933);
        let noisy_bonus = (89 * depth).min(748);
        let noisy_malus = (179 * depth).min(1391);

        if best_move.is_noisy() {
            td.noisy_history.update(
                td.board.all_threats(),
                td.board.moved_piece(best_move),
                best_move.to(),
                td.board.type_on(best_move.to()),
                noisy_bonus,
            );
        } else {
            td.quiet_history.update(td.board.all_threats(), stm, best_move, quiet_bonus);
            update_continuation_histories(td, ply, td.board.moved_piece(best_move), best_move.to(), cont_bonus);

            for (i, &mv) in quiet_moves.iter().enumerate() {
                let scale = 1024_i32 / (1 + i as i32);
                td.quiet_history.update(td.board.all_threats(), stm, mv, -quiet_malus * scale / 1024);
                update_continuation_histories(td, ply, td.board.moved_piece(mv), mv.to(), -cont_malus * scale / 1024);
            }
        }

        for &mv in noisy_moves.iter() {
            td.noisy_history.update(
                td.board.all_threats(),
                td.board.moved_piece(mv),
                mv.to(),
                td.board.type_on(mv.to()),
                -noisy_malus,
            );
        }
    }

    // Store result in the transposition table for future lookups.
    if !(NODE::ROOT && td.pv_index > 0) {
        td.shared.tt.write(hash, depth, raw_eval, best_score, bound, best_move, ply, tt_pv, NODE::PV);
    }

    // Update correction history: track how far the static eval deviated from the search result.
    // This lets future calls to eval_correction() adjust the raw NNUE score more accurately.
    if !td.board.in_check() && is_valid(raw_eval) {
        let eval = correct_eval(raw_eval, correction_value, td.board.halfmove_clock());
        let skip_update = best_move.is_noisy()
            || (bound == Bound::Upper && best_score >= eval)
            || (bound == Bound::Lower && best_score <= eval);
        if !skip_update {
            update_correction_histories(td, depth, best_score - eval, ply);
        }
    }

    best_score
}

// Quiescence search: called when depth reaches 0.
// Continues searching captures (and checks when in check) until the position is "quiet".
// This prevents the horizon effect — mistakenly evaluating a position mid-capture sequence.
//
// Stand-pat: the static eval is used as a lower bound. If it already exceeds beta, we assume
// we can stop early. This reflects the assumption that we're not forced to capture.
fn qsearch<NODE: NodeType>(td: &mut ThreadData, mut alpha: i32, beta: i32, ply: isize) -> i32 {
    debug_assert!(!NODE::ROOT);

    if NODE::PV {
        td.pv_table.clear(ply as usize);
        td.sel_depth = td.sel_depth.max(ply as i32);
    }

    if td.id == 0 && td.time_manager.check_time(td) {
        td.shared.status.set(Status::STOPPED);
        return Score::ZERO;
    }

    if td.board.is_draw(ply) {
        return draw(td);
    }

    if ply as usize >= MAX_PLY - 1 {
        return td.network.evaluate(td.board.side_to_move(), td.board.occupancies().popcount());
    }

    let hash = td.board.hash();
    let entry = td.shared.tt.read(hash, td.board.halfmove_clock(), ply);
    let tt_pv = NODE::PV;

    if let Some(entry) = &entry {
        let tt_score = entry.score;
        let tt_bound = entry.bound;

        if is_valid(tt_score)
            && match tt_bound {
                Bound::Upper => tt_score <= alpha,
                Bound::Lower => tt_score >= beta,
                _ => true,
            }
        {
            return tt_score;
        }
    }

    let in_check = td.board.in_check();

    let raw_eval;
    let mut best_score;

    if in_check {
        // When in check, stand-pat is not available — we must search all evasions.
        raw_eval = Score::NONE;
        best_score = -Score::INFINITE;
    } else {
        raw_eval = match &entry {
            Some(e) if is_valid(e.raw_eval) => e.raw_eval,
            _ => td.network.evaluate(td.board.side_to_move(), td.board.occupancies().popcount()),
        };
        best_score = raw_eval;

        // Stand pat: if static eval already beats beta, return early
        if best_score >= beta {
            return best_score;
        }
        if best_score > alpha {
            alpha = best_score;
        }
    }

    let mut best_move = Move::NULL;
    let mut move_count = 0;
    // In qsearch the move picker only yields captures (and all moves when in check).
    let mut move_picker = MovePicker::new_qsearch();

    while let Some(mv) = move_picker.next::<NODE>(td, !in_check, ply) {
        move_count += 1;
        make_move(td, ply, mv);
        let score = -qsearch::<NODE>(td, -beta, -alpha, ply + 1);
        undo_move(td, mv);

        if td.shared.status.get() == Status::STOPPED {
            return Score::ZERO;
        }

        if score > best_score {
            best_score = score;
            if score > alpha {
                best_move = mv;
                if NODE::PV {
                    td.pv_table.update(ply as usize, mv);
                }
                if score >= beta {
                    break;
                }
                alpha = score;
            }
        }
    }

    // No legal moves while in check means checkmate.
    if in_check && move_count == 0 {
        return mated_in(ply);
    }

    let bound = if best_score >= beta { Bound::Lower } else { Bound::Upper };
    td.shared.tt.write(hash, TtDepth::SOME, raw_eval, best_score, bound, best_move, ply, tt_pv, false);

    best_score
}

// Correction history: adjusts the raw NNUE score based on observed prediction errors.
// Indexed by pawn structure, white non-pawn material, and black non-pawn material —
// these are proxies for position type that tend to be where static eval is consistently off.
fn eval_correction(td: &ThreadData, ply: isize) -> i32 {
    let stm = td.board.side_to_move();
    let bucket = td.board.halfmove_clock_bucket();
    let corrhist = td.corrhist();

    (corrhist.pawn.get(stm, td.board.pawn_key(), bucket)
        + corrhist.non_pawn[crate::types::Color::White].get(stm, td.board.non_pawn_key(crate::types::Color::White), bucket)
        + corrhist.non_pawn[crate::types::Color::Black].get(stm, td.board.non_pawn_key(crate::types::Color::Black), bucket))
        / 73
}

fn update_correction_histories(td: &mut ThreadData, depth: i32, diff: i32, ply: isize) {
    let stm = td.board.side_to_move();
    let bucket = td.board.halfmove_clock_bucket();
    let corrhist = td.corrhist();
    let bonus = (142 * depth * diff / 128).clamp(-4771, 3001);

    corrhist.pawn.update(stm, td.board.pawn_key(), bucket, bonus);
    corrhist.non_pawn[crate::types::Color::White].update(stm, td.board.non_pawn_key(crate::types::Color::White), bucket, bonus);
    corrhist.non_pawn[crate::types::Color::Black].update(stm, td.board.non_pawn_key(crate::types::Color::Black), bucket, bonus);
}

// Continuation history: history indexed by the move made at a prior ply.
// Looks back 1, 2, 4, and 6 plies — these offsets capture: the immediate reply,
// the move that set up this position, and longer-range patterns.
fn update_continuation_histories(td: &mut ThreadData, ply: isize, piece: crate::types::Piece, sq: crate::types::Square, bonus: i32) {
    for offset in [1isize, 2, 4, 6] {
        if ply >= offset {
            let entry = &td.stack[ply - offset];
            if entry.mv.is_present() {
                td.continuation_history.update(entry.conthist, piece, sq, bonus);
            }
        }
    }
}

fn make_move(td: &mut ThreadData, ply: isize, mv: Move) {
    td.stack[ply].mv = mv;
    td.stack[ply].piece = td.board.moved_piece(mv);
    td.stack[ply].conthist =
        td.continuation_history.subtable_ptr(td.board.in_check(), mv.is_noisy(), td.board.moved_piece(mv), mv.to());
    td.stack[ply].contcorrhist =
        td.continuation_corrhist.subtable_ptr(td.board.in_check(), mv.is_noisy(), td.board.moved_piece(mv), mv.to());

    td.shared.nodes.increment(td.id);
    td.network.push();
    td.board.make_move(mv, &mut td.network);
    td.shared.tt.prefetch(td.board.hash()); // Prefetch TT entry for the new position while the CPU does other work
}

fn undo_move(td: &mut ThreadData, mv: Move) {
    td.network.pop();
    td.board.undo_move(mv);
}
