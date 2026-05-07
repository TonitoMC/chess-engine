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

    let mut best_score = -Score::INFINITE;
    let mut best_move = Move::NULL;
    let mut bound = Bound::Upper;
    let mut move_count = 0;

    let mut move_picker = MovePicker::new(tt_move);

    while let Some(mv) = move_picker.next::<NODE>(td, false, ply) {
        if NODE::ROOT && !td.root_moves[td.pv_start..td.pv_end].iter().any(|rm| rm.mv == mv) {
            continue;
        }

        move_count += 1;
        td.stack[ply].move_count = move_count;

        let initial_nodes = td.nodes();
        make_move(td, ply, mv);

        let score = if NODE::PV && move_count == 1 {
            -search::<PV>(td, -beta, -alpha, depth - 1, ply + 1)
        } else {
            let s = -search::<NonPV>(td, -alpha - 1, -alpha, depth - 1, ply + 1);
            if s > alpha && NODE::PV {
                -search::<PV>(td, -beta, -alpha, depth - 1, ply + 1)
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
    }

    if move_count == 0 {
        return if td.board.in_check() { mated_in(ply) } else { draw(td) };
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
