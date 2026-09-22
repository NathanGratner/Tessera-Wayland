//! Turns dirty cells into pixels.

use crate::{
    boxdraw::{self, Arms, Block, Drawn, Weight},
    color::Rgb,
    font::Font,
    grid::Grid,
};

/// A rectangle of changed pixels, for `wl_surface.damage_buffer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Damage {
    /// Left edge in buffer pixels.
    pub x: i32,
    /// Top edge in buffer pixels.
    pub y: i32,
    /// Width in pixels.
    pub width: i32,
    /// Height in pixels.
    pub height: i32,
}

/// A mutable `Argb8888` pixel buffer.
pub struct Surface<'a> {
    pixels: &'a mut [u8],
    /// Bytes per row, which may exceed `width * 4`.
    stride: usize,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl<'a> Surface<'a> {
    /// Wraps a buffer. `stride` is the byte length of one row.
    pub fn new(pixels: &'a mut [u8], stride: usize, width: u32, height: u32) -> Self {
        Self {
            pixels,
            stride,
            width,
            height,
        }
    }

    fn put(&mut self, x: u32, y: u32, color: Rgb) {
        if x >= self.width || y >= self.height {
            return;
        }
        let offset = y as usize * self.stride + x as usize * 4;
        let Some(slot) = self.pixels.get_mut(offset..offset + 4) else {
            return;
        };
        slot.copy_from_slice(&color.to_argb8888().to_ne_bytes());
    }

    fn get(&self, x: u32, y: u32) -> Rgb {
        let offset = y as usize * self.stride + x as usize * 4;
        match self.pixels.get(offset..offset + 4) {
            Some(slot) => {
                let word = u32::from_ne_bytes([slot[0], slot[1], slot[2], slot[3]]);
                Rgb((word >> 16) as u8, (word >> 8) as u8, word as u8)
            }
            None => Rgb::BLACK,
        }
    }

    fn blend(&mut self, x: u32, y: u32, color: Rgb, alpha: u8) {
        if alpha == 0 || x >= self.width || y >= self.height {
            return;
        }
        let blended = color.blend(self.get(x, y), alpha);
        self.put(x, y, blended);
    }

    fn fill(&mut self, x: u32, y: u32, width: u32, height: u32, color: Rgb) {
        for row in y..(y + height).min(self.height) {
            for col in x..(x + width).min(self.width) {
                self.put(col, row, color);
            }
        }
    }
}

/// Paints the dirty cells of `grid` into `surface`, returning the damaged areas.
///
/// One damage rectangle per run of dirty cells in a row, which keeps the list short
/// for the common case of a single changed line.
pub fn paint(grid: &Grid, font: &mut Font, surface: &mut Surface<'_>) -> Vec<Damage> {
    let metrics = font.metrics();
    let (cell_w, cell_h) = (metrics.width, metrics.height);
    let mut damage = Vec::new();

    for row in 0..grid.rows() {
        let mut run_start: Option<u16> = None;
        for col in 0..=grid.cols() {
            let dirty = col < grid.cols() && grid.is_dirty(col, row);
            if dirty {
                if run_start.is_none() {
                    run_start = Some(col);
                }
                if let Some(cell) = grid.cell(col, row) {
                    paint_cell(
                        surface,
                        font,
                        cell,
                        col as u32 * cell_w,
                        row as u32 * cell_h,
                    );
                }
            } else if let Some(start) = run_start.take() {
                damage.push(Damage {
                    x: start as i32 * cell_w as i32,
                    y: row as i32 * cell_h as i32,
                    width: (col - start) as i32 * cell_w as i32,
                    height: cell_h as i32,
                });
            }
        }
    }

    if !damage.is_empty() {
        damage.extend(paint_margins(grid, surface, cell_w, cell_h));
    }
    damage
}

/// Fills the strip beyond the last whole cell. A surface is rarely an exact
/// multiple of the cell size, and those pixels would otherwise show whatever
/// the buffer happened to contain.
fn paint_margins(grid: &Grid, surface: &mut Surface<'_>, cell_w: u32, cell_h: u32) -> Vec<Damage> {
    let used_w = grid.cols() as u32 * cell_w;
    let used_h = grid.rows() as u32 * cell_h;
    let background = grid.background();
    let mut damage = Vec::new();

    if used_w < surface.width {
        let width = surface.width - used_w;
        surface.fill(used_w, 0, width, surface.height, background);
        damage.push(Damage {
            x: used_w as i32,
            y: 0,
            width: width as i32,
            height: surface.height as i32,
        });
    }
    if used_h < surface.height {
        let height = surface.height - used_h;
        surface.fill(0, used_h, used_w.min(surface.width), height, background);
        damage.push(Damage {
            x: 0,
            y: used_h as i32,
            width: used_w.min(surface.width) as i32,
            height: height as i32,
        });
    }
    damage
}

fn paint_cell(
    surface: &mut Surface<'_>,
    font: &mut Font,
    cell: &crate::grid::Cell,
    x: u32,
    y: u32,
) {
    let metrics = font.metrics();
    let (cell_w, cell_h) = (metrics.width, metrics.height);
    surface.fill(x, y, cell_w, cell_h, cell.bg);

    match boxdraw::classify(cell.ch) {
        Some(Drawn::Lines(arms)) => draw_lines(surface, arms, cell.fg, x, y, cell_w, cell_h),
        Some(Drawn::Block(block)) => draw_block(surface, block, cell.fg, x, y, cell_w, cell_h),
        None => {
            if let Some(glyph) = font.glyph(cell.ch, cell.attrs.bold) {
                let pen_x = x as i32 + glyph.left;
                let pen_y = y as i32 + metrics.baseline as i32 - glyph.top;
                for gy in 0..glyph.height {
                    for gx in 0..glyph.width {
                        let alpha = glyph.coverage[(gy * glyph.width + gx) as usize];
                        let px = pen_x + gx as i32;
                        let py = pen_y + gy as i32;
                        if px >= 0 && py >= 0 {
                            surface.blend(px as u32, py as u32, cell.fg, alpha);
                        }
                    }
                }
            }
        }
    }

    if cell.attrs.underline {
        surface.fill(x, y + metrics.underline, cell_w, 1, cell.fg);
    }
}

/// Stroke thickness for a weight, in pixels.
fn thickness(weight: Weight, cell_h: u32) -> u32 {
    let light = (cell_h / 12).max(1);
    match weight {
        Weight::None => 0,
        Weight::Light | Weight::Double => light,
        Weight::Heavy => light * 2,
    }
}

fn draw_lines(
    surface: &mut Surface<'_>,
    arms: Arms,
    fg: Rgb,
    x: u32,
    y: u32,
    cell_w: u32,
    cell_h: u32,
) {
    let centre_x = x + cell_w / 2;
    let centre_y = y + cell_h / 2;

    // Each arm reaches from the centre to the cell edge, so neighbours meet exactly.
    let mut horizontal = |weight: Weight, from: u32, to: u32, offset: i32| {
        if weight == Weight::None {
            return;
        }
        let t = thickness(weight, cell_h);
        let top = (centre_y as i32 - (t / 2) as i32 + offset).max(y as i32) as u32;
        surface.fill(from, top, to.saturating_sub(from), t, fg);
    };
    let separation = (thickness(Weight::Light, cell_h) * 2).max(2) as i32;

    match arms.left {
        Weight::Double => {
            horizontal(Weight::Light, x, centre_x + separation as u32, -separation);
            horizontal(Weight::Light, x, centre_x + separation as u32, separation);
        }
        weight => horizontal(weight, x, centre_x + thickness(weight, cell_h), 0),
    }
    match arms.right {
        Weight::Double => {
            horizontal(
                Weight::Light,
                centre_x.saturating_sub(separation as u32),
                x + cell_w,
                -separation,
            );
            horizontal(
                Weight::Light,
                centre_x.saturating_sub(separation as u32),
                x + cell_w,
                separation,
            );
        }
        weight => horizontal(weight, centre_x, x + cell_w, 0),
    }

    let mut vertical = |weight: Weight, from: u32, to: u32, offset: i32| {
        if weight == Weight::None {
            return;
        }
        let t = thickness(weight, cell_h);
        let left = (centre_x as i32 - (t / 2) as i32 + offset).max(x as i32) as u32;
        surface.fill(left, from, t, to.saturating_sub(from), fg);
    };
    match arms.up {
        Weight::Double => {
            vertical(Weight::Light, y, centre_y + separation as u32, -separation);
            vertical(Weight::Light, y, centre_y + separation as u32, separation);
        }
        weight => vertical(weight, y, centre_y + thickness(weight, cell_h), 0),
    }
    match arms.down {
        Weight::Double => {
            vertical(
                Weight::Light,
                centre_y.saturating_sub(separation as u32),
                y + cell_h,
                -separation,
            );
            vertical(
                Weight::Light,
                centre_y.saturating_sub(separation as u32),
                y + cell_h,
                separation,
            );
        }
        weight => vertical(weight, centre_y, y + cell_h, 0),
    }
}

fn draw_block(
    surface: &mut Surface<'_>,
    block: Block,
    fg: Rgb,
    x: u32,
    y: u32,
    cell_w: u32,
    cell_h: u32,
) {
    match block {
        Block::Partial {
            left,
            right,
            top,
            bottom,
        } => {
            if left > 0.0 {
                surface.fill(x, y, (cell_w as f32 * left).round() as u32, cell_h, fg);
            }
            if right > 0.0 {
                let width = (cell_w as f32 * right).round() as u32;
                surface.fill(x + cell_w - width, y, width, cell_h, fg);
            }
            if top > 0.0 {
                surface.fill(x, y, cell_w, (cell_h as f32 * top).round() as u32, fg);
            }
            if bottom > 0.0 {
                let height = (cell_h as f32 * bottom).round() as u32;
                surface.fill(x, y + cell_h - height, cell_w, height, fg);
            }
        }
        Block::Shade(level) => {
            let alpha = (level * 255.0).round().clamp(0.0, 255.0) as u8;
            for py in y..y + cell_h {
                for px in x..x + cell_w {
                    surface.blend(px, py, fg, alpha);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::{Attrs, Cell};

    struct Canvas {
        pixels: Vec<u8>,
        width: u32,
        height: u32,
    }

    impl Canvas {
        fn new(width: u32, height: u32) -> Self {
            Self {
                pixels: vec![0; (width * height * 4) as usize],
                width,
                height,
            }
        }

        fn surface(&mut self) -> Surface<'_> {
            Surface::new(
                &mut self.pixels,
                (self.width * 4) as usize,
                self.width,
                self.height,
            )
        }

        fn at(&self, x: u32, y: u32) -> Rgb {
            let offset = (y * self.width + x) as usize * 4;
            let word = u32::from_ne_bytes([
                self.pixels[offset],
                self.pixels[offset + 1],
                self.pixels[offset + 2],
                self.pixels[offset + 3],
            ]);
            Rgb((word >> 16) as u8, (word >> 8) as u8, word as u8)
        }

        /// Highest red channel anywhere, i.e. how close to full coverage the glyph got.
        fn brightest(&self) -> u8 {
            (0..self.height)
                .flat_map(|y| (0..self.width).map(move |x| (x, y)))
                .map(|(x, y)| self.at(x, y).0)
                .max()
                .unwrap_or(0)
        }
    }

    fn font() -> Font {
        Font::load("JetBrains Mono", 16.0).expect("JetBrains Mono should be installed")
    }

    #[test]
    fn painting_fills_the_background_of_every_cell() {
        let mut font = font();
        let metrics = font.metrics();
        let mut grid = Grid::new(2, 1, Rgb::WHITE, Rgb(0x16, 0x36, 0xA0));
        let mut canvas = Canvas::new(metrics.width * 2, metrics.height);

        paint(&grid, &mut font, &mut canvas.surface());
        assert_eq!(canvas.at(0, 0), Rgb(0x16, 0x36, 0xA0));
        assert_eq!(canvas.at(metrics.width + 1, 0), Rgb(0x16, 0x36, 0xA0));

        grid.clear_dirty();
        assert!(paint(&grid, &mut font, &mut canvas.surface()).is_empty());
    }

    #[test]
    fn glyphs_are_drawn_in_the_foreground_colour() {
        let mut font = font();
        let metrics = font.metrics();
        let mut grid = Grid::new(1, 1, Rgb::WHITE, Rgb::BLACK);
        grid.set(
            0,
            0,
            Cell {
                ch: 'M',
                fg: Rgb::WHITE,
                bg: Rgb::BLACK,
                attrs: Attrs::default(),
            },
        );
        let mut canvas = Canvas::new(metrics.width, metrics.height);
        paint(&grid, &mut font, &mut canvas.surface());
        // Antialiasing means coverage peaks just below 255, so look for a near-white pixel.
        assert!(
            canvas.brightest() > 200,
            "the glyph should be visible; brightest pixel was {}",
            canvas.brightest()
        );
    }

    #[test]
    fn box_lines_reach_the_cell_edges_so_neighbours_join() {
        let mut font = font();
        let metrics = font.metrics();
        let mut grid = Grid::new(1, 1, Rgb::WHITE, Rgb::BLACK);
        grid.set(
            0,
            0,
            Cell {
                ch: '─',
                fg: Rgb::WHITE,
                bg: Rgb::BLACK,
                attrs: Attrs::default(),
            },
        );
        let mut canvas = Canvas::new(metrics.width, metrics.height);
        paint(&grid, &mut font, &mut canvas.surface());

        let mid = metrics.height / 2;
        let lit =
            |x: u32| (mid.saturating_sub(2)..=(mid + 2)).any(|y| canvas.at(x, y) == Rgb::WHITE);
        assert!(lit(0), "line should touch the left edge");
        assert!(lit(metrics.width - 1), "line should touch the right edge");
    }

    #[test]
    fn leftover_pixels_beyond_the_last_cell_are_filled() {
        let mut font = font();
        let metrics = font.metrics();
        let background = Rgb(0x16, 0x36, 0xA0);
        let grid = Grid::new(2, 1, Rgb::WHITE, background);
        // A surface a few pixels wider and taller than two whole cells.
        let mut canvas = Canvas::new(metrics.width * 2 + 5, metrics.height + 3);
        canvas.pixels.fill(0x7F); // stand-in for whatever the buffer held before

        paint(&grid, &mut font, &mut canvas.surface());
        assert_eq!(
            canvas.at(metrics.width * 2 + 4, 0),
            background,
            "right margin"
        );
        assert_eq!(
            canvas.at(0, metrics.height + 2),
            background,
            "bottom margin"
        );
    }

    #[test]
    fn damage_covers_one_run_per_row() {
        let mut font = font();
        let metrics = font.metrics();
        let mut grid = Grid::new(4, 2, Rgb::WHITE, Rgb::BLACK);
        grid.clear_dirty();
        grid.write_str(1, 1, "ab", Rgb::WHITE, Rgb::BLACK);

        let mut canvas = Canvas::new(metrics.width * 4, metrics.height * 2);
        let damage = paint(&grid, &mut font, &mut canvas.surface());
        // The surface is an exact multiple of the cell size here, so there are no margins.
        assert_eq!(
            damage,
            vec![Damage {
                x: metrics.width as i32,
                y: metrics.height as i32,
                width: metrics.width as i32 * 2,
                height: metrics.height as i32,
            }]
        );
    }

    #[test]
    fn underline_draws_below_the_baseline() {
        let mut font = font();
        let metrics = font.metrics();
        let mut grid = Grid::new(1, 1, Rgb::WHITE, Rgb::BLACK);
        grid.set(
            0,
            0,
            Cell {
                ch: ' ',
                fg: Rgb::WHITE,
                bg: Rgb::BLACK,
                attrs: Attrs {
                    bold: false,
                    underline: true,
                },
            },
        );
        let mut canvas = Canvas::new(metrics.width, metrics.height);
        paint(&grid, &mut font, &mut canvas.surface());
        assert_eq!(canvas.at(0, metrics.underline), Rgb::WHITE);
        assert_eq!(canvas.at(0, 0), Rgb::BLACK);
    }
}
