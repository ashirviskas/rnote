// Imports
use crate::cli;
use anyhow::Context;
use rnote_engine::Engine;
use rnote_engine::engine::PdfPagesExport;
use rnote_engine::notefolder::NoteFolder;
use std::path::Path;
use std::sync::Arc;

pub(crate) async fn run_pdf_pages(
    note: &Path,
    output: &Path,
    original: bool,
) -> anyhow::Result<()> {
    let dir = NoteFolder::folder_of(note).with_context(|| {
        format!(
            "\"{}\" is not a note folder. Only note folders keep Pdfs.",
            note.display()
        )
    })?;
    let mut engine = Engine::default();
    let _ = engine.load_snapshot(cli::load_note(&dir).await?);
    // The Pdfs are read from the note folder when they are needed
    for file in engine.pdf_page_files() {
        let bytes = NoteFolder::read_file(&dir, &file)?;
        engine.files.insert(Arc::new(bytes), file.extension());
    }

    let export = if original {
        PdfPagesExport::Original
    } else {
        PdfPagesExport::WithNotes
    };
    let bytes = engine.export_pdf_pages(export, None).await??;
    cli::create_overwrite_file_w_bytes(output, &bytes).await?;
    println!(
        "Exported the Pdf pages of \"{}\" to \"{}\".",
        note.display(),
        output.display()
    );
    Ok(())
}
