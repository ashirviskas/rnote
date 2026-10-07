//! A note folder as one file, for sending a note to someone.
//!
//! The file is a zip of a note folder that holds the note and nothing else: one device's batches, no records of
//! strokes that were removed, and the files the note refers to.

// Imports
use super::{Device, NoteFolder};
use crate::engine::EngineSnapshot;
use anyhow::Context;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use zip::write::SimpleFileOptions;

impl NoteFolder {
    /// The ending of the name of a packed note.
    pub const PACKED_EXTENSION: &'static str = "rnotez";

    /// Packs the note folder at the directory into one file.
    pub fn pack(dir: &Path, device: Device, packed_file: &Path) -> anyhow::Result<()> {
        let (_, snapshot) = Self::load(dir, device.clone())?;
        // The files the note refers to are packed with it
        for file in snapshot.pdf_files() {
            let bytes = Self::read_file(dir, &file)?;
            snapshot.files.insert(Arc::new(bytes), file.extension());
        }
        Self::pack_snapshot(&snapshot, device, packed_file)
    }

    /// Packs the document of the snapshot into one file.
    ///
    /// The document is saved for it as this device would save it into an empty note folder. What was removed
    /// from a note is then not in the file, although batches of its folder may still hold it. A Pdf is packed
    /// when it is among the files of the snapshot; without it its pages are packed whole.
    pub fn pack_snapshot(
        snapshot: &EngineSnapshot,
        device: Device,
        packed_file: &Path,
    ) -> anyhow::Result<()> {
        let tmp_dir = tempfile::tempdir()?;
        let clean_dir = tmp_dir.path().join("note");
        Self::create(&clean_dir, device)?.save(snapshot)?;

        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::<u8>::new()));
        add_dir(&mut zip, &clean_dir, "")?;
        let packed = zip.finish()?.into_inner();
        crate::utils::atomic_save_to_file(packed_file, packed)
            .with_context(|| format!("Writing the packed note {packed_file:?} failed."))
    }

    /// Loads the note of a packed note, without keeping anything of it on disk: the Pdfs it holds are among the
    /// files of the snapshot, so that saving the snapshot into a note folder saves them too.
    pub fn load_packed(packed_file: &Path, device: Device) -> anyhow::Result<EngineSnapshot> {
        let tmp_dir = tempfile::tempdir()?;
        let dir = tmp_dir.path().join("note");
        Self::unpack(packed_file, &dir)?;
        let (_, snapshot) = Self::load(&dir, device)?;
        for file in snapshot.pdf_files() {
            let bytes = Self::read_file(&dir, &file)?;
            snapshot.files.insert(Arc::new(bytes), file.extension());
        }
        Ok(snapshot)
    }

    /// Unpacks the packed note into a new note folder at the directory.
    pub fn unpack(packed_file: &Path, dir: &Path) -> anyhow::Result<()> {
        if dir.exists() {
            return Err(anyhow::anyhow!("{dir:?} exists already."));
        }
        let file = std::fs::File::open(packed_file)
            .with_context(|| format!("Opening the packed note {packed_file:?} failed."))?;
        let mut zip = zip::ZipArchive::new(file)
            .with_context(|| format!("{packed_file:?} is not a packed note."))?;
        if zip.by_name(Self::ENTRY_FILE_NAME).is_err() {
            return Err(anyhow::anyhow!("{packed_file:?} is not a packed note."));
        }

        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            // A name that leads out of the directory is not followed
            let name = entry.enclosed_name().with_context(|| {
                format!(
                    "The packed note has a file with a bad name: {:?}",
                    entry.name()
                )
            })?;
            let path = dir.join(name);
            if entry.is_dir() {
                std::fs::create_dir_all(&path)?;
                continue;
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut bytes = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut bytes)?;
            std::fs::write(&path, bytes).with_context(|| format!("Writing {path:?} failed."))?;
        }
        Ok(())
    }
}

/// Adds the files of the directory to the zip, under the name the directory has in it.
fn add_dir(
    zip: &mut zip::ZipWriter<std::io::Cursor<Vec<u8>>>,
    dir: &Path,
    name_in_zip: &str,
) -> anyhow::Result<()> {
    let mut entries = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = format!("{name_in_zip}{}", entry.file_name().to_string_lossy());
        let path = entry.path();
        if path.is_dir() {
            add_dir(zip, &path, &format!("{name}/"))?;
            continue;
        }
        // Batches are compressed, and so is what is in most Pdfs and pictures: they are stored as they are
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file(name, options)?;
        zip.write_all(&std::fs::read(&path)?)?;
    }
    Ok(())
}
