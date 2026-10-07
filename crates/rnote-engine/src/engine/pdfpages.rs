//! Taking the pages of the Pdfs a document keeps out of it again: as they are, or with what is drawn over them.
//!
//! The pages are never drawn again. They are lifted out of their Pdf with `hayro-write` and put into the new Pdf
//! as they are, so their text stays text. What is drawn over a page is drawn into a see-through page of its own,
//! which is laid on top.

// Imports
use crate::notefolder::FileName;
use crate::strokes::Stroke;
use crate::strokes::vectorimage::PdfPageRef;
use crate::{Drawable, Engine};
use anyhow::Context;
use futures::channel::oneshot;
use hayro::hayro_syntax::Pdf;
use hayro_write::{ExtractionQuery, ExtractionResult, extract};
use pdf_writer::{Content, Finish, Name, Rect, Ref};
use rnote_compose::shapes::{Rectangle, Shapeable};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::error;

/// What is exported of the pages of the Pdfs a document keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfPagesExport {
    /// The pages as they are in their Pdf.
    Original,
    /// The pages with what is drawn over them in the document.
    WithNotes,
}

/// A page of a kept Pdf as it lies on the document, with what is drawn over it.
#[derive(Debug, Clone)]
struct PlacedPage {
    pdf_page: PdfPageRef,
    rectangle: Rectangle,
    notes: Vec<Arc<Stroke>>,
}

/// The Pdfs the exported pages are from.
type Pdfs = HashMap<FileName, Arc<Vec<u8>>>;

impl Engine {
    /// The pages of kept Pdfs that an export takes: the selected ones, or all of the document when none is
    /// selected. With `of`, only the pages of that Pdf.
    ///
    /// They are in the order they lie on the document: top to bottom, and left to right.
    fn placed_pdf_pages(&self, of: Option<&FileName>) -> Vec<PlacedPage> {
        let kept_page = |stroke: &Stroke| match stroke {
            Stroke::VectorImage(image) => image
                .pdf_page
                .clone()
                .map(|pdf_page| (pdf_page, image.rectangle)),
            _ => None,
        };
        let pages_among = |keys: Vec<crate::store::StrokeKey>| {
            keys.into_iter()
                .filter_map(|key| kept_page(self.store.get_stroke_ref(key)?))
                .filter(|(pdf_page, _)| of.is_none_or(|file| pdf_page.file == *file))
                .collect::<Vec<(PdfPageRef, Rectangle)>>()
        };
        let mut pages = pages_among(self.store.selection_keys_as_rendered());
        if pages.is_empty() {
            pages = pages_among(self.store.stroke_keys_as_rendered());
        }
        pages.sort_by(|(_, first), (_, second)| {
            let (first, second) = (first.bounds().mins, second.bounds().mins);
            first[1]
                .total_cmp(&second[1])
                .then(first[0].total_cmp(&second[0]))
        });

        pages
            .into_iter()
            .map(|(pdf_page, rectangle)| {
                // The pages of Pdfs are what is drawn on, everything else that touches the page is drawn on it
                let keys = self
                    .store
                    .stroke_keys_as_rendered_intersecting_bounds(rectangle.bounds());
                let mut notes = self.store.get_strokes_arc(&keys);
                notes.retain(|stroke| kept_page(stroke).is_none());
                PlacedPage {
                    pdf_page,
                    rectangle,
                    notes,
                }
            })
            .collect()
    }

    /// The Pdfs whose pages [Self::export_pdf_pages] takes, in the order their first pages lie on the document.
    ///
    /// They have to be in [Self::files] for the export. A document that was loaded from a note folder does not
    /// hold them in memory.
    pub fn pdf_page_files(&self) -> Vec<FileName> {
        let mut files = Vec::new();
        for page in self.placed_pdf_pages(None) {
            if !files.contains(&page.pdf_page.file) {
                files.push(page.pdf_page.file);
            }
        }
        files
    }

    /// Exports the pages of kept Pdfs as one Pdf: the selected pages, or all when none is selected. With `of`, only
    /// the pages of that Pdf.
    ///
    /// When the pages are all pages of one Pdf in their order, [PdfPagesExport::Original] gives the Pdf itself,
    /// byte for byte.
    pub fn export_pdf_pages(
        &self,
        export: PdfPagesExport,
        of: Option<&FileName>,
    ) -> oneshot::Receiver<anyhow::Result<Vec<u8>>> {
        let (oneshot_sender, oneshot_receiver) = oneshot::channel::<anyhow::Result<Vec<u8>>>();
        let pages = self.placed_pdf_pages(of);
        let pdfs = pages
            .iter()
            .map(|page| {
                let file = &page.pdf_page.file;
                let bytes = self.files.get(file).with_context(|| {
                    format!("The Pdf {file:?} of a page is not among the files of the document.")
                })?;
                Ok((file.clone(), bytes))
            })
            .collect::<anyhow::Result<Pdfs>>();

        rayon::spawn(move || {
            let result = || -> anyhow::Result<Vec<u8>> {
                let pdfs = pdfs?;
                if pages.is_empty() {
                    return Err(anyhow::anyhow!("The document has no pages of a kept Pdf."));
                }
                match export {
                    PdfPagesExport::Original => original_pages(&pages, &pdfs),
                    PdfPagesExport::WithNotes => pages_with_notes(&pages, &pdfs),
                }
            };
            if oneshot_sender.send(result()).is_err() {
                error!(
                    "Sending result to receiver failed while exporting Pdf pages. Receiver already dropped."
                );
            }
        });

        oneshot_receiver
    }
}

fn open(bytes: &Arc<Vec<u8>>) -> anyhow::Result<Pdf> {
    Pdf::new(Arc::clone(bytes)).map_err(|e| anyhow::anyhow!("Reading the Pdf failed, Err: {e:?}"))
}

/// The references of what was taken out of a Pdf, one for each query.
fn roots(extracted: &ExtractionResult) -> anyhow::Result<Vec<Ref>> {
    extracted
        .root_refs
        .iter()
        .map(|root| root.map_err(|e| anyhow::anyhow!("Taking a page out failed, Err: {e:?}")))
        .collect()
}

/// A Pdf with the pages as they are in their Pdfs.
fn original_pages(pages: &[PlacedPage], pdfs: &Pdfs) -> anyhow::Result<Vec<u8>> {
    // All pages of one Pdf in their order: the Pdf itself
    if let Some(first) = pages.first() {
        let bytes = &pdfs[&first.pdf_page.file];
        let in_order = pages.iter().enumerate().all(|(i, page)| {
            page.pdf_page.file == first.pdf_page.file && page.pdf_page.page as usize == i
        });
        if in_order && open(bytes)?.pages().len() == pages.len() {
            return Ok(bytes.to_vec());
        }
    }

    let mut out = pdf_writer::Pdf::new();
    let mut next_ref = Ref::new(1);
    let catalog_ref = next_ref.bump();
    let page_tree_ref = next_ref.bump();
    let mut nodes = Vec::new();
    let mut n_pages = 0;
    // The pages of one Pdf that follow each other are taken out together, and are one node of the page tree
    for run in pages.chunk_by(|first, second| first.pdf_page.file == second.pdf_page.file) {
        let pdf = open(&pdfs[&run[0].pdf_page.file])?;
        let queries = run
            .iter()
            .map(|page| ExtractionQuery::new_page(page.pdf_page.page as usize))
            .collect::<Vec<ExtractionQuery>>();
        let extracted = extract(&pdf, Box::new(|| next_ref.bump()), &queries)
            .map_err(|e| anyhow::anyhow!("Taking the pages out failed, Err: {e:?}"))?;
        let page_refs = roots(&extracted)?;

        n_pages += page_refs.len();
        out.pages(extracted.page_tree_parent_ref)
            .parent(page_tree_ref)
            .count(page_refs.len() as i32)
            .kids(page_refs);
        out.extend(&extracted.chunk);
        nodes.push(extracted.page_tree_parent_ref);
    }
    out.catalog(catalog_ref).pages(page_tree_ref);
    out.pages(page_tree_ref).count(n_pages as i32).kids(nodes);
    Ok(out.finish())
}

/// The size of a page as it is shown, in points.
#[derive(Debug, Clone, Copy)]
struct PageSize {
    width: f32,
    height: f32,
}

/// A Pdf with the pages as they are in their Pdfs, each with its notes on top.
fn pages_with_notes(pages: &[PlacedPage], pdfs: &Pdfs) -> anyhow::Result<Vec<u8>> {
    let opened = pdfs
        .iter()
        .map(|(file, bytes)| Ok((file, open(bytes)?)))
        .collect::<anyhow::Result<HashMap<&FileName, Pdf>>>()?;
    let sizes = pages
        .iter()
        .map(|page| {
            let pdf_pages = opened[&page.pdf_page.file].pages();
            let pdf_page = pdf_pages
                .get(page.pdf_page.page as usize)
                .context("The Pdf has no such page.")?;
            let (width, height) = pdf_page.render_dimensions();
            Ok(PageSize { width, height })
        })
        .collect::<anyhow::Result<Vec<PageSize>>>()?;
    let notes = open(&Arc::new(notes_pages(pages, &sizes)?))?;

    let mut out = pdf_writer::Pdf::new();
    let mut next_ref = Ref::new(1);
    let catalog_ref = next_ref.bump();
    let page_tree_ref = next_ref.bump();

    // Both a page and its notes are put in as they are, as form XObjects. The pages of one Pdf are taken out
    // together, so that what they share, such as fonts, is in the new Pdf once.
    let mut files = Vec::new();
    for page in pages {
        if !files.contains(&&page.pdf_page.file) {
            files.push(&page.pdf_page.file);
        }
    }
    let mut page_xobjects = vec![None; pages.len()];
    for file in files {
        let of_file = pages
            .iter()
            .enumerate()
            .filter(|(_, page)| page.pdf_page.file == *file);
        let (indices, queries): (Vec<usize>, Vec<ExtractionQuery>) = of_file
            .map(|(i, page)| (i, ExtractionQuery::new_xobject(page.pdf_page.page as usize)))
            .unzip();
        let extracted = extract(&opened[file], Box::new(|| next_ref.bump()), &queries)
            .map_err(|e| anyhow::anyhow!("Taking the pages out failed, Err: {e:?}"))?;
        for (i, xobject) in indices.into_iter().zip(roots(&extracted)?) {
            page_xobjects[i] = Some(xobject);
        }
        out.extend(&extracted.chunk);
    }
    let page_xobjects = page_xobjects
        .into_iter()
        .collect::<Option<Vec<Ref>>>()
        .context("A page was not taken out of its Pdf.")?;
    let notes_queries = (0..pages.len())
        .map(ExtractionQuery::new_xobject)
        .collect::<Vec<ExtractionQuery>>();
    let extracted_notes = extract(&notes, Box::new(|| next_ref.bump()), &notes_queries)
        .map_err(|e| anyhow::anyhow!("Taking the notes out failed, Err: {e:?}"))?;
    out.extend(&extracted_notes.chunk);

    let mut page_refs = Vec::new();
    for ((page_xobject, notes_xobject), size) in page_xobjects
        .into_iter()
        .zip(roots(&extracted_notes)?)
        .zip(sizes)
    {
        let mut content = Content::new();
        content.x_object(Name(b"Page"));
        content.x_object(Name(b"Notes"));
        let content = content.finish();

        let page_ref = next_ref.bump();
        let content_ref = next_ref.bump();
        let mut page = out.page(page_ref);
        page.media_box(Rect::new(0.0, 0.0, size.width, size.height));
        page.parent(page_tree_ref);
        page.contents(content_ref);
        page.resources()
            .x_objects()
            .pair(Name(b"Page"), page_xobject)
            .pair(Name(b"Notes"), notes_xobject);
        page.finish();
        out.stream(content_ref, content.as_slice());
        page_refs.push(page_ref);
    }
    out.catalog(catalog_ref).pages(page_tree_ref);
    out.pages(page_tree_ref)
        .count(page_refs.len() as i32)
        .kids(page_refs);
    Ok(out.finish())
}

/// A Pdf with one see-through page for each page: its notes, drawn where they are on the page.
fn notes_pages(pages: &[PlacedPage], sizes: &[PageSize]) -> anyhow::Result<Vec<u8>> {
    let surface = cairo::PdfSurface::for_stream(1.0, 1.0, Vec::<u8>::new())
        .context("Creating the Pdf surface for the notes failed.")?;
    {
        let cx = cairo::Context::new(&surface)?;
        for (page, size) in pages.iter().zip(sizes) {
            let (width, height) = (f64::from(size.width), f64::from(size.height));
            surface.set_size(width, height)?;
            cx.save()?;
            cx.rectangle(0.0, 0.0, width, height);
            cx.clip();

            // From the document onto the page: the place of the page on the document, backwards
            let half_extents = page.rectangle.cuboid.half_extents;
            let to_page = page.rectangle.affine.inverse();
            cx.scale(
                width / (2.0 * half_extents[0]),
                height / (2.0 * half_extents[1]),
            );
            cx.translate(half_extents[0], half_extents[1]);
            cx.transform(cairo::Matrix::new(
                to_page.matrix2.x_axis.x,
                to_page.matrix2.x_axis.y,
                to_page.matrix2.y_axis.x,
                to_page.matrix2.y_axis.y,
                to_page.translation.x,
                to_page.translation.y,
            ));
            for stroke in page.notes.iter() {
                stroke.draw_to_cairo(&cx, Engine::STROKE_EXPORT_IMAGE_SCALE)?;
            }

            cx.restore()?;
            cx.show_page()?;
        }
    }
    let bytes = surface
        .finish_output_stream()
        .map_err(|e| anyhow::anyhow!("Finishing the Pdf of the notes failed, Err: {e:?}"))?;
    bytes
        .downcast::<Vec<u8>>()
        .map(|bytes| *bytes)
        .map_err(|_| anyhow::anyhow!("The Pdf of the notes was not written into memory."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strokes::imagetext;
    use crate::strokes::{BrushStroke, Content as _};
    use hayro::hayro_interpret::InterpreterSettings;
    use p2d::math::Vector2;
    use rnote_compose::Style;
    use rnote_compose::penpath::Element;

    /// A Pdf with a page for each of the texts.
    fn pdf_with_pages(texts: &[&str]) -> Vec<u8> {
        let surface = cairo::PdfSurface::for_stream(300.0, 400.0, Vec::<u8>::new()).unwrap();
        {
            let cx = cairo::Context::new(&surface).unwrap();
            cx.set_font_size(24.0);
            for text in texts {
                cx.move_to(40.0, 80.0);
                cx.show_text(text).unwrap();
                cx.show_page().unwrap();
            }
        }
        let bytes = surface.finish_output_stream().unwrap();
        *bytes.downcast::<Vec<u8>>().unwrap()
    }

    /// Imports the Pdf at the position, as the app does. No page is selected afterwards.
    fn import(engine: &mut Engine, pdf: &[u8], pos: Vector2) {
        let pages = engine.generate_pdf_pages_from_bytes(pdf.to_vec(), pos, None, None);
        let pages = futures::executor::block_on(pages).unwrap().unwrap();
        // Not through `import_generated_content()`: it starts rendering in the background, which would still
        // run when the test process ends
        for (stroke, layer) in pages {
            engine.store.insert_stroke(stroke, layer);
        }
    }

    fn export(engine: &Engine, export: PdfPagesExport, of: Option<&FileName>) -> Vec<u8> {
        futures::executor::block_on(engine.export_pdf_pages(export, of))
            .unwrap()
            .unwrap()
    }

    /// The text of each page of the Pdf.
    fn page_texts(pdf: &[u8]) -> Vec<String> {
        let pdf = open(&Arc::new(pdf.to_vec())).unwrap();
        let settings = InterpreterSettings::default();
        pdf.pages()
            .iter()
            .map(|page| {
                let lines = imagetext::pdf_page_text(page, &settings);
                let texts = lines.into_iter().map(|line| line.text);
                texts.collect::<Vec<String>>().join(" ")
            })
            .collect()
    }

    /// Whether the first page of the Pdf, drawn, is dark close to the position in points.
    fn is_dark_near(pdf: &[u8], pos: Vector2) -> bool {
        let pdf = open(&Arc::new(pdf.to_vec())).unwrap();
        let settings = hayro::RenderSettings {
            x_scale: 2.0,
            y_scale: 2.0,
            bg_color: hayro::vello_cpu::color::AlphaColor::WHITE,
            ..Default::default()
        };
        let pixmap = hayro::render(&pdf.pages()[0], &InterpreterSettings::default(), &settings);
        let (x, y) = ((pos[0] * 2.0) as i32, (pos[1] * 2.0) as i32);
        (-3..=3)
            .any(|dy| (-3..=3).any(|dx| pixmap.sample((x + dx) as u16, (y + dy) as u16).r < 128))
    }

    /// Selects the pages of the document that show the given pages of their Pdf, and no others.
    fn select_pages(engine: &mut Engine, pages: &[u32]) {
        for key in engine.store.stroke_keys_unordered() {
            let selected = match engine.store.get_stroke_ref(key) {
                Some(Stroke::VectorImage(image)) => image
                    .pdf_page
                    .as_ref()
                    .is_some_and(|pdf_page| pages.contains(&pdf_page.page)),
                _ => false,
            };
            engine.store.set_selected(key, selected);
        }
    }

    #[test]
    fn all_pages_of_a_pdf_give_the_pdf_itself() {
        let pdf = pdf_with_pages(&["one", "two", "three"]);
        let mut engine = Engine::default();
        import(&mut engine, &pdf, Vector2::ZERO);

        assert_eq!(engine.pdf_page_files(), [FileName::of(&pdf, "pdf")]);
        assert_eq!(export(&engine, PdfPagesExport::Original, None), pdf);
    }

    #[test]
    fn selected_pages_are_lifted_out_in_the_order_they_lie_on_the_document() {
        let pdf = pdf_with_pages(&["one", "two", "three"]);
        let mut engine = Engine::default();
        import(&mut engine, &pdf, Vector2::ZERO);

        select_pages(&mut engine, &[2, 0]);
        let exported = export(&engine, PdfPagesExport::Original, None);
        assert_ne!(exported, pdf);
        assert_eq!(page_texts(&exported), ["one", "three"]);
    }

    #[test]
    fn pages_of_two_pdfs_come_as_one_pdf_or_as_their_own() {
        let (first, second) = (pdf_with_pages(&["one", "two"]), pdf_with_pages(&["other"]));
        let mut engine = Engine::default();
        import(&mut engine, &first, Vector2::ZERO);
        import(&mut engine, &second, Vector2::new(0.0, 5000.0));
        let files = [FileName::of(&first, "pdf"), FileName::of(&second, "pdf")];
        assert_eq!(engine.pdf_page_files(), files);

        let together = export(&engine, PdfPagesExport::Original, None);
        assert_eq!(page_texts(&together), ["one", "two", "other"]);
        assert_eq!(
            export(&engine, PdfPagesExport::Original, Some(&files[0])),
            first
        );
        assert_eq!(
            export(&engine, PdfPagesExport::Original, Some(&files[1])),
            second
        );
    }

    #[test]
    fn notes_are_laid_on_top_of_the_pages_as_they_are() {
        let pdf = pdf_with_pages(&["one", "two"]);
        let mut engine = Engine::default();
        import(&mut engine, &pdf, Vector2::ZERO);
        // A stroke on the first page, above its text, and one far away from both pages
        for pos in [Vector2::new(50.0, 50.0), Vector2::new(9000.0, 9000.0)] {
            let mut stroke = BrushStroke::new(Element::new(pos, 0.5), Style::default());
            stroke.extend_w_segments([rnote_compose::penpath::Segment::LineTo {
                end: Element::new(pos + Vector2::new(60.0, 0.0), 0.5),
            }]);
            stroke.update_geometry();
            engine
                .store
                .insert_stroke(Stroke::BrushStroke(stroke), None);
        }

        let pages = engine.placed_pdf_pages(None);
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].notes.len(), 1);
        assert_eq!(pages[1].notes.len(), 0);

        let exported = export(&engine, PdfPagesExport::WithNotes, None);
        // The pages are the pages of the Pdf: their text is still text, and they have their size
        assert_eq!(page_texts(&exported), ["one", "two"]);

        // The stroke is drawn where it is on the page: the page lies on the document as large as its image is,
        // and the middle of the stroke is at (80, 50) of the document
        let page_bounds = pages[0].rectangle.bounds();
        let on_page = |x: f64, y: f64| {
            let (mins, extents) = (page_bounds.mins, page_bounds.extents());
            Vector2::new(
                (x - mins[0]) * 300.0 / extents[0],
                (y - mins[1]) * 400.0 / extents[1],
            )
        };
        let middle = on_page(80.0, 50.0);
        assert!(is_dark_near(&exported, middle));
        assert!(!is_dark_near(&pdf, middle));
        assert!(!is_dark_near(&exported, on_page(80.0, 20.0)));

        let exported = open(&Arc::new(exported)).unwrap();
        let original = open(&Arc::new(pdf)).unwrap();
        assert_eq!(
            exported.pages()[0].render_dimensions(),
            original.pages()[0].render_dimensions()
        );
    }

    #[test]
    fn a_document_without_kept_pdfs_has_nothing_to_export() {
        let engine = Engine::default();
        assert!(engine.pdf_page_files().is_empty());
        let exported = engine.export_pdf_pages(PdfPagesExport::Original, None);
        assert!(futures::executor::block_on(exported).unwrap().is_err());
    }
}
