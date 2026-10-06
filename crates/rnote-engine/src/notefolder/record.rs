//! The records a note folder is made of, and the batches they are stored in.

// Imports
use crate::store::{ChronoComponent, StrokeId};
use crate::strokes::Stroke;
use crate::strokes::vectorimage::PdfPageImage;
use crate::{Camera, Document};
use anyhow::Context;
use rand::RngExt;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Identifies an installation of the app. A device writes batches under its own id only, which is why the batches of
/// a note that is synced between devices never conflict.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeviceId(String);

impl DeviceId {
    /// Lower case letters and digits, so that the id can be part of a file name on every system.
    pub fn new(id: &str) -> anyhow::Result<Self> {
        let valid = !id.is_empty()
            && id.len() <= 16
            && id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
        if !valid {
            return Err(anyhow::anyhow!("\"{id}\" is not a valid device id."));
        }
        Ok(Self(id.to_string()))
    }

    pub fn random() -> Self {
        Self(format!("{:08x}", rand::rng().random::<u32>()))
    }

    /// The id of this installation. It is made when it is first asked for and kept in the data directory.
    pub fn of_this_installation() -> anyhow::Result<Self> {
        let file = glib::user_data_dir().join("rnote").join("device-id");
        if let Ok(id) = std::fs::read_to_string(&file) {
            return Self::new(id.trim())
                .with_context(|| format!("Reading the device id from {file:?} failed."));
        }
        let id = Self::random();
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&file, &id.0)
            .with_context(|| format!("Writing the device id to {file:?} failed."))?;
        Ok(id)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// When a record was written. Of two records about the same part of a note, the one with the higher stamp counts.
///
/// The counter is one more than the highest counter the device knew of in the note, so it needs no clocks. Stamps
/// with the same counter were written without knowing of each other and are ordered by the device.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Stamp {
    #[serde(rename = "counter")]
    pub counter: u64,
    #[serde(rename = "device")]
    pub device: DeviceId,
}

/// The part of a note a record is about.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum Subject {
    #[serde(rename = "stroke")]
    Stroke(StrokeId),
    /// The format, background and layout of the document.
    #[serde(rename = "document")]
    Document,
    /// The view of a device.
    #[serde(rename = "camera")]
    Camera(DeviceId),
}

/// What a record says about its subject.
#[derive(Debug, Clone, Deserialize)]
pub(crate) enum Content {
    #[serde(rename = "stroke")]
    Stroke {
        #[serde(rename = "chrono")]
        chrono: ChronoComponent,
        #[serde(rename = "stroke")]
        stroke: Box<Stroke>,
    },
    /// The stroke was removed. The record only exists to tell the other devices, see [super::NoteFolder::tidy].
    #[serde(rename = "removed")]
    Removed,
    #[serde(rename = "document")]
    Document(Box<Document>),
    #[serde(rename = "camera")]
    Camera(Box<Camera>),
}

/// A stroke as it is written into a record.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(untagged)]
pub(crate) enum StrokeRef<'a> {
    Whole(&'a Stroke),
    /// The image of a Pdf page, in a note folder that holds the Pdf.
    PdfPage(PdfPageStroke<'a>),
}

/// The [Stroke] of a [PdfPageImage]. Read back, it is a [Stroke::VectorImage].
#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) enum PdfPageStroke<'a> {
    #[serde(rename = "vectorimage")]
    VectorImage(PdfPageImage<'a>),
}

/// [Content] for writing. Serializes to the same.
#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) enum ContentRef<'a> {
    #[serde(rename = "stroke")]
    Stroke {
        #[serde(rename = "chrono")]
        chrono: &'a ChronoComponent,
        #[serde(rename = "stroke")]
        stroke: StrokeRef<'a>,
    },
    #[serde(rename = "removed")]
    Removed,
    #[serde(rename = "document")]
    Document(&'a Document),
    #[serde(rename = "camera")]
    Camera(&'a Camera),
}

/// A change to one part of a note. A line of a batch.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Record {
    #[serde(rename = "subject")]
    pub(crate) subject: Subject,
    #[serde(rename = "stamp")]
    pub(crate) stamp: Stamp,
    /// The [content_hash] of the content as it was written. Unchanged content is told by it without comparing.
    #[serde(rename = "hash")]
    pub(crate) hash: u64,
    #[serde(rename = "content")]
    pub(crate) content: Content,
}

/// What kind of content a record has, without reading the content.
#[derive(Debug, Clone, Copy, Deserialize)]
pub(crate) enum ContentKind {
    #[serde(rename = "stroke")]
    Stroke(serde::de::IgnoredAny),
    #[serde(rename = "removed")]
    Removed,
    #[serde(rename = "document")]
    Document(serde::de::IgnoredAny),
    #[serde(rename = "camera")]
    Camera(serde::de::IgnoredAny),
}

/// A [Record] without its content, for going through batches without building the strokes in them.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RecordHead {
    #[serde(rename = "subject")]
    pub(crate) subject: Subject,
    #[serde(rename = "stamp")]
    pub(crate) stamp: Stamp,
    #[serde(rename = "content")]
    pub(crate) content: ContentKind,
}

/// The serialized content of a record, with its hash.
#[derive(Debug, Clone)]
pub(crate) struct WrittenContent {
    json: String,
    pub(crate) hash: u64,
}

impl WrittenContent {
    pub(crate) fn new(content: ContentRef) -> anyhow::Result<Self> {
        let json = serde_json::to_string(&content)?;
        let hash = content_hash(json.as_bytes());
        Ok(Self { json, hash })
    }

    /// The line of a batch that is the record with this content.
    pub(crate) fn into_line(self, subject: &Subject, stamp: &Stamp) -> anyhow::Result<String> {
        Ok(format!(
            r#"{{"subject":{},"stamp":{},"hash":{},"content":{}}}"#,
            serde_json::to_string(subject)?,
            serde_json::to_string(stamp)?,
            self.hash,
            self.json
        ))
    }
}

/// The hash of the content of a record: FNV-1a over its serialized form.
///
/// It is written out here because it is stored in notes and compared between devices, so it must never change.
pub(crate) fn content_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// The name of a batch: the device that wrote it and a number that grows with every batch of that device.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct BatchName {
    pub(crate) device: DeviceId,
    pub(crate) seq: u32,
}

impl BatchName {
    const SUFFIX: &'static str = ".jsonl.gz";

    /// `None` for the name of a file that is no batch, such as a file that is being written or the conflict
    /// copy a sync tool made.
    fn parse(file_name: &str) -> Option<Self> {
        let (device, seq) = file_name.strip_suffix(Self::SUFFIX)?.split_once('-')?;
        if seq.len() != 6 || !seq.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        Some(Self {
            device: DeviceId::new(device).ok()?,
            seq: seq.parse().ok()?,
        })
    }

    pub(crate) fn file_name(&self) -> String {
        format!("{}-{:06}{}", self.device.as_str(), self.seq, Self::SUFFIX)
    }

    pub(crate) fn path(&self, ink_dir: &Path) -> PathBuf {
        ink_dir.join(self.file_name())
    }
}

/// The batches in the directory, sorted by device and number.
pub(crate) fn list_batches(ink_dir: &Path) -> anyhow::Result<Vec<BatchName>> {
    let mut batches = Vec::new();
    for entry in std::fs::read_dir(ink_dir)
        .with_context(|| format!("Reading the directory {ink_dir:?} failed."))?
    {
        if let Some(batch) = BatchName::parse(&entry?.file_name().to_string_lossy()) {
            batches.push(batch);
        }
    }
    batches.sort();
    Ok(batches)
}

/// The lines of a batch.
pub(crate) fn read_batch(file: &Path) -> anyhow::Result<Vec<String>> {
    let compressed = std::fs::read(file)?;
    let mut text = String::new();
    flate2::read::MultiGzDecoder::new(compressed.as_slice())
        .read_to_string(&mut text)
        .with_context(|| format!("Decompressing the batch {file:?} failed."))?;
    Ok(text.lines().map(String::from).collect())
}

/// Writes a batch with the lines. The file is there completely or not at all.
pub(crate) fn write_batch(file: &Path, lines: &[String]) -> anyhow::Result<()> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::<u8>::new(), flate2::Compression::new(5));
    for line in lines {
        encoder.write_all(line.as_bytes())?;
        encoder.write_all(b"\n")?;
    }
    crate::utils::atomic_save_to_file(file, encoder.finish()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_names_are_told_from_other_files() {
        let name = BatchName {
            device: DeviceId::new("a1b2c3d4").unwrap(),
            seq: 17,
        };
        assert_eq!(name.file_name(), "a1b2c3d4-000017.jsonl.gz");
        assert_eq!(BatchName::parse(&name.file_name()), Some(name));
        // A file that is being written, and the copies sync tools make of files they think conflict
        for other in [
            ".tmpAbC123",
            "a1b2c3d4-000017.jsonl",
            "a1b2c3d4-000017 (1).jsonl.gz",
            "a1b2c3d4-000017.sync-conflict-20261006.jsonl.gz",
            "A1B2C3D4-000017.jsonl.gz",
            "a1b2c3d4-17.jsonl.gz",
        ] {
            assert_eq!(BatchName::parse(other), None, "{other}");
        }
    }

    #[test]
    fn the_content_hash_does_not_change() {
        assert_eq!(content_hash(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(content_hash(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(content_hash(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn stamps_are_ordered_by_counter_then_device() {
        let stamp = |counter: u64, device: &str| Stamp {
            counter,
            device: DeviceId::new(device).unwrap(),
        };
        assert!(stamp(2, "aaaa") > stamp(1, "bbbb"));
        assert!(stamp(1, "bbbb") > stamp(1, "aaaa"));
    }
}
