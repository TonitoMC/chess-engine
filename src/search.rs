use std::sync::atomic::Ordering;

use crate::{
    board::NullBoardObserver,
    evaluation::{correct_eval, evaluate},
    movepick::MovePicker,
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

pub fn start(td: &mut ThreadData, report: Report, _thread_count: usize) {
    td.completed_depth = 0;
    td.pv_table.clear(0);
    td.multi_pv = 1;
    td.pv_index = 0;
    td.pv_start = 0;
    td.pv_end = td.root_moves.len();

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

        let score = search::<Root>(td, -Score::INFINITE, Score::INFINITE, depth, 0);

        if td.shared.status.get() == Status::STOPPED {
            break;
        }

        td.root_moves.sort_by_key(|rm| std::cmp::Reverse(rm.score));
        td.completed_depth = depth;

        if report == Report::Full {
            td.print_uci_info(depth);
        }

        let _ = score;

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

fn search<NODE: NodeType>(td: &mut ThreadData, mut alpha: i32, mut beta: i32, depth: i32, ply: isize) -> i32 {
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
            return evaluate(&td.board);
        }

        // Mate distance pruning: no point searching if we can't beat current bounds
        alpha = alpha.max(mated_in(ply));
        beta = beta.min(mate_in(ply + 1));
        if alpha >= beta {
            return alpha;
        }
    }

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

        // TT cutoff in non-PV nodes
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

    // Static eval (used for correction history bookkeeping)
    let raw_eval;
    let correction_value = eval_correction(td, ply);

    if td.board.in_check() {
        raw_eval = Score::NONE;
    } else if let Some(entry) = &entry {
        raw_eval = if is_valid(entry.raw_eval) { entry.raw_eval } else { evaluate(&td.board) };
    } else {
        raw_eval = evaluate(&td.board);
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

    // Reverse Futility Pruning: if static eval beats beta by a large enough
    // margin, we're unlikely to fall below beta after any move, so prune.
    if !NODE::PV
        && !td.board.in_check()
        && depth <= 8
        && eval - 75 * depth >= beta
    {
        return eval;
    }

    // Null Move Pruning: give the opponent a free move. If they still can't beat
    // beta, our position is good enough to prune without searching further.
    if !NODE::PV
        && !td.board.in_check()
        && depth >= 3
        && eval >= beta
        && td.board.plies_from_null() > 0
        && td.board.has_non_pawn_material(td.board.side_to_move())
    {
        let reduction = 3 + depth / 3;
        td.board.make_null_move();
        let null_score = -search::<NonPV>(td, -beta, -beta + 1, depth - reduction, ply + 1);
        td.board.undo_null_move();

        if null_score >= beta {
            return beta;
        }
    }

    // Singular Extensions: if the TT move looks way better than all alternatives,
    // extend its search by 1 ply. We verify this by searching all other moves at
    // reduced depth with a window just below the TT score. If nothing beats it, extend.
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

    let mut quiet_moves = crate::types::ArrayVec::<Move, 32>::new();
    let mut noisy_moves = crate::types::ArrayVec::<Move, 32>::new();

    // LMP thresholds: after trying this many quiet moves at low depth, skip the rest
    let lmp_threshold = [0, 8, 12, 16, 20];

    let mut move_picker = MovePicker::new(tt_move);

    while let Some(mv) = move_picker.next::<NODE>(td, false, ply) {
        if NODE::ROOT && !td.root_moves[td.pv_start..td.pv_end].iter().any(|rm| rm.mv == mv) {
            continue;
        }

        // Skip the excluded move during singular search
        if mv == td.stack[ply].excluded {
            continue;
        }

        move_count += 1;
        td.stack[ply].move_count = move_count;

        let initial_nodes = td.nodes();
        let is_quiet = !mv.is_noisy();

        // Late Move Pruning: at low depths, skip quiet moves beyond the threshold
        if !NODE::PV
            && is_quiet
            && depth <= 4
            && move_count > lmp_threshold[depth as usize]
        {
            continue;
        }

        let extension = if mv == tt_move && singular_extension { 1 } else { 0 };

        make_move(td, ply, mv);

        let score = if NODE::PV && move_count == 1 {
            -search::<PV>(td, -beta, -alpha, depth - 1 + extension, ply + 1)
        } else {
            // Late Move Reductions: moves tried late are likely bad, so search
            // them at reduced depth. If the reduced search beats alpha anyway,
            // re-search at full depth to confirm.
            let reduction = if depth >= 3 && move_count > 3 && is_quiet {
                let r = (depth as f32).ln() * (move_count as f32).ln() / 2.0;
                (r as i32).clamp(1, depth - 1)
            } else {
                0
            };

            let s = -search::<NonPV>(td, -alpha - 1, -alpha, depth - 1 + extension - reduction, ply + 1);

            // Re-search at full depth if the reduced search beat alpha
            let s = if s > alpha && reduction > 0 {
                -search::<NonPV>(td, -alpha - 1, -alpha, depth - 1 + extension, ply + 1)
            } else {
                s
            };

            if s > alpha && NODE::PV {
                -search::<PV>(td, -beta, -alpha, depth - 1 + extension, ply + 1)
            } else {
                s
            }
        };

        undo_move(td, mv);

        if td.shared.status.get() == Status::STOPPED {
            return Score::ZERO;
        }

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
                    bound = Bound::Lower;
                    break;
                }

                alpha = score;
            }
        }

        if mv != best_move && move_count <= 32 {
            if is_quiet { quiet_moves.push(mv); } else { noisy_moves.push(mv); }
        }
    }

    if move_count == 0 {
        return if td.board.in_check() { mated_in(ply) } else { draw(td) };
    }

    if best_move.is_present() {
        let stm = td.board.side_to_move();
        let quiet_bonus = (185 * depth).min(1648);
        let quiet_malus = (162 * depth).min(1198);
        let cont_bonus  = (107 * depth).min(1051);
        let cont_malus  = (399 * depth).min(933);
        let noisy_bonus = (89  * depth).min(748);
        let noisy_malus = (179 * depth).min(1391);

        if best_move.is_noisy() {
            td.noisy_history.update(td.board.all_threats(), td.board.moved_piece(best_move), best_move.to(), td.board.type_on(best_move.to()), noisy_bonus);
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
            td.noisy_history.update(td.board.all_threats(), td.board.moved_piece(mv), mv.to(), td.board.type_on(mv.to()), -noisy_malus);
        }
    }

    if !(NODE::ROOT && td.pv_index > 0) {
        td.shared.tt.write(hash, depth, raw_eval, best_score, bound, best_move, ply, tt_pv, NODE::PV);
    }

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
        return evaluate(&td.board);
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
        raw_eval = Score::NONE;
        best_score = -Score::INFINITE;
    } else {
        raw_eval = match &entry {
            Some(e) if is_valid(e.raw_eval) => e.raw_eval,
            _ => evaluate(&td.board),
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

    if in_check && move_count == 0 {
        return mated_in(ply);
    }

    let bound = if best_score >= beta { Bound::Lower } else { Bound::Upper };
    td.shared.tt.write(hash, TtDepth::SOME, raw_eval, best_score, bound, best_move, ply, tt_pv, false);

    best_score
}

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
    td.board.make_move(mv, &mut NullBoardObserver);
    td.shared.tt.prefetch(td.board.hash());
}

fn undo_move(td: &mut ThreadData, mv: Move) {
    td.board.undo_move(mv);
}
