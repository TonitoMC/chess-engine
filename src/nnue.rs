use crate::{
    board::Board,
    types::{Color, Piece, Square},
};

const HIDDEN_SIZE: usize = 384;
const NUM_OUTPUT_BUCKETS: usize = 4;
const SCALE: i32 = 400;
const QA: i16 = 255;
const QB: i16 = 64;

// (piece_count - 2) / 8, clamped to [0, NUM_OUTPUT_BUCKETS - 1]
fn output_bucket(piece_count: usize) -> usize {
    ((piece_count.saturating_sub(2)) / 8).min(NUM_OUTPUT_BUCKETS - 1)
}

#[inline]
fn screlu(x: i16) -> i32 {
    let y = i32::from(x).clamp(0, i32::from(QA));
    y * y
}

/// Chess768 feature index from the perspective of `pov`.
/// Mirrors the square for Black's perspective.
pub fn feature_index(piece: Piece, sq: Square, pov: Color) -> usize {
    let (color, piece_type, square) = if pov == Color::White {
        (piece.color() as usize, piece.piece_type() as usize, sq as usize)
    } else {
        (
            (piece.color() as usize) ^ 1,
            piece.piece_type() as usize,
            sq as usize ^ 56, // vertical flip
        )
    };
    color * 64 * 6 + piece_type * 64 + square
}

#[derive(Clone, Copy)]
#[repr(C, align(64))]
pub struct Accumulator {
    vals: [i16; HIDDEN_SIZE],
}

impl Accumulator {
    fn new(net: &Network) -> Self {
        net.feature_bias
    }

    pub fn add_feature(&mut self, feature_idx: usize, net: &Network) {
        for (a, &w) in self.vals.iter_mut().zip(&net.feature_weights[feature_idx].vals) {
            *a += w;
        }
    }

    pub fn remove_feature(&mut self, feature_idx: usize, net: &Network) {
        for (a, &w) in self.vals.iter_mut().zip(&net.feature_weights[feature_idx].vals) {
            *a -= w;
        }
    }
}

#[repr(C)]
pub struct Network {
    /// [768][HIDDEN_SIZE] column-major, quantized QA
    feature_weights: [Accumulator; 768],
    /// [HIDDEN_SIZE], quantized QA
    feature_bias: Accumulator,
    /// [NUM_OUTPUT_BUCKETS][2 * HIDDEN_SIZE], quantized QB (transposed for fast inference)
    output_weights: [[i16; 2 * HIDDEN_SIZE]; NUM_OUTPUT_BUCKETS],
    /// [NUM_OUTPUT_BUCKETS], quantized QA * QB
    output_bias: [i16; NUM_OUTPUT_BUCKETS],
}

impl Network {
    fn evaluate_bucket(&self, us: &Accumulator, them: &Accumulator, bucket: usize) -> i32 {
        let weights = &self.output_weights[bucket];
        let mut output = 0i32;

        for (&val, &w) in us.vals.iter().zip(&weights[..HIDDEN_SIZE]) {
            output += screlu(val) * i32::from(w);
        }
        for (&val, &w) in them.vals.iter().zip(&weights[HIDDEN_SIZE..]) {
            output += screlu(val) * i32::from(w);
        }

        output /= i32::from(QA);
        output += i32::from(self.output_bias[bucket]);
        output *= SCALE;
        output /= i32::from(QA) * i32::from(QB);
        output
    }
}

static NETWORK: Network =
    unsafe { std::mem::transmute(*include_bytes!(env!("NETWORK_FILE"))) };

/// Stack-based accumulator pair for incremental updates during search.
pub struct NnueState {
    stack: Vec<[Accumulator; 2]>,
    head: usize,
}

impl NnueState {
    pub fn new(board: &Board) -> Self {
        let net = &NETWORK;
        let acc = [Accumulator::new(net), Accumulator::new(net)];
        let mut state = Self { stack: vec![acc; 512], head: 0 };
        state.full_refresh(board);
        state
    }

    fn full_refresh(&mut self, board: &Board) {
        let net = &NETWORK;
        let accs = &mut self.stack[self.head];

        for pov in [Color::White, Color::Black] {
            accs[pov as usize] = Accumulator::new(net);
            for piece in Piece::ALL {
                for sq in board.colored_pieces(piece.color(), piece.piece_type()) {
                    let idx = feature_index(piece, sq, pov);
                    accs[pov as usize].add_feature(idx, net);
                }
            }
        }
    }

    /// Call before making a move.
    pub fn push(&mut self) {
        let prev = self.stack[self.head];
        self.head += 1;
        self.stack[self.head] = prev;
    }

    /// Call after unmaking a move.
    pub fn pop(&mut self) {
        self.head -= 1;
    }

    pub fn add_feature(&mut self, piece: Piece, sq: Square) {
        let net = &NETWORK;
        let accs = &mut self.stack[self.head];
        for pov in [Color::White, Color::Black] {
            let idx = feature_index(piece, sq, pov);
            accs[pov as usize].add_feature(idx, net);
        }
    }

    pub fn remove_feature(&mut self, piece: Piece, sq: Square) {
        let net = &NETWORK;
        let accs = &mut self.stack[self.head];
        for pov in [Color::White, Color::Black] {
            let idx = feature_index(piece, sq, pov);
            accs[pov as usize].remove_feature(idx, net);
        }
    }

    /// Evaluate from the side to move's perspective.
    pub fn evaluate(&self, stm: Color, piece_count: usize) -> i32 {
        let net = &NETWORK;
        let accs = &self.stack[self.head];
        let us = &accs[stm as usize];
        let them = &accs[(!stm) as usize];
        let bucket = output_bucket(piece_count);
        net.evaluate_bucket(us, them, bucket)
    }
}
