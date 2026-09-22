//! A ratatui backend that draws into a [`Grid`] instead of a terminal.

use std::convert::Infallible;

use ratatui::{
    backend::{Backend, ClearType, WindowSize},
    buffer,
    layout::{Position, Size},
    style::{Color, Modifier},
};

use crate::{
    color::{Palette, Rgb},
    font::CellMetrics,
    grid::{Attrs, Cell, Grid},
};

/// Wraps a [`Grid`] so ratatui widgets can render into it.
pub struct CellBackend {
    /// The cells ratatui has drawn, ready for [`crate::paint()`].
    pub grid: Grid,
    /// How named ratatui colours resolve to real colours.
    pub palette: Palette,
    metrics: CellMetrics,
    cursor: Position,
    cursor_visible: bool,
}

impl CellBackend {
    /// Wraps a grid so ratatui can draw into it.
    pub fn new(grid: Grid, palette: Palette, metrics: CellMetrics) -> Self {
        Self {
            grid,
            palette,
            metrics,
            cursor: Position::ORIGIN,
            cursor_visible: false,
        }
    }

    /// Whether the app asked for a visible cursor. The launcher draws its own.
    pub fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    /// Resizes the grid to fit a surface of this pixel size, returning the new grid size.
    pub fn resize_to_pixels(&mut self, width: u32, height: u32) -> (u16, u16) {
        let cols = (width / self.metrics.width).max(1).min(u16::MAX as u32) as u16;
        let rows = (height / self.metrics.height).max(1).min(u16::MAX as u32) as u16;
        self.grid.resize(cols, rows);
        (cols, rows)
    }

    fn resolve(&self, color: Color, fallback: Rgb) -> Rgb {
        match color {
            Color::Reset => fallback,
            Color::Black => self.palette.ansi[0],
            Color::Red => self.palette.ansi[1],
            Color::Green => self.palette.ansi[2],
            Color::Yellow => self.palette.ansi[3],
            Color::Blue => self.palette.ansi[4],
            Color::Magenta => self.palette.ansi[5],
            Color::Cyan => self.palette.ansi[6],
            Color::Gray => self.palette.ansi[7],
            Color::DarkGray => self.palette.ansi[8],
            Color::LightRed => self.palette.ansi[9],
            Color::LightGreen => self.palette.ansi[10],
            Color::LightYellow => self.palette.ansi[11],
            Color::LightBlue => self.palette.ansi[12],
            Color::LightMagenta => self.palette.ansi[13],
            Color::LightCyan => self.palette.ansi[14],
            Color::White => self.palette.ansi[15],
            Color::Indexed(index) => self.palette.indexed(index),
            Color::Rgb(r, g, b) => Rgb(r, g, b),
        }
    }
}

impl Backend for CellBackend {
    /// Drawing into memory cannot fail.
    type Error = Infallible;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a buffer::Cell)>,
    {
        for (col, row, cell) in content {
            let mut fg = self.resolve(cell.fg, self.palette.foreground);
            let mut bg = self.resolve(cell.bg, self.palette.background);
            if cell.modifier.contains(Modifier::REVERSED) {
                std::mem::swap(&mut fg, &mut bg);
            }
            // A wide glyph's trailing cell has an empty symbol; paint it as background.
            let ch = cell.symbol().chars().next().unwrap_or(' ');
            self.grid.set(
                col,
                row,
                Cell {
                    ch,
                    fg,
                    bg,
                    attrs: Attrs {
                        bold: cell.modifier.contains(Modifier::BOLD),
                        underline: cell.modifier.contains(Modifier::UNDERLINED),
                    },
                },
            );
        }
        Ok(())
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.cursor_visible = false;
        Ok(())
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.cursor_visible = true;
        Ok(())
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        Ok(self.cursor)
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        self.cursor = position.into();
        Ok(())
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.grid.clear();
        Ok(())
    }

    fn clear_region(&mut self, _clear_type: ClearType) -> Result<(), Self::Error> {
        // The launcher redraws whole screens, so a full clear is always correct here.
        self.grid.clear();
        Ok(())
    }

    fn size(&self) -> Result<Size, Self::Error> {
        Ok(Size::new(self.grid.cols(), self.grid.rows()))
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        Ok(WindowSize {
            columns_rows: Size::new(self.grid.cols(), self.grid.rows()),
            pixels: Size::new(
                (self.grid.cols() as u32 * self.metrics.width).min(u16::MAX as u32) as u16,
                (self.grid.rows() as u32 * self.metrics.height).min(u16::MAX as u32) as u16,
            ),
        })
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        // Pixels are produced by `paint` when the surface is ready for a frame.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{
        Terminal,
        style::Style,
        widgets::{Block, Paragraph},
    };

    fn backend(cols: u16, rows: u16) -> CellBackend {
        let palette = Palette::default();
        let metrics = CellMetrics {
            width: 8,
            height: 16,
            baseline: 12,
            underline: 13,
        };
        CellBackend::new(
            Grid::new(cols, rows, palette.foreground, palette.background),
            palette,
            metrics,
        )
    }

    #[test]
    fn widgets_land_in_the_grid() {
        let mut terminal = Terminal::new(backend(12, 3)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(Paragraph::new("hi"), frame.area());
            })
            .unwrap();

        let grid = &terminal.backend().grid;
        assert_eq!(grid.cell(0, 0).unwrap().ch, 'h');
        assert_eq!(grid.cell(1, 0).unwrap().ch, 'i');
    }

    #[test]
    fn blocks_draw_box_characters_the_painter_knows() {
        let mut terminal = Terminal::new(backend(6, 3)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(Block::bordered(), frame.area());
            })
            .unwrap();

        let grid = &terminal.backend().grid;
        assert_eq!(grid.cell(0, 0).unwrap().ch, '┌');
        assert_eq!(grid.cell(5, 2).unwrap().ch, '┘');
        assert!(crate::boxdraw::classify(grid.cell(1, 0).unwrap().ch).is_some());
    }

    #[test]
    fn named_colours_come_from_the_palette_and_reverse_swaps_them() {
        let mut terminal = Terminal::new(backend(4, 1)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    Paragraph::new("ab").style(Style::new().fg(Color::Red).bg(Color::Blue)),
                    frame.area(),
                );
            })
            .unwrap();

        let palette = Palette::default();
        let cell = terminal.backend().grid.cell(0, 0).unwrap();
        assert_eq!(cell.fg, palette.ansi[1]);
        assert_eq!(cell.bg, palette.ansi[4]);

        terminal
            .draw(|frame| {
                frame.render_widget(
                    Paragraph::new("ab").style(
                        Style::new()
                            .fg(Color::Red)
                            .bg(Color::Blue)
                            .add_modifier(Modifier::REVERSED),
                    ),
                    frame.area(),
                );
            })
            .unwrap();
        let cell = terminal.backend().grid.cell(0, 0).unwrap();
        assert_eq!((cell.fg, cell.bg), (palette.ansi[4], palette.ansi[1]));
    }

    #[test]
    fn pixel_size_decides_the_grid_size() {
        let mut backend = backend(1, 1);
        // 8x16 cells: 800x600 pixels is 100 columns by 37 rows, remainder ignored.
        assert_eq!(backend.resize_to_pixels(800, 600), (100, 37));
        assert_eq!(backend.size().unwrap(), Size::new(100, 37));
        // Never zero, however small the window gets.
        assert_eq!(backend.resize_to_pixels(1, 1), (1, 1));
    }
}
