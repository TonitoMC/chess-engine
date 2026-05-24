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
                beta = (alpha + beta) / 2;
                alpha = (score - delta).max(-Score::INFINITE);
                delta = delta / 2 + 10;
            } else if score >= beta {
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

    // Internal Iterative Reduction: without a TT move the search is poorly ordered,
    // so shave a ply rather than searching deeply on bad move ordering.
    if depth >= 4 && !tt_move.is_present() {
        depth -= 1;
    }

    // Static eval (used for correction history bookkeeping)
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

    // Improving: true when our eval is better than it was 2 plies ago.
    // Used to tighten or loosen pruning thresholds accordingly.
    let improving = !td.board.in_check()
        && ply >= 2
        && is_valid(td.stack[ply - 2].eval)
        && eval > td.stack[ply - 2].eval;

    // Reverse Futility Pruning
    if !NODE::PV
        && !td.board.in_check()
        && depth <= 8
        && eval - 75 * (depth - improving as i32) >= beta
    {
        return eval;
    }

    // Null Move Pruning
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

    // Razoring: if eval is far below alpha at low depth, a qsearch is likely
    // sufficient — a full search won't recover enough to matter.
    if !NODE::PV
        && !td.board.in_check()
        && depth <= 4
        && is_valid(eval)
        && eval + 220 * depth + 135 < alpha
    {
        return qsearch::<NODE>(td, alpha, beta, ply);
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
    let mut quiets_searched: i32 = 0;

    let mut move_picker = MovePicker::new(tt_move);

    while let Some(mv) = move_picker.next::<NODE>(td, false, ply) {
        if NODE::ROOT && !td.root_moves[td.pv_start..td.pv_end].iter().any(|rm| rm.mv == mv) {
            continue;
        }

        if mv == td.stack[ply].excluded {
            continue;
        }

        move_count += 1;
        td.stack[ply].move_count = move_count;

        let initial_nodes = td.nodes();
        let is_quiet = !mv.is_noisy();

        let is_bad_noisy = !is_quiet && move_picker.stage() == Stage::BadNoisy;

        if !NODE::PV && best_score > -Score::INFINITE {
            if is_quiet {
                // Late Move Pruning: quadratic threshold scaled by improving.
                if depth <= 4
                    && quiets_searched > 3 + depth * depth / (1 + !improving as i32)
                {
                    continue;
                }

                // Futility Pruning: skip quiet moves when static eval is far below alpha.
                if !td.board.in_check()
                    && depth <= 5
                    && is_valid(eval)
                    && eval + 130 * depth + 45 < alpha
                {
                    continue;
                }
            }

            // SEE pruning: skip bad captures at low depth.
            if is_bad_noisy && depth <= 6 {
                continue;
            }
        }

        let quiet_score = if is_quiet {
            td.quiet_history.get(td.board.all_threats(), td.board.side_to_move(), mv)
        } else {
            0
        };

        let singular_ext = mv == tt_move && singular_extension;

        make_move(td, ply, mv);

        // Check extension: extend moves that give check — low branching, forced sequences.
        let gives_check = td.board.in_check();
        let extension = i32::from(singular_ext) + i32::from(gives_check && !singular_ext);

        let score = if NODE::PV && move_count == 1 {
            -search::<PV>(td, -beta, -alpha, depth - 1 + extension, ply + 1)
        } else {
            let reduction = if depth >= 3 && move_count > 2 && is_quiet {
                let r = (depth as f32).ln() * (move_count as f32).ln() / 2.0;
                let mut r = r as i32 - quiet_score / 8192;
                r -= NODE::PV as i32;
                r -= gives_check as i32;
                r.clamp(0, depth - 1)
            } else {
                0
            };

            let s = -search::<NonPV>(td, -alpha - 1, -alpha, depth - 1 + extension - reduction, ply + 1);

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

        if is_quiet {
            quiets_searched += 1;
        }

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
    td.network.push();
    td.board.make_move(mv, &mut td.network);
    td.shared.tt.prefetch(td.board.hash());
}

fn undo_move(td: &mut ThreadData, mv: Move) {
    td.network.pop();
    td.board.undo_move(mv);
}
