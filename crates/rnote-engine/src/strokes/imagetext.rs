//! Text that an image carries, for example the text layer of an imported Pdf page.
//!
//! It is kept with the image, so it can be found by search without reading the image back with text recognition.

// Imports
use super::textstroke::{TextChar, TextLine};
use hayro::hayro_interpret::font::Glyph;
use hayro::hayro_interpret::hayro_cmap::BfString;
use hayro::hayro_interpret::hayro_syntax::page::Page;
use hayro::hayro_interpret::{
    BlendMode, ClipPath, Context, Device, GlyphDrawMode, Image, InterpreterSettings, Paint,
    PathDrawMode, SoftMask, TransformExt, interpret_page,
};
// The Pdf interpreter is on a newer kurbo than this crate
use hayro::vello_cpu::kurbo::{Affine, BezPath, Point, Rect};
use p2d::bounding_volume::{Aabb, BoundingVolume};
use p2d::math::Vector2;
use rnote_compose::ext::DAffine2Ext;
use rnote_compose::shapes::Rectangle;
use serde::{Deserialize, Serialize};

/// A line of text on an image. All positions are fractions of the image's width and height, so they hold whatever
/// the size of the image is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "image_text_line")]
pub struct ImageTextLine {
    #[serde(rename = "text")]
    pub text: String,
    /// The left and the right edge of each character. In a vertical line they are the same for all characters.
    #[serde(rename = "spans")]
    pub spans: Vec<[f64; 2]>,
    #[serde(rename = "top")]
    pub top: f64,
    #[serde(rename = "bottom")]
    pub bottom: f64,
}

impl ImageTextLine {
    /// The line on the document, for an image that is placed there as the given rectangle.
    pub fn to_document(&self, rectangle: &Rectangle) -> TextLine {
        let size = rectangle.cuboid.half_extents * 2.0;
        let on_document = |x0: f64, x1: f64| {
            rectangle.affine.transform_aabb(Aabb::new(
                (Vector2::new(x0, self.top) - Vector2::splat(0.5)) * size,
                (Vector2::new(x1, self.bottom) - Vector2::splat(0.5)) * size,
            ))
        };
        let chars = self
            .text
            .chars()
            .zip(self.spans.iter())
            .map(|(ch, span)| TextChar {
                ch,
                bounds: on_document(span[0], span[1]),
            })
            .collect::<Vec<TextChar>>();
        let bounds = chars
            .iter()
            .fold(Aabb::new_invalid(), |bounds, c| bounds.merged(&c.bounds));
        TextLine { chars, bounds }
    }
}

/// Collects the text of a Pdf page. Returns no lines when the page has no text that maps to characters, as is the
/// case for scans.
///
/// Text that is drawn invisibly counts: that is how a scanned Pdf with a recognised text layer carries it.
/// Text that does not run from left to right is left out.
pub fn pdf_page_text(page: &Page<'_>, settings: &InterpreterSettings) -> Vec<ImageTextLine> {
    let (width, height) = page.render_dimensions();
    let (width, height) = (width as f64, height as f64);
    let mut context = Context::new(
        page.initial_transform(true).to_kurbo(),
        Rect::new(0.0, 0.0, width, height),
        page.xref(),
        settings.clone(),
    );
    let mut collector = GlyphCollector::default();
    interpret_page(page, &mut context, &mut collector);
    lines_of_glyphs(collector.glyphs, width, height)
}

/// A character of a page, in the coordinates of the page with the origin at the top left.
#[derive(Debug, Clone, Copy)]
struct PageGlyph {
    ch: char,
    x0: f64,
    x1: f64,
    baseline: f64,
    /// The font size.
    size: f64,
}

/// A device for the Pdf interpreter that takes down the glyphs and ignores all else.
#[derive(Debug, Default)]
struct GlyphCollector {
    glyphs: Vec<PageGlyph>,
}

impl<'a> Device<'a> for GlyphCollector {
    fn draw_glyph(
        &mut self,
        glyph: &Glyph<'a>,
        transform: Affine,
        glyph_transform: Affine,
        _: &Paint<'a>,
        _: &GlyphDrawMode,
    ) {
        // Glyphs that are drawn with Pdf instructions have no advance to place them by. They are rare.
        let Glyph::Outline(outline) = glyph else {
            return;
        };
        let (Some(text), Some(advance)) = (outline.as_unicode(), outline.advance_width()) else {
            return;
        };
        let text = match text {
            BfString::Char(ch) => ch.to_string(),
            BfString::String(string) => string,
        };
        // Glyph coordinates have 1000 units to the font size
        let to_page = transform * glyph_transform;
        let origin = to_page * Point::ZERO;
        let end = to_page * Point::new(advance as f64, 0.0);
        let size = (to_page * Point::new(0.0, 1000.0) - origin).hypot();
        let run = end - origin;
        if run.x <= 0.0 || run.y.abs() > 0.2 * run.x || size <= 0.0 {
            return;
        }

        // A ligature stands for several characters, they share its width
        let n_chars = text.chars().count() as f64;
        for (i, ch) in text.chars().enumerate() {
            let is_text =
                !ch.is_control() && !('\u{E000}'..='\u{F8FF}').contains(&ch) && ch != '\u{FFFD}';
            if is_text {
                self.glyphs.push(PageGlyph {
                    ch,
                    x0: origin.x + run.x * i as f64 / n_chars,
                    x1: origin.x + run.x * (i + 1) as f64 / n_chars,
                    baseline: origin.y,
                    size,
                });
            }
        }
    }

    fn set_soft_mask(&mut self, _: Option<SoftMask<'a>>) {}
    fn set_blend_mode(&mut self, _: BlendMode) {}
    fn draw_path(&mut self, _: &BezPath, _: Affine, _: &Paint<'a>, _: &PathDrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_image(&mut self, _: Image<'a, '_>, _: Affine) {}
    fn pop_clip_path(&mut self) {}
    fn pop_transparency_group(&mut self) {}
}

/// Groups the glyphs of a page into lines.
///
/// Glyphs are not drawn in reading order: ruby text and labels are interleaved with the body text. So lines are
/// formed by position alone: glyphs on the same baseline and of the same size, split where a wide gap separates
/// columns. Single glyphs right below each other form a vertical line, which is how ruby zhuyin is set.
fn lines_of_glyphs(mut glyphs: Vec<PageGlyph>, width: f64, height: f64) -> Vec<ImageTextLine> {
    /// How far baselines of one line may be apart, as a share of the font size.
    const BASELINE_TOLERANCE: f64 = 0.3;
    /// A gap wider than this share of the font size separates words.
    const WORD_GAP: f64 = 0.2;
    /// A gap wider than this share of the font size separates lines, for example two columns.
    const LINE_GAP: f64 = 2.0;
    /// How far apart the baselines of a vertical line are, as shares of the font size.
    const STACK_STEP: std::ops::Range<f64> = 0.6..1.8;
    /// How far the box of a line reaches above and below its baseline, as shares of the font size.
    const ASCENT: f64 = 0.85;
    const DESCENT: f64 = 0.2;

    let same_size = |a: &PageGlyph, b: &PageGlyph| a.size.max(b.size) < 1.3 * a.size.min(b.size);
    glyphs.sort_by(|a, b| a.baseline.total_cmp(&b.baseline));
    let mut rows: Vec<Vec<PageGlyph>> = Vec::new();
    for glyph in glyphs {
        // The row of this baseline and size, among the last few rows: sizes mix where baselines are close
        let row = rows.iter_mut().rev().take(8).find(|row| {
            let last = row[row.len() - 1];
            same_size(&last, &glyph)
                && (glyph.baseline - last.baseline).abs() < BASELINE_TOLERANCE * glyph.size
        });
        match row {
            Some(row) => row.push(glyph),
            None => rows.push(vec![glyph]),
        }
    }

    // Rows are cut into horizontal lines at wide gaps, narrower gaps become spaces
    let mut horizontal: Vec<Vec<PageGlyph>> = Vec::new();
    for mut row in rows {
        row.sort_by(|a, b| a.x0.total_cmp(&b.x0));
        let mut line: Vec<PageGlyph> = Vec::new();
        for glyph in row {
            if let Some(&previous) = line.last() {
                let gap = glyph.x0 - previous.x1;
                if gap > LINE_GAP * glyph.size {
                    horizontal.push(std::mem::take(&mut line));
                } else if gap > WORD_GAP * glyph.size && !previous.ch.is_whitespace() {
                    line.push(PageGlyph {
                        ch: ' ',
                        x0: previous.x1,
                        x1: glyph.x0,
                        ..previous
                    });
                }
            }
            line.push(glyph);
        }
        horizontal.push(line);
    }
    // Spaces at the ends carry no text
    for line in horizontal.iter_mut() {
        let text = line
            .iter()
            .position(|g| !g.ch.is_whitespace())
            .unwrap_or(line.len())
            ..line
                .iter()
                .rposition(|g| !g.ch.is_whitespace())
                .map_or(0, |i| i + 1);
        *line = line.get(text).unwrap_or_default().to_vec();
    }
    horizontal.retain(|line| !line.is_empty());

    // Single glyphs, from top to bottom, join the vertical line that ends right above them
    let (singles, horizontal): (Vec<_>, Vec<_>) =
        horizontal.into_iter().partition(|line| line.len() == 1);
    let mut vertical: Vec<Vec<PageGlyph>> = Vec::new();
    for glyph in singles.into_iter().map(|line| line[0]) {
        let stack = vertical.iter_mut().find(|stack| {
            let last = stack[stack.len() - 1];
            same_size(&last, &glyph)
                && (glyph.x0 - last.x0).abs() < 0.5 * glyph.size
                && STACK_STEP.contains(&((glyph.baseline - last.baseline) / glyph.size))
        });
        match stack {
            Some(stack) => stack.push(glyph),
            None => vertical.push(vec![glyph]),
        }
    }

    let round = |v: f64| (v * 1e5).round() / 1e5;
    let line_of = |glyphs: &[PageGlyph], spans: Vec<[f64; 2]>| {
        let size = glyphs.iter().map(|g| g.size).fold(0.0, f64::max);
        let top = glyphs.iter().map(|g| g.baseline).fold(f64::MAX, f64::min) - ASCENT * size;
        let bottom = glyphs.iter().map(|g| g.baseline).fold(f64::MIN, f64::max) + DESCENT * size;
        ImageTextLine {
            text: glyphs.iter().map(|g| g.ch).collect(),
            spans: spans
                .into_iter()
                .map(|[x0, x1]| [round(x0 / width), round(x1 / width)])
                .collect(),
            top: round(top / height),
            bottom: round(bottom / height),
        }
    };
    let mut lines = Vec::new();
    for line in horizontal {
        lines.push(line_of(&line, line.iter().map(|g| [g.x0, g.x1]).collect()));
    }
    for stack in vertical {
        let x0 = stack.iter().map(|g| g.x0).fold(f64::MAX, f64::min);
        let x1 = stack.iter().map(|g| g.x1).fold(f64::MIN, f64::max);
        lines.push(line_of(&stack, vec![[x0, x1]; stack.len()]));
    }
    lines.sort_by(|a, b| {
        a.top
            .total_cmp(&b.top)
            .then(a.spans[0][0].total_cmp(&b.spans[0][0]))
    });
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use p2d::glamx::DAffine2;

    fn glyph(ch: char, x0: f64, baseline: f64, size: f64) -> PageGlyph {
        PageGlyph {
            ch,
            x0,
            x1: x0 + size,
            baseline,
            size,
        }
    }

    #[test]
    fn ruby_drawn_between_the_characters_does_not_break_their_line() {
        // 我 with ㄨㄛ beside it, then 是 with ㄕ, drawn in that order
        let glyphs = vec![
            glyph('我', 10.0, 50.0, 20.0),
            glyph('ㄨ', 30.0, 38.0, 6.0),
            glyph('ㄛ', 30.0, 46.0, 6.0),
            glyph('是', 38.0, 50.0, 20.0),
            glyph('ㄕ', 58.0, 42.0, 6.0),
        ];
        let lines = lines_of_glyphs(glyphs, 100.0, 100.0);
        let texts = lines.iter().map(|l| l.text.as_str()).collect::<Vec<&str>>();
        // The line of the characters is whole, and each stack of ruby beside them is a line from top to bottom
        assert_eq!(texts, ["ㄨㄛ", "我 是", "ㄕ"]);
        assert_eq!(lines[0].spans, [[0.3, 0.36], [0.3, 0.36]]);
    }

    #[test]
    fn gaps_become_spaces_and_columns_become_lines() {
        let mut glyphs = "ab"
            .chars()
            .enumerate()
            .map(|(i, ch)| glyph(ch, i as f64 * 10.0, 20.0, 10.0));
        let glyphs = glyphs
            .by_ref()
            .chain([glyph('c', 25.0, 20.0, 10.0), glyph('d', 80.0, 20.0, 10.0)])
            .collect();
        let lines = lines_of_glyphs(glyphs, 100.0, 100.0);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "ab c");
        assert_eq!(
            lines[0].spans,
            [[0.0, 0.1], [0.1, 0.2], [0.2, 0.25], [0.25, 0.35]]
        );
        assert_eq!(lines[1].text, "d");
    }

    #[test]
    fn imported_pdf_pages_carry_their_text() {
        use crate::document::Format;
        use crate::engine::import::PdfImportPrefs;
        use crate::strokes::{BitmapImage, Stroke, VectorImage};
        use rnote_compose::shapes::Shapeable;

        // One page with "Hello World" and "second line" below it
        let pdf = include_bytes!("../../../../misc/file-tests/text.pdf");
        let (prefs, pos, format) = (
            PdfImportPrefs::default(),
            Vector2::new(100.0, 200.0),
            Format::default(),
        );
        let vector =
            VectorImage::from_pdf_bytes(pdf, prefs, pos, None, &format, None, None).unwrap();
        let bitmap = BitmapImage::from_pdf_bytes(pdf, prefs, pos, None, &format, None).unwrap();
        let strokes = [
            Stroke::VectorImage(vector[0].clone()),
            Stroke::BitmapImage(bitmap[0].clone()),
        ];
        for stroke in strokes {
            let lines = stroke.text_lines().unwrap();
            let texts = lines
                .iter()
                .map(|l| l.chars.iter().map(|c| c.ch).collect::<String>())
                .collect::<Vec<String>>();
            assert_eq!(texts, ["Hello World", "second line"]);
            // The text sits on the page where it was put on the document, the first line above the second
            assert!(stroke.bounds().contains(&lines[0].bounds));
            assert!(lines[0].bounds.maxs.y < lines[1].bounds.mins.y);
            // 100 of 612 points from the left edge of the page
            let page = stroke.bounds();
            let left = (lines[0].bounds.mins.x - page.mins.x) / page.extents().x;
            assert!((left - 100.0 / 612.0).abs() < 1e-3);
        }
    }

    #[test]
    fn a_line_follows_its_image_onto_the_document() {
        let line = ImageTextLine {
            text: "ab".to_string(),
            spans: vec![[0.0, 0.25], [0.25, 0.5]],
            top: 0.5,
            bottom: 1.0,
        };
        // An image of 200 x 100 with its top left corner at (1000, 2000)
        let rectangle = Rectangle {
            cuboid: p2d::shape::Cuboid::new(Vector2::new(100.0, 50.0)),
            affine: DAffine2::from_translation(Vector2::new(1100.0, 2050.0)),
        };
        let on_document = line.to_document(&rectangle);
        assert_eq!(on_document.chars[1].ch, 'b');
        assert_eq!(
            on_document.chars[1].bounds.mins,
            Vector2::new(1050.0, 2050.0)
        );
        assert_eq!(
            on_document.chars[1].bounds.maxs,
            Vector2::new(1100.0, 2100.0)
        );
        assert_eq!(on_document.bounds.mins, Vector2::new(1000.0, 2050.0));
    }
}
