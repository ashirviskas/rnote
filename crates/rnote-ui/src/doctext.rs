//! The text of an open document: what search looks through right away, and what a text selection copies.

// Imports
use crate::RnCanvas;
use crate::appwindow::RnAppWindow;
use gettextrs::gettext;
use gtk4::prelude::*;
use p2d::bounding_volume::Aabb;
use rnote_compose::SplitOrder;
use rnote_ocr::{Bounds, CharBox, Index, Line};
use std::path::PathBuf;
use tracing::{debug, error};

/// A line of text an open document carries.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DocumentLine {
    /// The index of the page the line is on.
    pub(crate) page: u32,
    pub(crate) line: Line,
}

/// The path the index knows the document by. A document that was never saved has none.
pub(crate) fn index_path(canvas: &RnCanvas) -> Option<PathBuf> {
    canvas
        .output_file()
        .and_then(|file| file.path())
        .map(|path| path.canonicalize().unwrap_or(path))
}

/// The lines of text the document carries: typed text and the text of imported Pdf pages. They need no
/// recognition, so they are known whether the document was saved or not.
pub(crate) fn document_lines(canvas: &RnCanvas) -> Vec<DocumentLine> {
    let mut lines = Vec::new();
    for unit in canvas
        .engine_ref()
        .extract_text_units(SplitOrder::default())
    {
        for stroke in unit.content.strokes.iter() {
            let text_lines = stroke.text_lines().unwrap_or_else(|e| {
                error!("Getting the text of a stroke failed, Err: {e:?}");
                Vec::new()
            });
            for text_line in text_lines {
                let extents = text_line.bounds.extents();
                lines.push(DocumentLine {
                    page: unit.page as u32,
                    line: Line {
                        bounds: Bounds {
                            x: text_line.bounds.mins.x,
                            y: text_line.bounds.mins.y,
                            w: extents.x,
                            h: extents.y,
                        },
                        chars: text_line
                            .chars
                            .iter()
                            .map(|c| CharBox::exact(c.ch, c.bounds.mins.x, c.bounds.maxs.x))
                            .collect(),
                    },
                });
            }
        }
    }
    lines
}

/// Whether both bounds are the same piece of text: once from the open document and once from the index, which
/// knows the typed text and Pdf text of a saved document too.
pub(crate) fn same_place(a: Bounds, b: Bounds) -> bool {
    (a.x - b.x).abs() < 1.0 && (a.y - b.y).abs() < 1.0 && (a.w - b.w).abs() < 1.0
}

/// Copies the text inside the area of the document to the clipboard, and tells what happened with a toast.
///
/// The text the document carries is taken from the document as it is right now. Handwriting and images are
/// taken from the index, as they were read when the document was last saved.
pub(crate) async fn copy_text(appwindow: &RnAppWindow, canvas: &RnCanvas, area: Aabb) {
    let mut lines = document_lines(canvas)
        .into_iter()
        .map(|document_line| document_line.line)
        .collect::<Vec<Line>>();
    let path = index_path(canvas);
    if let Some(path) = path.clone() {
        match blocking::unblock(move || Index::open()?.lines(&path)).await {
            Ok(indexed) => {
                let in_document = lines.len();
                for line in indexed {
                    let known = lines[..in_document]
                        .iter()
                        .any(|known| same_place(known.bounds, line.bounds));
                    if !known {
                        lines.push(line);
                    }
                }
            }
            Err(e) => error!("Getting the lines of the document from the index failed, Err: {e:?}"),
        }
    }

    let extents = area.extents();
    let text = rnote_ocr::select::text_in(
        &lines,
        Bounds {
            x: area.mins.x,
            y: area.mins.y,
            w: extents.x,
            h: extents.y,
        },
    );
    let message = if !text.is_empty() {
        debug!("Copying the text of the text selection: {text:?}");
        appwindow.clipboard().set_text(&text);
        gettext("Text copied")
    } else if crate::indexer::is_busy() {
        gettext("No text here yet. Notes are still being read in the background.")
    } else if path.is_none() {
        gettext("No text here. Handwriting and images can be copied once the document is saved.")
    } else {
        gettext("No text in the selection")
    };
    appwindow
        .overlays()
        .dispatch_toast_text(&message, crate::overlays::TEXT_TOAST_TIMEOUT_DEFAULT);
}
