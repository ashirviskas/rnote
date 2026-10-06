//! Takes pages out of a Pdf without drawing them again: as they are, and with ink on top of them.
//!
//! Usage: `cargo run -p rnote-engine --example pdf_pages -- <file.pdf> <out-dir> <page>...` (pages count from 1)
//!
//! Writes `original_pages.pdf` and `with_ink.pdf` into the directory. The ink is a stroke and a highlighter bar.

use anyhow::Context;
use hayro::hayro_syntax::Pdf;
use hayro_write::{ExtractionQuery, ExtractionResult, extract};
use pdf_writer::{Content, Finish, Name, Rect, Ref};
use std::path::PathBuf;
use std::sync::Arc;

/// The size of a page as it is shown, in points.
#[derive(Debug, Clone, Copy)]
struct PageSize {
    width: f32,
    height: f32,
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let usage = "Usage: pdf_pages <file.pdf> <out-dir> <page>...";
    let pdf_file = PathBuf::from(args.next().context(usage)?);
    let out_dir = PathBuf::from(args.next().context(usage)?);
    let pages = args
        .map(|page| Ok(page.to_string_lossy().parse::<usize>()?.saturating_sub(1)))
        .collect::<anyhow::Result<Vec<usize>>>()?;
    let pdf = open(std::fs::read(pdf_file)?)?;
    std::fs::create_dir_all(&out_dir)?;

    std::fs::write(
        out_dir.join("original_pages.pdf"),
        original_pages(&pdf, &pages)?,
    )?;
    std::fs::write(out_dir.join("with_ink.pdf"), with_ink(&pdf, &pages)?)?;
    Ok(())
}

fn open(bytes: Vec<u8>) -> anyhow::Result<Pdf> {
    Pdf::new(Arc::new(bytes)).map_err(|e| anyhow::anyhow!("Reading the Pdf failed, Err: {e:?}"))
}

/// The references of what the queries asked for.
fn roots(extracted: &ExtractionResult) -> anyhow::Result<Vec<Ref>> {
    extracted
        .root_refs
        .iter()
        .map(|root| root.map_err(|e| anyhow::anyhow!("Taking a page out failed, Err: {e:?}")))
        .collect()
}

/// A Pdf with the given pages of the Pdf, as they are.
fn original_pages(pdf: &Pdf, pages: &[usize]) -> anyhow::Result<Vec<u8>> {
    let mut out = pdf_writer::Pdf::new();
    let mut next_ref = Ref::new(1);
    let catalog_ref = next_ref.bump();

    let queries = pages
        .iter()
        .map(|&page| ExtractionQuery::new_page(page))
        .collect::<Vec<ExtractionQuery>>();
    let extracted = extract(pdf, Box::new(|| next_ref.bump()), &queries)
        .map_err(|e| anyhow::anyhow!("Taking the pages out failed, Err: {e:?}"))?;
    let page_refs = roots(&extracted)?;

    out.catalog(catalog_ref)
        .pages(extracted.page_tree_parent_ref);
    out.pages(extracted.page_tree_parent_ref)
        .count(page_refs.len() as i32)
        .kids(page_refs);
    out.extend(&extracted.chunk);
    Ok(out.finish())
}

/// A Pdf with the given pages of the Pdf, each with ink drawn on top of it.
fn with_ink(pdf: &Pdf, pages: &[usize]) -> anyhow::Result<Vec<u8>> {
    let sizes = pages
        .iter()
        .map(|&page| {
            let page = pdf.pages().get(page).context("No such page")?;
            let (width, height) = page.render_dimensions();
            Ok(PageSize { width, height })
        })
        .collect::<anyhow::Result<Vec<PageSize>>>()?;
    // The ink is drawn into a Pdf of its own, one see-through page for each page
    let ink = open(ink_pages(&sizes)?)?;

    let mut out = pdf_writer::Pdf::new();
    let mut next_ref = Ref::new(1);
    let catalog_ref = next_ref.bump();
    let page_tree_ref = next_ref.bump();

    // Both the page and its ink are placed as they are, as form XObjects
    let page_queries = pages
        .iter()
        .map(|&page| ExtractionQuery::new_xobject(page))
        .collect::<Vec<ExtractionQuery>>();
    let ink_queries = (0..pages.len())
        .map(ExtractionQuery::new_xobject)
        .collect::<Vec<ExtractionQuery>>();
    let extracted_pages = extract(pdf, Box::new(|| next_ref.bump()), &page_queries)
        .map_err(|e| anyhow::anyhow!("Taking the pages out failed, Err: {e:?}"))?;
    let extracted_ink = extract(&ink, Box::new(|| next_ref.bump()), &ink_queries)
        .map_err(|e| anyhow::anyhow!("Taking the ink out failed, Err: {e:?}"))?;

    let mut page_refs = Vec::new();
    for ((page_xobject, ink_xobject), size) in roots(&extracted_pages)?
        .into_iter()
        .zip(roots(&extracted_ink)?)
        .zip(sizes)
    {
        let mut content = Content::new();
        content.x_object(Name(b"Page"));
        content.x_object(Name(b"Ink"));
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
            .pair(Name(b"Ink"), ink_xobject);
        page.finish();
        out.stream(content_ref, content.as_slice());
        page_refs.push(page_ref);
    }

    out.catalog(catalog_ref).pages(page_tree_ref);
    out.pages(page_tree_ref)
        .count(page_refs.len() as i32)
        .kids(page_refs);
    out.extend(&extracted_pages.chunk);
    out.extend(&extracted_ink.chunk);
    Ok(out.finish())
}

/// A Pdf with one page of ink for each size: a stroke, and a highlighter bar that lets the page shine through.
fn ink_pages(sizes: &[PageSize]) -> anyhow::Result<Vec<u8>> {
    let surface = cairo::PdfSurface::for_stream(1.0, 1.0, Vec::<u8>::new())?;
    {
        let cx = cairo::Context::new(&surface)?;
        for size in sizes {
            let (width, height) = (size.width as f64, size.height as f64);
            surface.set_size(width, height)?;

            cx.set_source_rgba(1.0, 0.9, 0.0, 0.4);
            cx.rectangle(0.1 * width, 0.2 * height, 0.8 * width, 0.05 * height);
            cx.fill()?;

            cx.set_source_rgba(0.85, 0.1, 0.1, 1.0);
            cx.set_line_width(3.0);
            cx.set_line_cap(cairo::LineCap::Round);
            cx.move_to(0.1 * width, 0.5 * height);
            cx.curve_to(
                0.3 * width,
                0.3 * height,
                0.6 * width,
                0.7 * height,
                0.9 * width,
                0.5 * height,
            );
            cx.stroke()?;

            cx.show_page()?;
        }
    }
    let bytes = surface
        .finish_output_stream()
        .map_err(|e| anyhow::anyhow!("Finishing the ink Pdf failed, Err: {e:?}"))?;
    bytes
        .downcast::<Vec<u8>>()
        .map(|bytes| *bytes)
        .map_err(|_| anyhow::anyhow!("The ink Pdf was not written into memory."))
}
