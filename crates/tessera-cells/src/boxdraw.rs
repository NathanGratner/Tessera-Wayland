//! Box-drawing characters drawn as geometry rather than glyphs.
//!
//! Fonts round the ends of these strokes and rarely line up between cells, so
//! terminals like foot and kitty draw them by hand. Tessera does the same: lines
//! are built from the cell's own width and height, so they always join.

/// Stroke weight of one arm of a box-drawing character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weight {
    /// No line in this direction.
    None,
    /// A single thin line, e.g. `─`.
    Light,
    /// A single thick line, e.g. `━`.
    Heavy,
    /// Two parallel thin lines, e.g. `═`.
    Double,
}

/// The four arms of a line-drawing character, clockwise from the top.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arms {
    /// Line running from the centre of the cell to its top edge.
    pub up: Weight,
    /// Line running from the centre to the right edge.
    pub right: Weight,
    /// Line running from the centre to the bottom edge.
    pub down: Weight,
    /// Line running from the centre to the left edge.
    pub left: Weight,
}

impl Arms {
    const fn new(up: Weight, right: Weight, down: Weight, left: Weight) -> Self {
        Self {
            up,
            right,
            down,
            left,
        }
    }
}

use Weight::{Double as D, Heavy as H, Light as L, None as N};

/// How much of a cell a block character fills.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Block {
    /// Fraction of the cell filled from the given edge: (from_left, from_right, from_top, from_bottom).
    Partial {
        /// Fraction of the cell filled from the left edge.
        left: f32,
        /// Fraction filled from the right edge.
        right: f32,
        /// Fraction filled from the top edge.
        top: f32,
        /// Fraction filled from the bottom edge.
        bottom: f32,
    },
    /// Uniform coverage, for the shade characters.
    Shade(f32),
}

/// What, if anything, this character should be drawn as instead of a glyph.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Drawn {
    /// A line-drawing character, described by its four arms.
    Lines(Arms),
    /// A block or shade character, described by its coverage.
    Block(Block),
}

/// Returns the geometry for box-drawing and block characters, or `None` for normal text.
pub fn classify(ch: char) -> Option<Drawn> {
    let arms = match ch {
        '─' => Arms::new(N, L, N, L),
        '━' => Arms::new(N, H, N, H),
        '│' => Arms::new(L, N, L, N),
        '┃' => Arms::new(H, N, H, N),
        '┌' => Arms::new(N, L, L, N),
        '┏' => Arms::new(N, H, H, N),
        '┐' => Arms::new(N, N, L, L),
        '┓' => Arms::new(N, N, H, H),
        '└' => Arms::new(L, L, N, N),
        '┗' => Arms::new(H, H, N, N),
        '┘' => Arms::new(L, N, N, L),
        '┛' => Arms::new(H, N, N, H),
        '├' => Arms::new(L, L, L, N),
        '┣' => Arms::new(H, H, H, N),
        '┤' => Arms::new(L, N, L, L),
        '┫' => Arms::new(H, N, H, H),
        '┬' => Arms::new(N, L, L, L),
        '┳' => Arms::new(N, H, H, H),
        '┴' => Arms::new(L, L, N, L),
        '┻' => Arms::new(H, H, N, H),
        '┼' => Arms::new(L, L, L, L),
        '╋' => Arms::new(H, H, H, H),
        '═' => Arms::new(N, D, N, D),
        '║' => Arms::new(D, N, D, N),
        '╔' => Arms::new(N, D, D, N),
        '╗' => Arms::new(N, N, D, D),
        '╚' => Arms::new(D, D, N, N),
        '╝' => Arms::new(D, N, N, D),
        '╠' => Arms::new(D, D, D, N),
        '╣' => Arms::new(D, N, D, D),
        '╦' => Arms::new(N, D, D, D),
        '╩' => Arms::new(D, D, N, D),
        '╬' => Arms::new(D, D, D, D),
        _ => {
            let block = match ch {
                '█' => Block::Partial {
                    left: 1.0,
                    right: 0.0,
                    top: 0.0,
                    bottom: 0.0,
                },
                '▀' => Block::Partial {
                    left: 0.0,
                    right: 0.0,
                    top: 0.5,
                    bottom: 0.0,
                },
                '▄' => Block::Partial {
                    left: 0.0,
                    right: 0.0,
                    top: 0.0,
                    bottom: 0.5,
                },
                '▌' => Block::Partial {
                    left: 0.5,
                    right: 0.0,
                    top: 0.0,
                    bottom: 0.0,
                },
                '▐' => Block::Partial {
                    left: 0.0,
                    right: 0.5,
                    top: 0.0,
                    bottom: 0.0,
                },
                '░' => Block::Shade(0.25),
                '▒' => Block::Shade(0.5),
                '▓' => Block::Shade(0.75),
                _ => return None,
            };
            return Some(Drawn::Block(block));
        }
    };
    Some(Drawn::Lines(arms))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_characters_report_their_arms() {
        assert_eq!(classify('─'), Some(Drawn::Lines(Arms::new(N, L, N, L))));
        assert_eq!(classify('┼'), Some(Drawn::Lines(Arms::new(L, L, L, L))));
        assert_eq!(classify('╔'), Some(Drawn::Lines(Arms::new(N, D, D, N))));
        assert_eq!(classify('┗'), Some(Drawn::Lines(Arms::new(H, H, N, N))));
    }

    #[test]
    fn block_characters_report_their_coverage() {
        assert!(
            matches!(classify('█'), Some(Drawn::Block(Block::Partial { left, .. })) if left == 1.0)
        );
        assert!(matches!(classify('▒'), Some(Drawn::Block(Block::Shade(s))) if s == 0.5));
    }

    #[test]
    fn ordinary_text_is_left_to_the_font() {
        assert_eq!(classify('A'), None);
        assert_eq!(classify(' '), None);
        assert_eq!(classify('→'), None);
    }
}
