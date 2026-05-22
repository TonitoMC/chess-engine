use crate::{
    board::{Board, BoardObserver},
    types::{Color, Piece, PieceType, Square},
};

mod simd;

const INPUT_SIZE: usize = 768;
const HIDDEN_SIZE: usize = 384;
const OUTPUT_BUCKETS: usize = 4;

const EVAL_SCALE: i32 = 400;
const L0_SCALE: i32 = 256;
const L1_SCALE: i32 = 64;

const MATE_BOUND: i32 = 31000;

#[derive(Clone)]
pub struct Network {
    accumulators: [[i16; HIDDEN_SIZE]; 2],
    stack: Vec<[[i16; HIDDEN_SIZE]; 2]>,
}

impl Network {
    pub fn push(&mut self) {
        self.stack.push(self.accumulators);
    }

    pub fn pop(&mut self) {
        self.accumulators = self.stack.pop().unwrap();
    }

    pub fn evaluate(&self, side_to_move: Color, piece_count: usize) -> i32 {
        let bucket = ((piece_count.min(32) - 2) * OUTPUT_BUCKETS / (32 - 2 + 1)).min(OUTPUT_BUCKETS - 1);
        let stm = self.accumulators[side_to_move];
        let nstm = self.accumulators[!side_to_move];
        let weights = &PARAMETERS.output_weights[bucket];

        let output = simd::forward(&stm, &weights[..HIDDEN_SIZE]) + simd::forward(&nstm, &weights[HIDDEN_SIZE..]);
        let score = (output / L0_SCALE + i32::from(PARAMETERS.output_bias[bucket])) * EVAL_SCALE / (L0_SCALE * L1_SCALE);

        score.clamp(-MATE_BOUND, MATE_BOUND)
    }

    fn activate(&mut self, piece: Piece, square: Square) {
        let (white, black) = index(piece, square);
        for i in 0..HIDDEN_SIZE {
            self.accumulators[0][i] += PARAMETERS.input_weights[white][i];
            self.accumulators[1][i] += PARAMETERS.input_weights[black][i];
        }
    }

    fn deactivate(&mut self, piece: Piece, square: Square) {
        let (white, black) = index(piece, square);
        for i in 0..HIDDEN_SIZE {
            self.accumulators[0][i] -= PARAMETERS.input_weights[white][i];
            self.accumulators[1][i] -= PARAMETERS.input_weights[black][i];
        }
    }

    pub fn full_refresh(&mut self, board: &Board) {
        self.accumulators = [PARAMETERS.input_bias; 2];
        for color in [Color::White, Color::Black] {
            for pt in [PieceType::Pawn, PieceType::Knight, PieceType::Bishop, PieceType::Rook, PieceType::Queen, PieceType::King] {
                let piece = Piece::new(color, pt);
                for sq in board.colored_pieces(color, pt) {
                    self.activate(piece, sq);
                }
            }
        }
    }
}

impl Default for Network {
    fn default() -> Self {
        Self {
            accumulators: [PARAMETERS.input_bias; 2],
            stack: Vec::default(),
        }
    }
}

impl BoardObserver for Network {
    fn on_piece_change(&mut self, _: &Board, piece: Piece, sq: Square, add: bool) {
        if add {
            self.activate(piece, sq);
        } else {
            self.deactivate(piece, sq);
        }
    }

    fn on_piece_move(&mut self, _: &Board, piece: Piece, from: Square, to: Square) {
        self.deactivate(piece, from);
        self.activate(piece, to);
    }

    fn on_piece_mutate(&mut self, _: &Board, old_piece: Piece, new_piece: Piece, sq: Square) {
        self.deactivate(old_piece, sq);
        self.activate(new_piece, sq);
    }
}

fn index(piece: Piece, square: Square) -> (usize, usize) {
    let color = piece.color() as usize;
    let pt = piece.piece_type() as usize;
    (
        384 * color + 64 * pt + square as usize,
        384 * (1 - color) + 64 * pt + (square as usize ^ 56),
    )
}

#[repr(C)]
struct Parameters {
    input_weights: [[i16; HIDDEN_SIZE]; INPUT_SIZE],
    input_bias: [i16; HIDDEN_SIZE],
    output_weights: [[i16; 2 * HIDDEN_SIZE]; OUTPUT_BUCKETS],
    output_bias: [i16; OUTPUT_BUCKETS],
    _padding: [u8; 56],
}

static PARAMETERS: Parameters = unsafe { std::mem::transmute(*include_bytes!("../networks/384net.nnue")) };
