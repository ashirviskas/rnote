// Imports
use crate::{cli, validators};
use rnote_engine::Engine;
use rnote_engine::notefolder::{Device, NoteFolder};
use std::path::Path;

pub(crate) async fn run_convert(input: &Path, output: &Path) -> anyhow::Result<()> {
    validators::path_is_note(input)?;
    if output.exists() {
        return Err(anyhow::anyhow!("\"{}\" exists already.", output.display()));
    }
    let engine_snapshot = cli::load_note(input).await?;

    if output
        .extension()
        .is_some_and(|ext| ext == NoteFolder::EXTENSION)
    {
        let mut note = NoteFolder::create(output, Device::this()?)?;
        note.save(&engine_snapshot)?;
    } else {
        let mut engine = Engine::default();
        let _ = engine.load_snapshot(engine_snapshot);
        let file_name = output
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        let rnote_bytes = engine.save_as_rnote_bytes(file_name).await??;
        cli::create_overwrite_file_w_bytes(output, &rnote_bytes).await?;
    }
    println!(
        "Converted \"{}\" to \"{}\".",
        input.display(),
        output.display()
    );
    Ok(())
}
