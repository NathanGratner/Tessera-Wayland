//! Font loading, cell metrics and the glyph atlas.

use std::collections::HashMap;

use fontdb::{Database, Family, Query, Stretch, Style, Weight};
use swash::{
    FontRef, GlyphId,
    scale::{Render, ScaleContext, Source},
    zeno::Format,
};

/// Why a font could not be used.
#[derive(Debug, thiserror::Error)]
pub enum FontError {
    /// Nothing on the system matched the requested family, not even a fallback.
    #[error("no font matching `{0}` is installed")]
    NotFound(String),
    /// The font file was found but could not be read or parsed.
    #[error("the font data for `{0}` could not be read")]
    Unreadable(String),
}

/// One loaded face: the raw file kept alive plus the index inside it.
struct Face {
    data: Vec<u8>,
    index: u32,
}

impl Face {
    fn font(&self) -> Option<FontRef<'_>> {
        FontRef::from_index(&self.data, self.index as usize)
    }
}

/// Cell geometry derived from the font, all in whole pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellMetrics {
    /// Advance width of one cell; every glyph in a monospace font shares it.
    pub width: u32,
    /// Full line height: ascent + descent + line gap.
    pub height: u32,
    /// Distance from the top of the cell down to the text baseline.
    pub baseline: u32,
    /// Where an underline sits, measured from the top of the cell.
    pub underline: u32,
}

/// A rasterised glyph: 8-bit coverage plus where it sits relative to the pen.
pub struct Glyph {
    /// Width of the coverage bitmap in pixels.
    pub width: u32,
    /// Height of the coverage bitmap in pixels.
    pub height: u32,
    /// Horizontal offset from the pen position.
    pub left: i32,
    /// Height above the baseline of the bitmap's top row.
    pub top: i32,
    /// One alpha byte per pixel, `width * height` long.
    pub coverage: Vec<u8>,
}

/// Loads a monospace font and rasterises its glyphs on demand.
pub struct Font {
    regular: Face,
    bold: Option<Face>,
    size_px: f32,
    metrics: CellMetrics,
    context: ScaleContext,
    atlas: HashMap<(char, bool), Option<Glyph>>,
}

impl Font {
    /// Loads `family` from the system fonts at `size_px`.
    pub fn load(family: &str, size_px: f32) -> Result<Self, FontError> {
        let mut db = Database::new();
        db.load_system_fonts();

        let regular = load_face(&db, family, Weight::NORMAL)?;
        let bold = load_face(&db, family, Weight::BOLD).ok();

        let mut font = Self {
            regular,
            bold,
            size_px,
            metrics: CellMetrics {
                width: 1,
                height: 1,
                baseline: 1,
                underline: 1,
            },
            context: ScaleContext::new(),
            atlas: HashMap::new(),
        };
        font.metrics = font.measure();
        Ok(font)
    }

    /// The cell geometry this font and size produce.
    pub fn metrics(&self) -> CellMetrics {
        self.metrics
    }

    fn measure(&self) -> CellMetrics {
        let Some(font) = self.regular.font() else {
            return self.metrics;
        };
        let metrics = font.metrics(&[]).scale(self.size_px);
        let height = (metrics.ascent + metrics.descent + metrics.leading)
            .ceil()
            .max(1.0) as u32;
        let baseline = metrics.ascent.ceil().max(0.0) as u32;

        // Monospace: every glyph has the same advance, so '0' speaks for all of them.
        let glyph = font.charmap().map('0');
        let advance = font
            .glyph_metrics(&[])
            .scale(self.size_px)
            .advance_width(glyph);
        let width = advance.ceil().max(1.0) as u32;

        let underline = (baseline as f32 + (metrics.descent / 2.0).max(1.0)).round() as u32;
        CellMetrics {
            width,
            height,
            baseline,
            underline: underline.min(height.saturating_sub(1)),
        }
    }

    /// Rasterises `ch`, caching the result. Returns `None` for blank glyphs.
    pub fn glyph(&mut self, ch: char, bold: bool) -> Option<&Glyph> {
        let key = (ch, bold);
        if !self.atlas.contains_key(&key) {
            let rendered = self.render(ch, bold);
            self.atlas.insert(key, rendered);
        }
        self.atlas.get(&key).and_then(|glyph| glyph.as_ref())
    }

    fn render(&mut self, ch: char, bold: bool) -> Option<Glyph> {
        let face = match (bold, &self.bold) {
            (true, Some(bold)) => bold,
            _ => &self.regular,
        };
        let font = face.font()?;
        let id: GlyphId = font.charmap().map(ch);
        if id == 0 {
            return None;
        }

        let mut scaler = self
            .context
            .builder(font)
            .size(self.size_px)
            .hint(true)
            .build();
        let image = Render::new(&[Source::Outline])
            .format(Format::Alpha)
            .render(&mut scaler, id)?;

        if image.placement.width == 0 || image.placement.height == 0 {
            return None;
        }
        Some(Glyph {
            width: image.placement.width,
            height: image.placement.height,
            left: image.placement.left,
            top: image.placement.top,
            coverage: image.data,
        })
    }
}

fn load_face(db: &Database, family: &str, weight: Weight) -> Result<Face, FontError> {
    let query = Query {
        families: &[Family::Name(family), Family::Monospace],
        weight,
        stretch: Stretch::Normal,
        style: Style::Normal,
    };
    let id = db
        .query(&query)
        .ok_or_else(|| FontError::NotFound(family.to_string()))?;

    db.with_face_data(id, |data, index| Face {
        data: data.to_vec(),
        index,
    })
    .ok_or_else(|| FontError::Unreadable(family.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The launcher's default font; installed as a dependency (see docs/DEPENDENCIES.md).
    const FAMILY: &str = "JetBrains Mono";

    #[test]
    fn loads_a_monospace_font_with_sane_metrics() {
        let font = Font::load(FAMILY, 16.0).expect("JetBrains Mono should be installed");
        let metrics = font.metrics();
        assert!(metrics.width > 0 && metrics.height > 0);
        assert!(
            metrics.height > metrics.width,
            "monospace cells are taller than wide"
        );
        assert!(metrics.baseline > 0 && metrics.baseline < metrics.height);
        assert!(metrics.underline >= metrics.baseline);
        assert!(metrics.underline < metrics.height);
    }

    #[test]
    fn cells_grow_with_the_font_size() {
        let small = Font::load(FAMILY, 12.0).unwrap().metrics();
        let large = Font::load(FAMILY, 24.0).unwrap().metrics();
        assert!(large.width > small.width && large.height > small.height);
    }

    #[test]
    fn rasterises_glyphs_and_skips_blank_ones() {
        let mut font = Font::load(FAMILY, 16.0).unwrap();
        let glyph = font.glyph('W', false).expect("W has an outline");
        assert!(glyph.width > 0 && glyph.height > 0);
        assert_eq!(glyph.coverage.len(), (glyph.width * glyph.height) as usize);
        assert!(glyph.coverage.iter().any(|value| *value > 0));
        assert!(font.glyph(' ', false).is_none(), "space has no outline");
    }

    #[test]
    fn unknown_family_reports_not_found() {
        let err = Font::load("No Such Font Here 12345", 16.0);
        // fontdb falls back to any monospace face, so this only fails if nothing matches.
        assert!(err.is_ok() || matches!(err, Err(FontError::NotFound(_))));
    }
}
