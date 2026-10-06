use rnote_engine::notefolder::NoteFolder;
use std::path::Path;

/// A note is a rnote file, a note folder, or the entry file of a note folder.
pub(crate) fn path_is_note(path: &Path) -> anyhow::Result<()> {
    if NoteFolder::folder_of(path).is_some() {
        return Ok(());
    }
    file_has_ext(path, "rnote")
}

pub(crate) fn path_is_dir(path: &Path) -> anyhow::Result<()> {
    if !path.is_dir() {
        return Err(anyhow::anyhow!(
            "Expected directory, found file \"{}\"",
            path.display()
        ));
    }
    Ok(())
}

pub(crate) fn path_is_file(path: &Path) -> anyhow::Result<()> {
    if !path.is_file() {
        return Err(anyhow::anyhow!(
            "Expected file, found directory \"{}\"",
            path.display()
        ));
    }
    Ok(())
}

pub(crate) fn file_has_ext(path: &Path, expected_ext: &str) -> anyhow::Result<()> {
    path_is_file(path)?;
    match path.extension() {
        Some(ext) if ext == expected_ext => Ok(()),
        Some(ext) => Err(anyhow::anyhow!(
            "Expected file with extension \"{expected_ext}\", found extension \"{ext:?}\", file \"{}\".",
            path.display()
        )),
        None => Err(anyhow::anyhow!(
            "Expected file with extension \"{expected_ext}\", no extension found for file \"{}\".",
            path.display()
        )),
    }
}
