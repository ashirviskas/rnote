//! The files of a note folder: what was imported into the note, kept as it was.

// Imports
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

/// The name of a file in the `files` directory of a note folder: the hash of its content, and its ending.
///
/// A file is written once under this name and never changed. Two devices that write the same name write the same
/// content, so these files can not conflict.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FileName(String);

impl FileName {
    pub fn of(bytes: &[u8], extension: &str) -> Self {
        Self(format!("{:032x}.{extension}", file_hash(bytes)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn extension(&self) -> &str {
        self.0
            .split_once('.')
            .map_or("", |(_, extension)| extension)
    }

    /// The name without its ending.
    pub(crate) fn stem(&self) -> &str {
        self.0.split('.').next().unwrap_or_default()
    }

    /// Where the file is in the directory. `None` for a name that is not one this type makes: names come out of
    /// records, which other devices wrote, and must not lead out of the directory.
    pub(crate) fn path_in(&self, files_dir: &Path) -> Option<PathBuf> {
        let is_plain =
            |part: &str| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric());
        let (stem, extension) = self.0.split_once('.')?;
        (is_plain(stem) && is_plain(extension)).then(|| files_dir.join(&self.0))
    }
}

/// The hash a file is named by: FNV-1a with 128 bits.
///
/// It is written out here because it names files in notes, so it must never change.
fn file_hash(bytes: &[u8]) -> u128 {
    const OFFSET_BASIS: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;
    bytes.iter().fold(OFFSET_BASIS, |hash, byte| {
        (hash ^ u128::from(*byte)).wrapping_mul(PRIME)
    })
}

/// Files that were imported into a document and are held in memory until the document is saved into a note
/// folder. All clones share the same files.
#[derive(Debug, Clone, Default)]
pub struct Files(Arc<Mutex<HashMap<FileName, Arc<Vec<u8>>>>>);

impl Files {
    /// Keeps the file. Returns the name it is known by.
    pub fn insert(&self, bytes: Arc<Vec<u8>>, extension: &str) -> FileName {
        let name = FileName::of(&bytes, extension);
        let mut files = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        files.insert(name.clone(), bytes);
        name
    }

    pub fn get(&self, name: &FileName) -> Option<Arc<Vec<u8>>> {
        let files = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        files.get(name).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_hash_does_not_change() {
        assert_eq!(
            FileName::of(b"", "pdf").as_str(),
            "6c62272e07bb014262b821756295c58d.pdf"
        );
        assert_eq!(
            FileName::of(b"a", "pdf").as_str(),
            "d228cb696f1a8caf78912b704e4a8964.pdf"
        );
    }

    #[test]
    fn a_name_can_not_lead_out_of_the_directory() {
        let dir = Path::new("/note/files");
        let name = FileName::of(b"a", "pdf");
        assert_eq!(name.path_in(dir), Some(dir.join(name.as_str())));
        assert_eq!(name.stem(), "d228cb696f1a8caf78912b704e4a8964");
        for bad in ["../secret.pdf", "a/b.pdf", "nothing", ".pdf", "a.b.c", "a."] {
            assert_eq!(FileName(bad.to_string()).path_in(dir), None, "{bad}");
        }
    }
}
