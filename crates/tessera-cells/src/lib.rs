//! Character-cell grid, glyph atlas and CPU painter (design §5).
//!
//! The launcher lays out screens with ratatui widgets, which land in a [`Grid`]
//! through [`CellBackend`]. [`paint()`] then turns the changed cells into pixels,
//! which the Wayland front end hands to the compositor as a shared-memory buffer.

#![warn(missing_docs)]

pub mod backend;
pub mod boxdraw;
pub mod color;
pub mod font;
pub mod grid;
pub mod paint;

pub use backend::CellBackend;
pub use color::{Palette, Rgb};
pub use font::{CellMetrics, Font, FontError};
pub use grid::{Attrs, Cell, Grid};
pub use paint::{Damage, Surface, paint};
