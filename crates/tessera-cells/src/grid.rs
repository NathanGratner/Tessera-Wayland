//! The character grid: what the launcher draws into, and what the painter reads.

use crate::color::Rgb;

/// Per-cell style flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Attrs {
    /// Draw with the bold face, falling back to the regular one.
    pub bold: bool,
    /// Draw a line under the cell.
    pub underline: bool,
}

/// One character cell: what to draw, in which colours.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// The character to draw. Wide characters occupy the first cell only.
    pub ch: char,
    /// Colour of the glyph or line.
    pub fg: Rgb,
    /// Colour behind it.
    pub bg: Rgb,
    /// Bold and underline flags.
    pub attrs: Attrs,
}

impl Cell {
    /// An empty cell in the given colours.
    pub fn blank(fg: Rgb, bg: Rgb) -> Self {
        Self {
            ch: ' ',
            fg,
            bg,
            attrs: Attrs::default(),
        }
    }
}

/// A grid of character cells with per-cell dirty tracking.
#[derive(Debug, Clone)]
pub struct Grid {
    cols: u16,
    rows: u16,
    cells: Vec<Cell>,
    dirty: Vec<bool>,
    blank: Cell,
}

impl Grid {
    /// A grid of blank cells, marked entirely dirty so the first paint draws it all.
    pub fn new(cols: u16, rows: u16, fg: Rgb, bg: Rgb) -> Self {
        let blank = Cell::blank(fg, bg);
        let count = cols as usize * rows as usize;
        Self {
            cols,
            rows,
            cells: vec![blank.clone(); count],
            dirty: vec![true; count],
            blank,
        }
    }

    /// Number of columns.
    pub fn cols(&self) -> u16 {
        self.cols
    }

    /// Number of rows.
    pub fn rows(&self) -> u16 {
        self.rows
    }

    /// The colour behind everything, used for the pixels beyond the last whole cell.
    pub fn background(&self) -> Rgb {
        self.blank.bg
    }

    /// The cell at this position, or `None` when it is outside the grid.
    pub fn cell(&self, col: u16, row: u16) -> Option<&Cell> {
        self.index(col, row).map(|index| &self.cells[index])
    }

    /// Whether this cell changed since the last paint.
    pub fn is_dirty(&self, col: u16, row: u16) -> bool {
        self.index(col, row)
            .map(|index| self.dirty[index])
            .unwrap_or(false)
    }

    /// Whether anything needs repainting.
    pub fn any_dirty(&self) -> bool {
        self.dirty.iter().any(|dirty| *dirty)
    }

    fn index(&self, col: u16, row: u16) -> Option<usize> {
        (col < self.cols && row < self.rows)
            .then(|| row as usize * self.cols as usize + col as usize)
    }

    /// Writes a cell, marking it dirty only when something actually changed.
    pub fn set(&mut self, col: u16, row: u16, cell: Cell) {
        let Some(index) = self.index(col, row) else {
            return;
        };
        if self.cells[index] != cell {
            self.cells[index] = cell;
            self.dirty[index] = true;
        }
    }

    /// Resets every cell to the blank cell.
    pub fn clear(&mut self) {
        for (cell, dirty) in self.cells.iter_mut().zip(self.dirty.iter_mut()) {
            if *cell != self.blank {
                *cell = self.blank.clone();
                *dirty = true;
            }
        }
    }

    /// Resizes the grid, keeping the cells that still fit. Everything is redrawn after this.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        if (cols, rows) == (self.cols, self.rows) {
            return;
        }
        let mut cells = vec![self.blank.clone(); cols as usize * rows as usize];
        for row in 0..rows.min(self.rows) {
            for col in 0..cols.min(self.cols) {
                let old = row as usize * self.cols as usize + col as usize;
                let new = row as usize * cols as usize + col as usize;
                cells[new] = self.cells[old].clone();
            }
        }
        self.cells = cells;
        self.dirty = vec![true; cols as usize * rows as usize];
        self.cols = cols;
        self.rows = rows;
    }

    /// Marks everything dirty, e.g. after attaching a buffer the compositor never showed.
    pub fn mark_all_dirty(&mut self) {
        self.dirty.iter_mut().for_each(|dirty| *dirty = true);
    }

    /// Marks everything clean; call after a successful paint.
    pub fn clear_dirty(&mut self) {
        self.dirty.iter_mut().for_each(|dirty| *dirty = false);
    }

    /// Convenience for tests and simple screens.
    pub fn write_str(&mut self, col: u16, row: u16, text: &str, fg: Rgb, bg: Rgb) {
        for (offset, ch) in text.chars().enumerate() {
            self.set(
                col + offset as u16,
                row,
                Cell {
                    ch,
                    fg,
                    bg,
                    attrs: Attrs::default(),
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> Grid {
        Grid::new(4, 3, Rgb::BLACK, Rgb::WHITE)
    }

    #[test]
    fn new_grids_start_fully_dirty() {
        let grid = grid();
        assert_eq!((grid.cols(), grid.rows()), (4, 3));
        assert!(grid.any_dirty());
        assert!(grid.is_dirty(3, 2));
        assert_eq!(grid.cell(0, 0).unwrap().ch, ' ');
    }

    #[test]
    fn writing_the_same_cell_twice_only_dirties_it_once() {
        let mut grid = grid();
        grid.clear_dirty();
        let cell = Cell {
            ch: 'x',
            fg: Rgb::BLACK,
            bg: Rgb::WHITE,
            attrs: Attrs::default(),
        };
        grid.set(1, 1, cell.clone());
        assert!(grid.is_dirty(1, 1));
        grid.clear_dirty();
        grid.set(1, 1, cell);
        assert!(!grid.is_dirty(1, 1), "unchanged cell should stay clean");
    }

    #[test]
    fn out_of_bounds_writes_are_ignored() {
        let mut grid = grid();
        grid.set(99, 99, Cell::blank(Rgb::WHITE, Rgb::BLACK));
        assert!(grid.cell(99, 99).is_none());
        assert!(!grid.is_dirty(99, 99));
    }

    #[test]
    fn resize_keeps_overlapping_cells() {
        let mut grid = grid();
        grid.write_str(0, 0, "ab", Rgb::BLACK, Rgb::WHITE);
        grid.resize(8, 2);
        assert_eq!((grid.cols(), grid.rows()), (8, 2));
        assert_eq!(grid.cell(1, 0).unwrap().ch, 'b');
        assert_eq!(grid.cell(7, 1).unwrap().ch, ' ');
        assert!(grid.is_dirty(7, 1));
    }

    #[test]
    fn clear_resets_to_blank() {
        let mut grid = grid();
        grid.write_str(0, 0, "hi", Rgb::BLACK, Rgb::WHITE);
        grid.clear_dirty();
        grid.clear();
        assert_eq!(grid.cell(0, 0).unwrap().ch, ' ');
        assert!(grid.is_dirty(0, 0));
    }
}
