//! The recognised text of a note, kept as files with the note.
//!
//! A note folder holds the text that was read from its units in its `text` directory, one file for each unit. A
//! device that gets the note finds the text there and does not have to read it again.
//!
//! A file is named by the hash of its unit and by [TEXT_VERSION], and is written once. Two devices that read the
//! same unit write the same name, so the files of a note that is synced between devices do not conflict.

// Imports
use crate::Line;
use anyhow::Context;
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

/// To be raised whenever the recognition models change. Text that was read by another version is not used, and is
/// left alone for the devices that run that version.
pub const TEXT_VERSION: u32 = 1;

fn file_name(unit_hash: u64) -> String {
    format!("{unit_hash:016x}.{TEXT_VERSION}.json")
}

/// The hash of the unit a file of this version is for. `None` for other files.
fn unit_hash_of(file_name: &str) -> Option<u64> {
    let stem = file_name.strip_suffix(&format!(".{TEXT_VERSION}.json"))?;
    (stem.len() == 16)
        .then(|| u64::from_str_radix(stem, 16).ok())
        .flatten()
}

/// Whether the file holds text that another version read.
fn is_of_other_version(file_name: &str) -> bool {
    let mut parts = file_name.split('.');
    let is_hash = |part: &str| part.len() == 16 && part.chars().all(|c| c.is_ascii_hexdigit());
    let is_version = |part: &str| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit());
    parts.next().is_some_and(is_hash)
        && parts.next().is_some_and(is_version)
        && parts.next() == Some("json")
        && parts.next().is_none()
        && unit_hash_of(file_name).is_none()
}

/// The lines that were read from the unit, when they are kept in the directory.
pub fn read(dir: &Path, unit_hash: u64) -> Option<Vec<Line>> {
    let bytes = std::fs::read(dir.join(file_name(unit_hash))).ok()?;
    // A file that can not be read is one a sync tool has not brought all of yet
    serde_json::from_slice(&bytes).ok()
}

/// Keeps the lines that were read from the unit in the directory. A unit whose lines are kept already is left as
/// it is.
pub fn write(dir: &Path, unit_hash: u64, lines: &[Line]) -> anyhow::Result<()> {
    let file = dir.join(file_name(unit_hash));
    if file.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(dir)
        .with_context(|| format!("Creating the directory {dir:?} failed."))?;
    // Written under another name first, so that the file is there completely or not at all
    let mut tmp_file = tempfile::NamedTempFile::new_in(dir)?;
    tmp_file.write_all(&serde_json::to_vec(lines)?)?;
    tmp_file
        .persist(&file)
        .with_context(|| format!("Writing the text file {file:?} failed."))?;
    Ok(())
}

/// Removes the files of units that the note no longer has. `kept` are the hashes of its units.
///
/// The files of other versions stay: the devices that run those versions use them. Everything else that is not
/// the text of a kept unit goes, such as the copies sync tools make of files they think conflict.
pub fn retain(dir: &Path, kept: &HashSet<u64>) -> anyhow::Result<usize> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        // No text was kept yet
        return Ok(0);
    };
    let mut removed = Vec::<PathBuf>::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        let is_kept = unit_hash_of(&name).is_some_and(|hash| kept.contains(&hash));
        if !is_kept && !is_of_other_version(&name) && entry.path().is_file() {
            removed.push(entry.path());
        }
    }
    for file in removed.iter() {
        std::fs::remove_file(file)?;
    }
    Ok(removed.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Bounds, CharBox};

    fn line(text: &str) -> Line {
        Line {
            bounds: Bounds {
                x: 1.0,
                y: 2.0,
                w: 30.0,
                h: 12.0,
            },
            chars: text
                .chars()
                .enumerate()
                .map(|(i, ch)| CharBox::exact(ch, i as f64, i as f64 + 1.0))
                .collect(),
        }
    }

    #[test]
    fn text_is_kept_once_and_read_back() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("text");
        assert_eq!(read(&dir, 7), None);

        write(&dir, 7, &[line("老師"), line("abc")]).unwrap();
        assert_eq!(read(&dir, 7), Some(vec![line("老師"), line("abc")]));
        assert_eq!(read(&dir, 8), None);
        // What is kept is not written again
        write(&dir, 7, &[line("other")]).unwrap();
        assert_eq!(read(&dir, 7).unwrap()[0].text(), "老師");
        // A file a sync tool has brought half of
        std::fs::write(dir.join(file_name(9)), b"[{\"bounds\"").unwrap();
        assert_eq!(read(&dir, 9), None);
    }

    #[test]
    fn the_text_of_units_that_are_gone_is_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("text");
        assert_eq!(retain(&dir, &HashSet::new()).unwrap(), 0);
        for hash in [1, 2, 3] {
            write(&dir, hash, &[line("text")]).unwrap();
        }
        // Text another version read, and the copy a sync tool made
        let other_version = format!("{:016x}.{}.json", 1, TEXT_VERSION + 1);
        let conflict_copy = format!("{:016x}.{TEXT_VERSION} (1).json", 2);
        std::fs::write(dir.join(&other_version), b"[]").unwrap();
        std::fs::write(dir.join(&conflict_copy), b"[]").unwrap();

        assert_eq!(retain(&dir, &HashSet::from([2, 4])).unwrap(), 3);
        assert_eq!(read(&dir, 1), None);
        assert!(read(&dir, 2).is_some());
        assert_eq!(read(&dir, 3), None);
        assert!(dir.join(other_version).exists());
        assert!(!dir.join(conflict_copy).exists());
    }
}
