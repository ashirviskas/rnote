// Inspired by: https://github.com/ayykamp/rnote-thumbnailer/blob/main/src/main.rs
// Author: ayykamp <kamp@ayyy.dev>

use anyhow::{Context, anyhow};
use async_fs::File;
use core::time::Duration;
use futures::{AsyncWriteExt, FutureExt, select};
use rnote_engine::Engine;
use rnote_engine::notefolder::NoteFolder;
use smol::Timer;
use std::path::PathBuf;

pub(crate) async fn run_thumbnail(
    rnote_file: PathBuf,
    size: u32,
    output: PathBuf,
    timeout: Option<Duration>,
) -> anyhow::Result<()> {
    // A note folder carries the picture of its first page, so it needs no drawing
    if let Some(thumbnail) =
        NoteFolder::folder_of(&rnote_file).and_then(|dir| NoteFolder::thumbnail(&dir))
    {
        let image = image::open(&thumbnail)
            .with_context(|| format!("Reading the thumbnail {thumbnail:?} of the note failed."))?;
        let image = if image.width().max(image.height()) > size {
            image.thumbnail(size, size)
        } else {
            image
        };
        image.save_with_format(&output, image::ImageFormat::Png)?;
        return Ok(());
    }

    let mut engine = Engine::default();
    let engine_snapshot = crate::cli::load_note(&rnote_file).await?;

    // We dont care about the return values of these functions
    let _ = engine.load_snapshot(engine_snapshot);
    let mut timeout = if let Some(timeout) = timeout {
        Timer::after(timeout).fuse()
    } else {
        Timer::never().fuse()
    };
    let mut export_op = engine.generate_thumbnail(size).fuse();
    let export_bytes = select! {
        res = export_op => res??,
        _ = timeout => return Err(anyhow!("Timeout reached"))
    };
    let mut fh = File::create(output).await?;
    fh.write_all(&export_bytes).await?;
    fh.sync_all().await?;

    Ok(())
}
