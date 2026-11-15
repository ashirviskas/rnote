//! The preview in the Pdf import dialog: the first pages of the chosen range, placed as the import will place them.
//!
//! The pages are placed by [PdfImportPrefs::page_positions], the function the import uses. Each page is drawn
//! once, small and in the background, and kept for as long as the dialog is open; a change of the layout only moves
//! the drawn pages.

// Imports
use adw::prelude::*;
use gettextrs::gettext;
use gtk4::{DrawingArea, Label, cairo, glib};
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use p2d::bounding_volume::{Aabb, BoundingVolume};
use p2d::math::Vector2;
use rnote_engine::document::{Format, Layout};
use rnote_engine::engine::import::{PdfImportPageSpacing, PdfImportPrefs, PdfPageToPlace};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use tracing::error;

/// How wide a page is drawn, in pixels. Enough for the preview on a screen with scale 2.
const THUMBNAIL_WIDTH: f64 = 240.0;
/// Space around the pages in the preview, in pixels.
const PADDING: f64 = 14.0;
const CORNER_RADIUS: f64 = 3.0;

/// A page drawn in the background, in the pixel layout of a cairo ARGB32 surface.
struct Thumbnail {
    width: i32,
    height: i32,
    data: Vec<u8>,
}

/// What the preview shows: the import prefs and the pages chosen in the dialog.
#[derive(Debug, Clone)]
pub(crate) struct PreviewRequest {
    pub(crate) prefs: PdfImportPrefs,
    /// Indices of the chosen pages in the Pdf.
    pub(crate) pages: Range<usize>,
}

/// A page as it lies in the preview, in document coordinates.
struct PlacedPage {
    index: usize,
    bounds: Aabb,
}

/// The pages of the preview and the width of the document they are imported into.
struct PreviewLayout {
    pages: Vec<PlacedPage>,
    doc_width: f64,
    /// The height of a document page, when every row takes one.
    row_height: Option<f64>,
}

struct Inner {
    area: DrawingArea,
    caption: Label,
    pdf: Arc<Pdf>,
    format: Format,
    /// The layout of the document the pages are imported into.
    layout: Layout,
    request: RefCell<Option<PreviewRequest>>,
    thumbnails: RefCell<HashMap<usize, cairo::ImageSurface>>,
    requested: RefCell<HashSet<usize>>,
}

#[derive(Clone)]
pub(crate) struct PdfImportPreview(Rc<Inner>);

impl PdfImportPreview {
    /// How many rows of pages the preview shows.
    const ROWS: usize = 2;

    pub(crate) fn new(
        area: &DrawingArea,
        caption: &Label,
        pdf: Arc<Pdf>,
        format: Format,
        layout: Layout,
    ) -> Self {
        let preview = Self(Rc::new(Inner {
            area: area.clone(),
            caption: caption.clone(),
            pdf,
            format,
            layout,
            request: RefCell::new(None),
            thumbnails: RefCell::new(HashMap::new()),
            requested: RefCell::new(HashSet::new()),
        }));
        let weak = Rc::downgrade(&preview.0);
        area.set_draw_func(move |area, cx, width, height| {
            if let Some(inner) = weak.upgrade() {
                Self(inner).draw(area, cx, width as f64, height as f64);
            }
        });
        preview
    }

    /// Shows the pages and layout of `request`. Pages not drawn yet are drawn in the background.
    pub(crate) fn update(&self, request: PreviewRequest) {
        let shown = Self::shown_pages(&request);
        self.0.caption.set_label(&self.caption(&request, &shown));
        *self.0.request.borrow_mut() = Some(request);
        for index in shown {
            if self.0.requested.borrow_mut().insert(index) {
                self.draw_thumbnail(index);
            }
        }
        self.0.area.queue_draw();
    }

    /// The first pages of the chosen range: as many as fill two rows.
    fn shown_pages(request: &PreviewRequest) -> Range<usize> {
        let columns = request.prefs.columns.clamp(1, PdfImportPrefs::COLUMNS_MAX) as usize;
        let lead = if request.prefs.first_page_alone {
            columns - 1
        } else {
            0
        };
        let start = request.pages.start;
        let first_row = (start + lead) / columns;
        let end = (start..request.pages.end)
            .find(|index| (index + lead) / columns - first_row >= Self::ROWS)
            .unwrap_or(request.pages.end);
        start..end
    }

    fn caption(&self, request: &PreviewRequest, shown: &Range<usize>) -> String {
        if shown.is_empty() {
            return gettext("No pages");
        }
        let mut parts = vec![if shown.len() < request.pages.len() {
            format!(
                "{} {}–{} {} {}–{}",
                gettext("Pages"),
                shown.start + 1,
                shown.end,
                gettext("of"),
                request.pages.start + 1,
                request.pages.end
            )
        } else {
            format!("{} {}–{}", gettext("Pages"), shown.start + 1, shown.end)
        }];
        let columns = request.prefs.columns.max(1);
        parts.push(if columns == 1 {
            gettext("below each other")
        } else {
            format!("{columns} {}", gettext("per row"))
        });
        match (columns, request.prefs.first_page_alone) {
            (2, true) => parts.push(gettext("odd pages on the right, page 1 alone")),
            (2, false) => parts.push(gettext("odd pages on the left")),
            (3.., true) => parts.push(gettext("page 1 alone")),
            _ => {}
        }
        if self.0.layout.is_fixed_width()
            && !request.prefs.adjust_document
            && let Some(layout) = self.layout(request, shown.clone())
            && layout
                .pages
                .iter()
                .any(|page| page.bounds.maxs[0] > layout.doc_width + 0.5)
        {
            parts.push(gettext("wider than the document, which is widened"));
        }
        parts.join(" · ")
    }

    fn layout(&self, request: &PreviewRequest, shown: Range<usize>) -> Option<PreviewLayout> {
        let pages = self.0.pdf.pages();
        let first_width = pages.first()?.render_dimensions().0 as f64;
        let prefs = request.prefs;
        let page_width = if prefs.adjust_document {
            self.0.format.width()
        } else {
            self.0.format.width() * (prefs.page_width_perc / 100.0)
        };
        let page_zoom = page_width / first_width;
        let to_place = shown
            .filter_map(|index| {
                let dimensions = pages.get(index)?.render_dimensions();
                Some(PdfPageToPlace {
                    index,
                    size: Vector2::new(dimensions.0 as f64, dimensions.1 as f64) * page_zoom,
                })
            })
            .collect::<Vec<PdfPageToPlace>>();
        let positions = prefs.page_positions(&to_place, Vector2::ZERO, self.0.format.height());
        let placed = to_place
            .iter()
            .zip(positions)
            .map(|(page, pos)| PlacedPage {
                index: page.index,
                bounds: Aabb::new(pos, pos + page.size),
            })
            .collect::<Vec<PlacedPage>>();
        let right = placed
            .iter()
            .map(|page| page.bounds.maxs[0])
            .fold(0.0, f64::max);
        let (doc_width, row_height) = if prefs.adjust_document {
            let height = placed
                .iter()
                .map(|page| page.bounds.extents()[1])
                .fold(0.0, f64::max);
            (right, Some(height))
        } else {
            let row_height = matches!(prefs.page_spacing, PdfImportPageSpacing::OnePerDocumentPage)
                .then(|| self.0.format.height());
            (self.0.format.width(), row_height)
        };
        Some(PreviewLayout {
            pages: placed,
            doc_width,
            row_height,
        })
    }

    fn draw_thumbnail(&self, index: usize) {
        let pdf = Arc::clone(&self.0.pdf);
        let (sender, receiver) = futures::channel::oneshot::channel::<Option<Thumbnail>>();
        rayon::spawn(move || {
            let thumbnail = pdf.pages().get(index).map(|page| {
                let scale = (THUMBNAIL_WIDTH / page.render_dimensions().0 as f64) as f32;
                let settings = hayro::RenderSettings {
                    x_scale: scale,
                    y_scale: scale,
                    width: None,
                    height: None,
                    bg_color: hayro::vello_cpu::color::AlphaColor::WHITE,
                };
                let pixmap = hayro::render(page, &InterpreterSettings::default(), &settings);
                // Premultiplied RGBA to cairo's premultiplied ARGB32, which is BGRA in memory on little endian
                let mut data = pixmap.data_as_u8_slice().to_vec();
                for px in data.as_chunks_mut::<4>().0 {
                    px.swap(0, 2);
                }
                Thumbnail {
                    width: pixmap.width() as i32,
                    height: pixmap.height() as i32,
                    data,
                }
            });
            let _ = sender.send(thumbnail);
        });
        let weak: Weak<Inner> = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            let Ok(Some(thumbnail)) = receiver.await else {
                return;
            };
            let Some(inner) = weak.upgrade() else {
                return;
            };
            match cairo::ImageSurface::create_for_data(
                thumbnail.data,
                cairo::Format::ARgb32,
                thumbnail.width,
                thumbnail.height,
                thumbnail.width * 4,
            ) {
                Ok(surface) => {
                    inner.thumbnails.borrow_mut().insert(index, surface);
                    inner.area.queue_draw();
                }
                Err(e) => error!("Creating the preview of Pdf page {index} failed, Err: {e:?}"),
            }
        });
    }

    fn draw(&self, area: &DrawingArea, cx: &cairo::Context, width: f64, height: f64) {
        let Some(request) = self.0.request.borrow().clone() else {
            return;
        };
        let Some(layout) = self.layout(&request, Self::shown_pages(&request)) else {
            return;
        };
        if layout.pages.is_empty() {
            return;
        }
        let fg = area.color();
        let fg_alpha = |alpha: f64| {
            cx.set_source_rgba(fg.red() as f64, fg.green() as f64, fg.blue() as f64, alpha)
        };

        let content = layout.pages.iter().fold(
            Aabb::new(Vector2::ZERO, Vector2::new(layout.doc_width, 0.0)),
            |acc, page| acc.merged(&page.bounds),
        );
        let extents = content.extents();
        let scale =
            ((width - 2.0 * PADDING) / extents[0]).min((height - 2.0 * PADDING) / extents[1]);
        if !scale.is_finite() || scale <= 0.0 {
            return;
        }
        let offset = Vector2::new(
            (width - extents[0] * scale) * 0.5 - content.mins[0] * scale,
            (height - extents[1] * scale) * 0.5 - content.mins[1] * scale,
        );
        let to_px = |v: Vector2| offset + v * scale;

        // The document: its width, and its pages when every row takes one
        let doc_top_left = to_px(Vector2::ZERO);
        let doc_bottom_right = to_px(Vector2::new(layout.doc_width, content.maxs[1]));
        cx.set_line_width(1.0);
        cx.set_dash(&[4.0, 3.0], 0.0);
        fg_alpha(0.3);
        cx.rectangle(
            doc_top_left[0].round() + 0.5,
            doc_top_left[1].round() + 0.5,
            (doc_bottom_right[0] - doc_top_left[0]).round(),
            (doc_bottom_right[1] - doc_top_left[1]).round(),
        );
        let _ = cx.stroke();
        if let Some(row_height) = layout.row_height.filter(|h| *h > 0.0) {
            let mut y = row_height;
            while y < content.maxs[1] - 0.5 {
                let py = to_px(Vector2::new(0.0, y))[1].round() + 0.5;
                cx.move_to(doc_top_left[0], py);
                cx.line_to(doc_bottom_right[0], py);
                y += row_height;
            }
            let _ = cx.stroke();
        }
        cx.set_dash(&[], 0.0);

        let thumbnails = self.0.thumbnails.borrow();
        let accent = adw::StyleManager::default().accent_color_rgba();
        for page in &layout.pages {
            let mins = to_px(page.bounds.mins);
            let maxs = to_px(page.bounds.maxs);
            let (x, y, w, h) = (mins[0], mins[1], maxs[0] - mins[0], maxs[1] - mins[1]);

            // Shadow
            for (grow, dy, alpha) in [(2.0, 2.0, 0.05), (1.0, 1.0, 0.1)] {
                rounded_rect(
                    cx,
                    x - grow,
                    y - grow + dy,
                    w + 2.0 * grow,
                    h + 2.0 * grow,
                    CORNER_RADIUS + grow,
                );
                fg_alpha(alpha);
                let _ = cx.fill();
            }

            // The page, or a placeholder while it is drawn
            let _ = cx.save();
            rounded_rect(cx, x, y, w, h, CORNER_RADIUS);
            cx.clip();
            match thumbnails.get(&page.index) {
                Some(surface) => {
                    cx.set_source_rgb(1.0, 1.0, 1.0);
                    let _ = cx.paint();
                    cx.translate(x, y);
                    cx.scale(w / surface.width() as f64, h / surface.height() as f64);
                    let _ = cx.set_source_surface(surface, 0.0, 0.0);
                    cx.source().set_filter(cairo::Filter::Good);
                    let _ = cx.paint();
                }
                None => {
                    fg_alpha(0.08);
                    let _ = cx.paint();
                }
            }
            let _ = cx.restore();

            // Page number
            let text = (page.index + 1).to_string();
            cx.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
            cx.set_font_size(10.0);
            if let Ok(extents) = cx.text_extents(&text) {
                let (bw, bh) = (extents.width() + 10.0, 15.0);
                let (bx, by) = (x + (w - bw) * 0.5, y + h - bh - 4.0);
                if h > bh + 8.0 && w > bw + 4.0 {
                    rounded_rect(cx, bx, by, bw, bh, bh * 0.5);
                    cx.set_source_rgba(
                        accent.red() as f64,
                        accent.green() as f64,
                        accent.blue() as f64,
                        0.95,
                    );
                    let _ = cx.fill();
                    cx.set_source_rgb(1.0, 1.0, 1.0);
                    cx.move_to(
                        bx + (bw - extents.width()) * 0.5 - extents.x_bearing(),
                        by + (bh - extents.height()) * 0.5 - extents.y_bearing(),
                    );
                    let _ = cx.show_text(&text);
                }
            }
        }
    }
}

fn rounded_rect(cx: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w * 0.5).min(h * 0.5);
    let degrees = std::f64::consts::PI / 180.0;
    cx.new_sub_path();
    cx.arc(x + w - r, y + r, r, -90.0 * degrees, 0.0);
    cx.arc(x + w - r, y + h - r, r, 0.0, 90.0 * degrees);
    cx.arc(x + r, y + h - r, r, 90.0 * degrees, 180.0 * degrees);
    cx.arc(x + r, y + r, r, 180.0 * degrees, 270.0 * degrees);
    cx.close_path();
}
