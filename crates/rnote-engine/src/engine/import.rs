// Imports
use super::StrokeContent;
use crate::document::Layout;
use crate::engine_view_mut;
use crate::pens::Pen;
use crate::pens::PenStyle;
use crate::store::StrokeKey;
use crate::store::chrono_comp::StrokeLayer;
use crate::strokes::vectorimage::PdfPageDrawing;
use crate::strokes::{BitmapImage, Stroke, VectorImage};
use crate::strokes::{Resize, resize::ImageSizeOption, resize::calculate_resize_ratio};
use crate::{Engine, WidgetFlags};
use futures::channel::oneshot;
use p2d::math::Vector2;
use rnote_compose::shapes::Shapeable;
use serde::{Deserialize, Serialize};
use std::ops::Range;
use std::sync::Arc;
use std::time::Instant;
use tracing::error;

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, num_derive::FromPrimitive, num_derive::ToPrimitive,
)]
#[serde(rename = "pdf_import_pages_type")]
pub enum PdfImportPagesType {
    #[serde(rename = "bitmap")]
    Bitmap = 0,
    #[serde(rename = "vector")]
    Vector,
}

impl Default for PdfImportPagesType {
    fn default() -> Self {
        Self::Vector
    }
}

impl TryFrom<u32> for PdfImportPagesType {
    type Error = anyhow::Error;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        num_traits::FromPrimitive::from_u32(value).ok_or_else(|| {
            anyhow::anyhow!(
                "PdfImportPagesType try_from::<u32>() for value {} failed",
                value
            )
        })
    }
}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, num_derive::FromPrimitive, num_derive::ToPrimitive,
)]
#[serde(rename = "pdf_import_page_spacing")]
pub enum PdfImportPageSpacing {
    #[serde(rename = "continuous")]
    Continuous = 0,
    #[serde(rename = "one_per_document_page")]
    OnePerDocumentPage,
}

impl Default for PdfImportPageSpacing {
    fn default() -> Self {
        Self::Continuous
    }
}

impl TryFrom<u32> for PdfImportPageSpacing {
    type Error = anyhow::Error;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        num_traits::FromPrimitive::from_u32(value).ok_or_else(|| {
            anyhow::anyhow!(
                "PdfImportPageSpacing try_from::<u32>() for value {} failed",
                value
            )
        })
    }
}

/// Pdf import preferences.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default, rename = "pdf_import_prefs")]
pub struct PdfImportPrefs {
    /// Pdf page width in percentage to the format width.
    #[serde(rename = "page_width_perc")]
    pub page_width_perc: f64,
    /// Pdf page spacing.
    #[serde(rename = "page_spacing")]
    pub page_spacing: PdfImportPageSpacing,
    /// Pdf pages import type.
    #[serde(rename = "pages_type")]
    pub pages_type: PdfImportPagesType,
    /// The scalefactor when importing as bitmap image
    #[serde(rename = "bitmap_scalefactor")]
    pub bitmap_scalefactor: f64,
    /// Whether the document layout should be adjusted to the Pdf
    #[serde(rename = "adjust_document")]
    pub adjust_document: bool,
    /// How many pages are placed next to each other in a row. 1 places them below each other, 2 as spreads of a
    /// book.
    #[serde(rename = "columns")]
    pub columns: u32,
    /// Whether the first page of the Pdf sits alone at the end of the first row, like the cover of a book. Only has
    /// an effect with more than one column.
    #[serde(rename = "first_page_alone")]
    pub first_page_alone: bool,
    /// The gap between the pages of a row.
    #[serde(rename = "column_gap")]
    pub column_gap: f64,
    /// The gap between rows, with continuous page spacing.
    #[serde(rename = "row_gap")]
    pub row_gap: f64,
}

impl Default for PdfImportPrefs {
    fn default() -> Self {
        Self {
            pages_type: PdfImportPagesType::default(),
            page_width_perc: 50.0,
            page_spacing: PdfImportPageSpacing::default(),
            bitmap_scalefactor: 1.8,
            adjust_document: false,
            columns: 1,
            first_page_alone: false,
            column_gap: Self::GAP_DEFAULT,
            row_gap: Self::GAP_DEFAULT,
        }
    }
}

/// A page of a Pdf that is placed on the document at import.
#[derive(Debug, Clone, Copy)]
pub struct PdfPageToPlace {
    /// The index of the page in the Pdf.
    pub index: usize,
    /// The size of the page on the document.
    pub size: Vector2,
}

impl PdfImportPrefs {
    pub const GAP_DEFAULT: f64 = 16.0;
    pub const COLUMNS_MAX: u32 = 20;

    /// The top left corner of each page on the document, in the order of `pages`. `pages` are in the order of the
    /// Pdf.
    ///
    /// Pages fill rows of [Self::columns] from left to right. The column of a page follows its index in the Pdf, so
    /// that spreads pair up as in the Pdf whatever page the import starts at. All columns are as wide as the widest
    /// page.
    ///
    /// A row is as high as its highest page, plus [Self::row_gap] with continuous spacing. With one row per
    /// document page, or when the document is adjusted to the Pdf, every row takes a document page.
    /// `format_height` is the document page height when the document is not adjusted.
    pub fn page_positions(
        &self,
        pages: &[PdfPageToPlace],
        origin: Vector2,
        format_height: f64,
    ) -> Vec<Vector2> {
        let Some(first) = pages.first() else {
            return vec![];
        };
        let columns = self.columns.clamp(1, Self::COLUMNS_MAX) as usize;
        let lead = if self.first_page_alone {
            columns - 1
        } else {
            0
        };
        let first_row = (first.index + lead) / columns;
        let row = |page: &PdfPageToPlace| (page.index + lead) / columns - first_row;
        let column = |page: &PdfPageToPlace| (page.index + lead) % columns;

        let column_width = pages.iter().map(|page| page.size[0]).fold(0.0, f64::max);
        let page_height_max = pages.iter().map(|page| page.size[1]).fold(0.0, f64::max);
        let n_rows = pages.iter().map(|page| row(page) + 1).max().unwrap_or(0);
        let mut row_heights = vec![0.0f64; n_rows];
        for page in pages {
            row_heights[row(page)] = row_heights[row(page)].max(page.size[1]);
        }
        let row_tops = row_heights
            .iter()
            .scan(origin[1], |top, &height| {
                let row_top = *top;
                *top += if self.adjust_document {
                    page_height_max
                } else {
                    match self.page_spacing {
                        PdfImportPageSpacing::Continuous => height + self.row_gap,
                        PdfImportPageSpacing::OnePerDocumentPage => format_height,
                    }
                };
                Some(row_top)
            })
            .collect::<Vec<f64>>();

        pages
            .iter()
            .map(|page| {
                Vector2::new(
                    origin[0] + column(page) as f64 * (column_width + self.column_gap),
                    row_tops[row(page)],
                )
            })
            .collect()
    }
}

/// Xournal++ `.xopp` file import preferences.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename = "xopp_import_prefs")]
pub struct XoppImportPrefs {
    /// Import DPI.
    #[serde(rename = "pages_type")]
    pub dpi: f64,
}

impl Default for XoppImportPrefs {
    fn default() -> Self {
        Self { dpi: 96.0 }
    }
}

/// Import preferences.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(default, rename = "import_prefs")]
pub struct ImportPrefs {
    /// Pdf import preferences
    #[serde(rename = "pdf_import_prefs")]
    pub pdf_import_prefs: PdfImportPrefs,
    /// Xournal++ `.xopp` file import preferences
    #[serde(rename = "xopp_import_prefs")]
    pub xopp_import_prefs: XoppImportPrefs,
}

impl Engine {
    /// Generate a vectorimage from the bytes.
    ///
    /// The bytes are expected to be from a valid UTF-8 encoded Svg string.
    pub fn generate_vectorimage_from_bytes(
        &self,
        pos: Vector2,
        bytes: Vec<u8>,
        respect_borders: bool,
    ) -> oneshot::Receiver<anyhow::Result<VectorImage>> {
        let (oneshot_sender, oneshot_receiver) = oneshot::channel::<anyhow::Result<VectorImage>>();

        let resize_struct = Resize {
            width: self.document.config.format.width(),
            height: self.document.config.format.height(),
            layout_fixed_width: self.document.config.layout.is_fixed_width(),
            max_viewpoint: Some(self.camera.viewport().maxs),
            restrain_to_viewport: true,
            respect_borders,
        };
        rayon::spawn(move || {
            let result = || -> anyhow::Result<VectorImage> {
                let svg_str = String::from_utf8(bytes)?;

                VectorImage::from_svg_str(
                    &svg_str,
                    pos,
                    ImageSizeOption::ResizeImage(resize_struct),
                )
            };

            if oneshot_sender.send(result()).is_err() {
                error!(
                    "Sending result to receiver while generating VectorImage from bytes failed. Receiver already dropped."
                );
            }
        });

        oneshot_receiver
    }

    /// Generate a bitmapimage for the bytes.
    ///
    /// The bytes are expected to be from a valid bitmap image (Png/Jpeg).
    pub fn generate_bitmapimage_from_bytes(
        &self,
        pos: Vector2,
        bytes: Vec<u8>,
        respect_borders: bool,
    ) -> oneshot::Receiver<anyhow::Result<BitmapImage>> {
        let (oneshot_sender, oneshot_receiver) = oneshot::channel::<anyhow::Result<BitmapImage>>();

        let resize_struct = Resize {
            width: self.document.config.format.width(),
            height: self.document.config.format.height(),
            layout_fixed_width: self.document.config.layout.is_fixed_width(),
            max_viewpoint: Some(self.camera.viewport().maxs),
            restrain_to_viewport: true,
            respect_borders,
        };
        let files = self.files.clone();
        rayon::spawn(move || {
            let result = || -> anyhow::Result<BitmapImage> {
                let image = BitmapImage::from_image_bytes(
                    &bytes,
                    pos,
                    ImageSizeOption::ResizeImage(resize_struct),
                )?;
                // The picture is kept as it was, so that a note folder can save the file instead of its pixels
                let extension = image::guess_format(&bytes)?
                    .extensions_str()
                    .first()
                    .copied()
                    .unwrap_or("img");
                let file = files.insert(Arc::new(bytes), extension);
                Ok(BitmapImage {
                    file: Some(file),
                    ..image
                })
            };

            if oneshot_sender.send(result()).is_err() {
                error!(
                    "Sending result to receiver while generating BitmapImage from bytes failed. Receiver already dropped."
                );
            }
        });

        oneshot_receiver
    }

    /// Generate image strokes for each page for the bytes.
    ///
    /// The bytes are expected to be from a valid Pdf.
    ///
    /// Note: `insert_pos` does not have an effect when the `adjust_document` import pref is set true.
    ///
    /// `drawing` is how the pages of a Pdf the document keeps are drawn.
    #[allow(clippy::type_complexity)]
    pub fn generate_pdf_pages_from_bytes(
        &self,
        bytes: Vec<u8>,
        insert_pos: Vector2,
        page_range: Option<Range<usize>>,
        password: Option<String>,
        drawing: PdfPageDrawing,
    ) -> oneshot::Receiver<anyhow::Result<Vec<(Stroke, Option<StrokeLayer>)>>> {
        let (oneshot_sender, oneshot_receiver) =
            oneshot::channel::<anyhow::Result<Vec<(Stroke, Option<StrokeLayer>)>>>();
        let pdf_import_prefs = self.config.read().import_prefs.pdf_import_prefs;
        let format = self.document.config.format;
        let insert_pos = if self
            .config
            .read()
            .import_prefs
            .pdf_import_prefs
            .adjust_document
        {
            Vector2::ZERO
        } else {
            insert_pos
        };

        let files = self.files.clone();

        rayon::spawn(move || {
            let result = || -> anyhow::Result<Vec<(Stroke, Option<StrokeLayer>)>> {
                match pdf_import_prefs.pages_type {
                    PdfImportPagesType::Bitmap => {
                        let bitmapimages = BitmapImage::from_pdf_bytes(
                            &bytes,
                            pdf_import_prefs,
                            insert_pos,
                            page_range,
                            &format,
                            password,
                        )?
                        .into_iter()
                        .map(|s| (Stroke::BitmapImage(s), Some(StrokeLayer::Document)))
                        .collect::<Vec<(Stroke, Option<StrokeLayer>)>>();
                        Ok(bitmapimages)
                    }
                    PdfImportPagesType::Vector => {
                        // The Pdf is kept, so that a note folder can save it as it is. Not one that needs a
                        // password: its pages could not be made again without asking for it.
                        let bytes = Arc::new(bytes);
                        let pdf_file = password
                            .is_none()
                            .then(|| files.insert(Arc::clone(&bytes), "pdf"));
                        let vectorimages = VectorImage::from_pdf_bytes(
                            &bytes,
                            pdf_import_prefs,
                            insert_pos,
                            page_range,
                            &format,
                            password,
                            pdf_file,
                            drawing,
                        )?
                        .into_iter()
                        .map(|s| (Stroke::VectorImage(s), Some(StrokeLayer::Document)))
                        .collect::<Vec<(Stroke, Option<StrokeLayer>)>>();
                        Ok(vectorimages)
                    }
                }
            };

            if oneshot_sender.send(result()).is_err() {
                error!(
                    "Sending result to receiver while importing Pdf bytes failed. Receiver already dropped"
                );
            }
        });

        oneshot_receiver
    }

    /// Import the generated strokes into the store.
    pub fn import_generated_content(
        &mut self,
        strokes: Vec<(Stroke, Option<StrokeLayer>)>,
        adjust_document: bool,
    ) -> WidgetFlags {
        let mut widget_flags = WidgetFlags::default();
        if strokes.is_empty() {
            return widget_flags;
        }
        let select = !adjust_document;

        // we need to always deselect all strokes. Even tough changing the pen style deselects too, it does only when
        // the pen is actually different.
        let all_strokes = self.store.stroke_keys_as_rendered();
        self.store.set_selected_keys(&all_strokes, false);

        if select {
            widget_flags |= self.change_pen_style(PenStyle::Selector);
        }

        // The pages of a row of a Pdf can be wider than the document
        let content_width = strokes
            .iter()
            .map(|(stroke, _)| stroke.bounds().maxs[0])
            .fold(0.0, f64::max);
        if adjust_document {
            // A row of pages becomes a document page. The pages are placed from the origin.
            let page_height = strokes
                .iter()
                .map(|(stroke, _)| stroke.bounds().extents()[1])
                .fold(0.0, f64::max);
            self.document.config.format.set_width(content_width);
            self.document.config.format.set_height(page_height);
            widget_flags |= self.set_doc_layout(Layout::FixedSize) | self.doc_resize_autoexpand()
        } else if self.document.config.layout.is_fixed_width()
            && content_width > self.document.config.format.width()
        {
            // Widened to keep the margin on the left on the right as well
            let left = strokes
                .iter()
                .map(|(stroke, _)| stroke.bounds().mins[0])
                .fold(f64::INFINITY, f64::min)
                .max(0.0);
            self.document.config.format.set_width(content_width + left);
        }

        let inserted = strokes
            .into_iter()
            .map(|(stroke, layer)| self.store.insert_stroke(stroke, layer))
            .collect::<Vec<StrokeKey>>();

        // resize after the strokes are inserted, but before they are set selected
        widget_flags |= self.doc_resize_to_fit_content();
        if select {
            self.store.set_selected_keys(&inserted, true);
        }
        widget_flags |= self.current_pen_update_state();
        widget_flags |= self.store.record(Instant::now());
        widget_flags.resize = true;
        widget_flags.store_modified = true;
        widget_flags.refresh_ui = true;

        widget_flags
    }

    /// Insert text.
    pub fn insert_text(&mut self, text: String, pos: Option<Vector2>) -> WidgetFlags {
        let mut widget_flags = WidgetFlags::default();

        // we need to always deselect all strokes. Even tough changing the pen style deselects too, but only when the pen is actually changed.
        let all_strokes = self.store.stroke_keys_as_rendered();
        self.store.set_selected_keys(&all_strokes, false);

        widget_flags |= self.change_pen_style(PenStyle::Typewriter);

        if let Pen::Typewriter(typewriter) = self.penholder.current_pen_mut() {
            widget_flags |= typewriter.insert_text(text, pos, &mut engine_view_mut!(self));
        }

        widget_flags |= self.store.record(Instant::now());
        widget_flags.redraw = true;
        widget_flags
    }

    /// Insert the stroke content.
    ///
    /// The data usually comes from the clipboard, drag-and-drop, ..
    pub fn insert_stroke_content(
        &mut self,
        content: StrokeContent,
        pos: Vector2,
        resize: ImageSizeOption,
    ) -> WidgetFlags {
        let mut widget_flags = WidgetFlags::default();

        // we need to always deselect all strokes
        // even though changing the pen style deselects too, but only when the pen is actually different.
        let all_strokes = self.store.stroke_keys_as_rendered();
        self.store.set_selected_keys(&all_strokes, false);
        widget_flags |= self.change_pen_style(PenStyle::Selector);

        // calculate ratio
        let ratio = match resize {
            ImageSizeOption::ResizeImage(resize) => {
                calculate_resize_ratio(resize, content.size().unwrap(), pos)
            }
            _ => 1.0f64,
        };
        let inserted_keys = self.store.insert_stroke_content(content, ratio, pos);

        // re generate view
        self.store.update_geometry_for_strokes(&inserted_keys);
        self.store.regenerate_rendering_in_viewport_threaded(
            self.tasks_tx.clone(),
            false,
            self.camera.viewport(),
            self.camera.image_scale(),
        );

        widget_flags |= self
            .penholder
            .current_pen_update_state(&mut engine_view_mut!(self));

        widget_flags |= self.store.record(Instant::now());
        widget_flags.redraw = true;

        widget_flags
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A4: Vector2 = Vector2::new(100.0, 140.0);

    fn pages(indices: Range<usize>) -> Vec<PdfPageToPlace> {
        indices
            .map(|index| PdfPageToPlace { index, size: A4 })
            .collect()
    }

    fn prefs(columns: u32, first_page_alone: bool) -> PdfImportPrefs {
        PdfImportPrefs {
            columns,
            first_page_alone,
            column_gap: 10.0,
            row_gap: 20.0,
            ..PdfImportPrefs::default()
        }
    }

    fn xy(positions: Vec<Vector2>) -> Vec<(f64, f64)> {
        positions.into_iter().map(|p| (p[0], p[1])).collect()
    }

    #[test]
    fn one_column_stacks_pages_with_the_row_gap() {
        let positions = prefs(1, true).page_positions(&pages(0..3), Vector2::new(5.0, 7.0), 1000.0);
        assert_eq!(xy(positions), [(5.0, 7.0), (5.0, 167.0), (5.0, 327.0)]);
    }

    #[test]
    fn spreads_with_the_first_page_alone_on_the_right() {
        let positions = prefs(2, true).page_positions(&pages(0..5), Vector2::ZERO, 1000.0);
        assert_eq!(
            xy(positions),
            [
                (110.0, 0.0),
                (0.0, 160.0),
                (110.0, 160.0),
                (0.0, 320.0),
                (110.0, 320.0)
            ]
        );
    }

    #[test]
    fn spreads_pair_up_as_in_the_pdf_when_starting_later() {
        // Pages 4 to 6 (indices 3..6) of a book with a cover: page 4 is on the left, 5 on the right
        let positions = prefs(2, true).page_positions(&pages(3..6), Vector2::ZERO, 1000.0);
        assert_eq!(xy(positions), [(0.0, 0.0), (110.0, 0.0), (0.0, 160.0)]);
        // Without a cover, page 5 (index 4) starts a row
        let positions = prefs(2, false).page_positions(&pages(4..7), Vector2::ZERO, 1000.0);
        assert_eq!(xy(positions), [(0.0, 0.0), (110.0, 0.0), (0.0, 160.0)]);
    }

    #[test]
    fn grid_fills_rows_left_to_right() {
        let positions = prefs(3, false).page_positions(&pages(0..4), Vector2::ZERO, 1000.0);
        assert_eq!(
            xy(positions),
            [(0.0, 0.0), (110.0, 0.0), (220.0, 0.0), (0.0, 160.0)]
        );
    }

    #[test]
    fn rows_take_a_document_page_each_when_asked_or_adjusted() {
        let one_per_page = PdfImportPrefs {
            page_spacing: PdfImportPageSpacing::OnePerDocumentPage,
            ..prefs(2, false)
        };
        let positions = one_per_page.page_positions(&pages(0..3), Vector2::ZERO, 1000.0);
        assert_eq!(xy(positions), [(0.0, 0.0), (110.0, 0.0), (0.0, 1000.0)]);

        // Adjusted: the document page becomes as high as the highest page, without a row gap
        let adjusted = PdfImportPrefs {
            adjust_document: true,
            ..prefs(2, false)
        };
        let mut to_place = pages(0..3);
        to_place[2].size[1] = 150.0;
        let positions = adjusted.page_positions(&to_place, Vector2::ZERO, 1000.0);
        assert_eq!(xy(positions), [(0.0, 0.0), (110.0, 0.0), (0.0, 150.0)]);
    }

    #[test]
    fn columns_are_as_wide_as_the_widest_page() {
        let mut to_place = pages(0..2);
        to_place[0].size[0] = 130.0;
        let positions = prefs(2, false).page_positions(&to_place, Vector2::ZERO, 1000.0);
        assert_eq!(xy(positions), [(0.0, 0.0), (140.0, 0.0)]);
    }
}
