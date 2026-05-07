use crate::{
    board::Board,
    types::{Color, PieceType, Square},
};

// Classic centipawn material values
const PAWN_VALUE:   i32 = 100;
const KNIGHT_VALUE: i32 = 320;
const BISHOP_VALUE: i32 = 330;
const ROOK_VALUE:   i32 = 500;
const QUEEN_VALUE:  i32 = 900;

// Piece-square tables (white's perspective, a1=index 0, h8=index 63)
// Source: Tomasz Michniewski's Simplified Evaluation Function
#[rustfmt::skip]
const PAWN_PST: [i32; 64] = [
     0,  0,  0,  0,  0,  0,  0,  0,
    50, 50, 50, 50, 50, 50, 50, 50,
    10, 10, 20, 30, 30, 20, 10, 10,
     5,  5, 10, 25, 25, 10,  5,  5,
     0,  0,  0, 20, 20,  0,  0,  0,
     5, -5,-10,  0,  0,-10, -5,  5,
     5, 10, 10,-20,-20, 10, 10,  5,
     0,  0,  0,  0,  0,  0,  0,  0,
];

#[rustfmt::skip]
const KNIGHT_PST: [i32; 64] = [
    -50,-40,-30,-30,-30,-30,-40,-50,
    -40,-20,  0,  0,  0,  0,-20,-40,
    -30,  0, 10, 15, 15, 10,  0,-30,
    -30,  5, 15, 20, 20, 15,  5,-30,
    -30,  0, 15, 20, 20, 15,  0,-30,
    -30,  5, 10, 15, 15, 10,  5,-30,
    -40,-20,  0,  5,  5,  0,-20,-40,
    -50,-40,-30,-30,-30,-30,-40,-50,
];

#[rustfmt::skip]
const BISHOP_PST: [i32; 64] = [
    -20,-10,-10,-10,-10,-10,-10,-20,
    -10,  0,  0,  0,  0,  0,  0,-10,
    -10,  0,  5, 10, 10,  5,  0,-10,
    -10,  5,  5, 10, 10,  5,  5,-10,
    -10,  0, 10, 10, 10, 10,  0,-10,
    -10, 10, 10, 10, 10, 10, 10,-10,
    -10,  5,  0,  0,  0,  0,  5,-10,
    -20,-10,-10,-10,-10,-10,-10,-20,
];

#[rustfmt::skip]
const ROOK_PST: [i32; 64] = [
     0,  0,  0,  0,  0,  0,  0,  0,
     5, 10, 10, 10, 10, 10, 10,  5,
    -5,  0,  0,  0,  0,  0,  0, -5,
    -5,  0,  0,  0,  0,  0,  0, -5,
    -5,  0,  0,  0,  0,  0,  0, -5,
    -5,  0,  0,  0,  0,  0,  0, -5,
    -5,  0,  0,  0,  0,  0,  0, -5,
     0,  0,  0,  5,  5,  0,  0,  0,
];

#[rustfmt::skip]
const QUEEN_PST: [i32; 64] = [
    -20,-10,-10, -5, -5,-10,-10,-20,
    -10,  0,  0,  0,  0,  0,  0,-10,
    -10,  0,  5,  5,  5,  5,  0,-10,
     -5,  0,  5,  5,  5,  5,  0, -5,
      0,  0,  5,  5,  5,  5,  0, -5,
    -10,  5,  5,  5,  5,  5,  0,-10,
    -10,  0,  5,  0,  0,  0,  0,-10,
    -20,-10,-10, -5, -5,-10,-10,-20,
];

#[rustfmt::skip]
const KING_PST: [i32; 64] = [
    -30,-40,-40,-50,-50,-40,-40,-30,
    -30,-40,-40,-50,-50,-40,-40,-30,
    -30,-40,-40,-50,-50,-40,-40,-30,
    -30,-40,-40,-50,-50,-40,-40,-30,
    -20,-30,-30,-40,-40,-30,-30,-20,
    -10,-20,-20,-20,-20,-20,-20,-10,
     20, 20,  0,  0,  0,  0, 20, 20,
     20, 30, 10,  0,  0, 10, 30, 20,
];

fn pst_index(sq: Square, color: Color) -> usize {
    let rank = sq.rank() as usize;
    let file = sq.file() as usize;
    // PSTs are stored from rank 8 down to rank 1 (black's view flipped for white)
    let rank = if color == Color::White { 7 - rank } else { rank };
    rank * 8 + file
}

fn score_for(board: &Board, color: Color) -> i32 {
    let mut score = 0;

    for sq in board.colored_pieces(color, PieceType::Pawn) {
        score += PAWN_VALUE + PAWN_PST[pst_index(sq, color)];
    }
    for sq in board.colored_pieces(color, PieceType::Knight) {
        score += KNIGHT_VALUE + KNIGHT_PST[pst_index(sq, color)];
    }
    for sq in board.colored_pieces(color, PieceType::Bishop) {
        score += BISHOP_VALUE + BISHOP_PST[pst_index(sq, color)];
    }
    for sq in board.colored_pieces(color, PieceType::Rook) {
        score += ROOK_VALUE + ROOK_PST[pst_index(sq, color)];
    }
    for sq in board.colored_pieces(color, PieceType::Queen) {
        score += QUEEN_VALUE + QUEEN_PST[pst_index(sq, color)];
    }
    for sq in board.colored_pieces(color, PieceType::King) {
        score += KING_PST[pst_index(sq, color)];
    }

    score
}

/// Returns evaluation in centipawns from the side to move's perspective.
pub fn evaluate(board: &Board) -> i32 {
    let white = score_for(board, Color::White);
    let black = score_for(board, Color::Black);
    let score = white - black;
    if board.side_to_move() == Color::White { score } else { -score }
}

/// Applies a small correction to the raw eval based on position history.
/// You can ignore this — it's a lightweight learned adjustment on top of your eval.
pub fn correct_eval(raw_eval: i32, correction_value: i32, halfmove_clock: u8) -> i32 {
    let mut eval = raw_eval;
    eval = eval * (200 - halfmove_clock as i32) / 200;
    eval += correction_value;
    eval
}
