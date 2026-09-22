//! Colours, and the palette ratatui's named colours map onto.

/// An opaque 8-bit-per-channel colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Solid black.
    pub const BLACK: Rgb = Rgb(0, 0, 0);
    /// Solid white.
    pub const WHITE: Rgb = Rgb(0xFF, 0xFF, 0xFF);

    /// Packs into the `Argb8888` word `wl_shm` expects (alpha always opaque).
    pub fn to_argb8888(self) -> u32 {
        0xFF00_0000 | ((self.0 as u32) << 16) | ((self.1 as u32) << 8) | self.2 as u32
    }

    /// Blends `self` over `dst` with coverage `alpha` (0–255).
    pub fn blend(self, dst: Rgb, alpha: u8) -> Rgb {
        if alpha == 255 {
            return self;
        }
        if alpha == 0 {
            return dst;
        }
        let mix = |src: u8, dst: u8| {
            let src = src as u32 * alpha as u32;
            let dst = dst as u32 * (255 - alpha) as u32;
            ((src + dst + 127) / 255) as u8
        };
        Rgb(mix(self.0, dst.0), mix(self.1, dst.1), mix(self.2, dst.2))
    }
}

/// The sixteen ANSI colours, used when a cell asks for a named colour.
/// These are the classic VGA values the kernel's menuconfig is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// The sixteen ANSI colours, in the usual order: black, red, green, yellow,
    /// blue, magenta, cyan, white, then the eight bright variants.
    pub ansi: [Rgb; 16],
    /// Colour for text that asks for the default foreground (`Color::Reset`).
    pub foreground: Rgb,
    /// Colour behind text that asks for the default background.
    pub background: Rgb,
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            ansi: [
                Rgb(0x10, 0x10, 0x10), // black
                Rgb(0xA5, 0x1E, 0x1E), // red
                Rgb(0x2E, 0x8B, 0x57), // green
                Rgb(0xB0, 0x8A, 0x2A), // yellow
                Rgb(0x16, 0x36, 0xA0), // blue
                Rgb(0x8B, 0x3A, 0x8B), // magenta
                Rgb(0x2A, 0x8B, 0x8B), // cyan
                Rgb(0xC4, 0xC6, 0xCC), // white (the dialog grey)
                Rgb(0x6B, 0x6E, 0x76), // bright black
                Rgb(0xE0, 0x5A, 0x4A),
                Rgb(0x5A, 0xC4, 0x8A),
                Rgb(0xFF, 0xE2, 0x7A),
                Rgb(0x5A, 0x7A, 0xE0),
                Rgb(0xC4, 0x7A, 0xC4),
                Rgb(0x6F, 0xCB, 0xCB),
                Rgb(0xFF, 0xFF, 0xFF), // bright white
            ],
            foreground: Rgb(0x10, 0x10, 0x10),
            background: Rgb(0xC4, 0xC6, 0xCC),
        }
    }
}

impl Palette {
    /// Resolves an xterm 256-colour index.
    pub fn indexed(&self, index: u8) -> Rgb {
        match index {
            0..=15 => self.ansi[index as usize],
            16..=231 => {
                // 6x6x6 colour cube.
                let index = index - 16;
                let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                Rgb(level(index / 36), level((index / 6) % 6), level(index % 6))
            }
            _ => {
                let value = 8 + (index - 232) * 10;
                Rgb(value, value, value)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_argb_with_opaque_alpha() {
        assert_eq!(Rgb(0x12, 0x34, 0x56).to_argb8888(), 0xFF12_3456);
    }

    #[test]
    fn blending_covers_the_endpoints_and_the_middle() {
        let fg = Rgb(200, 100, 0);
        let bg = Rgb(0, 0, 100);
        assert_eq!(fg.blend(bg, 255), fg);
        assert_eq!(fg.blend(bg, 0), bg);
        assert_eq!(fg.blend(bg, 128), Rgb(100, 50, 50));
    }

    #[test]
    fn indexed_colours_cover_cube_and_greys() {
        let palette = Palette::default();
        assert_eq!(palette.indexed(1), palette.ansi[1]);
        assert_eq!(palette.indexed(16), Rgb(0, 0, 0));
        assert_eq!(palette.indexed(231), Rgb(255, 255, 255));
        assert_eq!(palette.indexed(232), Rgb(8, 8, 8));
        assert_eq!(palette.indexed(255), Rgb(238, 238, 238));
    }
}
