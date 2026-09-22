//! Tiling: the pure split tree, and the code that applies it to real windows.

pub mod apply;
pub mod place;
pub mod tree;

pub use tree::{Direction, LayoutOptions, LayoutTree};
