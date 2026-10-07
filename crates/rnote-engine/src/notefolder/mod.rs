//! A note as a folder of files, made for folders that are synced between devices.
//!
//! ```text
//! Lesson 1.rnoted/
//! ├── note.rnoted                   the entry file: opens the note, written once
//! ├── files/
//! │   └── 9f2c…e1.pdf               an imported Pdf as it was, named by the hash of its content
//! └── ink/
//!     ├── a1b2c3d4-000017.jsonl.gz  a batch of changes saved by the device a1b2c3d4
//!     └── 77e0f9aa-000003.jsonl.gz  a batch of changes saved by the device 77e0f9aa
//! ```
//!
//! A device only ever writes and deletes batches with its own id in their name, and a file in `files` has the
//! same content whoever writes it. So no file of a note has two writers that could disagree, and a sync tool has
//! nothing to conflict on.
//!
//! The pages of a Pdf in `files` are saved without what the canvas draws of them. Every device makes that from
//! the Pdf and keeps it in its [PageCache].
//!
//! A batch holds [records](record::Record): the strokes, the document settings and the view as they were when the
//! batch was saved, and marks for removed strokes. The note is what is left when of all records about the same
//! thing only the one with the highest [Stamp] is kept. That gives the same note on every device, in whatever
//! order the batches arrive.
//!
//! Saving writes one new batch with what changed since the last save. [NoteFolder::tidy] takes out what is no
//! longer needed.

// Modules
mod files;
mod pack;
mod pagecache;
mod record;
mod tidy;

// Re-exports
pub use files::{FileName, Files};
pub use pagecache::PageCache;
pub(crate) use record::stable_content_hash;
pub use record::{DeviceId, Stamp};
pub use tidy::TidyReport;

// Imports
use self::record::{
    BatchName, Content, ContentRef, PdfPageStroke, Record, StrokeRef, Subject, WrittenContent,
};
use crate::engine::EngineSnapshot;
use crate::store::{ChronoComponent, StrokeId, StrokeKey};
use crate::strokes::{Stroke, VectorImage};
use crate::{Camera, Document};
use anyhow::Context;
use hayro::hayro_syntax;
use serde::{Deserialize, Serialize};
use slotmap::{SecondaryMap, SlotMap};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::warn;

/// What a device brings to the note folders it opens.
#[derive(Debug, Clone)]
pub struct Device {
    pub id: DeviceId,
    pub page_cache: PageCache,
}

impl Device {
    /// This installation of the app, with its id and its page cache.
    pub fn this() -> anyhow::Result<Self> {
        Ok(Self {
            id: DeviceId::of_this_installation()?,
            page_cache: PageCache::default(),
        })
    }
}

/// The content of the entry file.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct Entry {
    #[serde(rename = "format")]
    format: u32,
}

/// The newest record about a part of the note that this device knows of.
#[derive(Debug, Clone)]
struct Known {
    hash: u64,
    part: KnownPart,
}

#[derive(Debug, Clone)]
enum KnownPart {
    Stroke(Option<SavedStroke>),
    RemovedStroke,
    Other,
}

/// A stroke as it was in the snapshot that was saved last. A stroke that is still the same allocation with the same
/// chrono component did not change, and is told without serializing it.
#[derive(Debug, Clone)]
struct SavedStroke {
    chrono: ChronoComponent,
    stroke: Arc<Stroke>,
}

/// What saving wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveReport {
    /// How many records were written.
    pub records: usize,
    /// The batch with them. `None` when nothing had changed.
    pub batch: Option<PathBuf>,
}

/// An open note folder: where it is, and what of it this device has read and written so far.
#[derive(Debug)]
pub struct NoteFolder {
    dir: PathBuf,
    device: Device,
    known: HashMap<Subject, Known>,
    /// The highest counter of the stamps in the note.
    counter: u64,
    /// The batches that were read or written.
    batches: HashSet<BatchName>,
}

impl NoteFolder {
    /// The ending of the name of a note folder, and of its entry file.
    pub const EXTENSION: &'static str = "rnoted";
    /// The file in a note folder that stands for the note where a folder can not be opened: in file managers and
    /// file dialogs.
    pub const ENTRY_FILE_NAME: &'static str = "note.rnoted";
    /// To be raised when a note folder is written in a way that older versions can not read.
    const FORMAT: u32 = 1;
    const INK_DIR_NAME: &'static str = "ink";
    const FILES_DIR_NAME: &'static str = "files";
    /// The directory of a note folder for the text that was read from the note. The engine does not read text;
    /// who does keeps it there, so that it is synced with the note.
    pub const TEXT_DIR_NAME: &'static str = "text";

    /// The note folder the path stands for: the path of a note folder itself, or of its entry file.
    pub fn folder_of(path: &Path) -> Option<PathBuf> {
        if path.is_dir() {
            return path
                .join(Self::ENTRY_FILE_NAME)
                .is_file()
                .then(|| path.to_path_buf());
        }
        let is_entry = path
            .file_name()
            .is_some_and(|name| name == Self::ENTRY_FILE_NAME);
        path.parent()
            .filter(|_| is_entry && path.is_file())
            .map(Path::to_path_buf)
    }

    /// Reads a file of the note folder at the directory. What is read is checked against the name of the file,
    /// which is the hash of what it has to hold.
    pub fn read_file(dir: &Path, file: &FileName) -> anyhow::Result<Vec<u8>> {
        let path = file
            .path_in(&dir.join(Self::FILES_DIR_NAME))
            .with_context(|| format!("{file:?} is not a valid file name."))?;
        let bytes = std::fs::read(&path)
            .with_context(|| format!("Reading the file {path:?} of the note failed."))?;
        if FileName::of(&bytes, file.extension()) != *file {
            return Err(anyhow::anyhow!(
                "The file {path:?} of the note is not what it was when it was saved."
            ));
        }
        Ok(bytes)
    }

    /// Makes a new, empty note folder.
    pub fn create(dir: &Path, device: Device) -> anyhow::Result<Self> {
        if dir.exists() {
            return Err(anyhow::anyhow!("{dir:?} exists already."));
        }
        let note = Self::new(dir, device);
        std::fs::create_dir_all(note.ink_dir())
            .with_context(|| format!("Creating the note folder {dir:?} failed."))?;
        let entry = serde_json::to_vec(&Entry {
            format: Self::FORMAT,
        })?;
        crate::utils::atomic_save_to_file(dir.join(Self::ENTRY_FILE_NAME), entry)?;
        Ok(note)
    }

    /// Reads the note folder. The snapshot is the note as all of its batches together give it.
    pub fn load(dir: &Path, device: Device) -> anyhow::Result<(Self, EngineSnapshot)> {
        let entry_file = dir.join(Self::ENTRY_FILE_NAME);
        let entry = std::fs::read(&entry_file)
            .with_context(|| format!("{dir:?} is not a note folder, it has no entry file."))?;
        let entry: Entry = serde_json::from_slice(&entry)
            .with_context(|| format!("Reading the entry file {entry_file:?} failed."))?;
        if entry.format > Self::FORMAT {
            return Err(anyhow::anyhow!(
                "The note {dir:?} was written by a newer version of the app."
            ));
        }

        let mut note = Self::new(dir, device);
        let ink_dir = note.ink_dir();
        // The record with the highest stamp about each part
        let mut newest: HashMap<Subject, Record> = HashMap::new();
        for batch in record::list_batches(&ink_dir)? {
            // A batch that can not be read is one that a sync tool is still writing. It stays unread, so that
            // `has_unread_batches()` keeps telling of it.
            let lines = match record::read_batch(&batch.path(&ink_dir)) {
                Ok(lines) => lines,
                Err(e) => {
                    warn!("Reading the batch {batch:?} of {dir:?} failed, Err: {e:?}");
                    continue;
                }
            };
            for line in lines {
                let record = match serde_json::from_str::<Record>(&line) {
                    Ok(record) => record,
                    Err(e) => {
                        warn!(
                            "Reading a record of the batch {batch:?} of {dir:?} failed, Err: {e:?}"
                        );
                        continue;
                    }
                };
                note.counter = note.counter.max(record.stamp.counter);
                let is_newer = newest
                    .get(&record.subject)
                    .is_none_or(|other| record.stamp > other.stamp);
                if is_newer {
                    newest.insert(record.subject.clone(), record);
                }
            }
            note.batches.insert(batch);
        }

        let mut document = Document::default();
        let mut camera = Camera::default();
        let mut strokes = Vec::new();
        for (subject, record) in newest {
            let part = match record.content {
                Content::Stroke { chrono, stroke } => {
                    strokes.push((chrono, stroke));
                    // The stroke is only known by its hash until it is saved for the first time. Holding on to it
                    // here would make the engine copy every stroke it touches while loading.
                    KnownPart::Stroke(None)
                }
                Content::Removed => KnownPart::RemovedStroke,
                Content::Document(loaded) => {
                    document = *loaded;
                    KnownPart::Other
                }
                Content::Camera(loaded) => {
                    // Every device has its own view
                    if subject == Subject::Camera(note.device.id.clone()) {
                        camera = *loaded;
                    }
                    KnownPart::Other
                }
            };
            note.known.insert(
                subject,
                Known {
                    hash: record.hash,
                    part,
                },
            );
        }

        note.fill_pdf_pages(strokes.iter_mut().map(|(_, stroke)| stroke.as_mut()));

        // In a fixed order, so that the same note gives the same snapshot
        strokes.sort_by_key(|(chrono, _)| (chrono.t(), chrono.id));
        let mut stroke_components = SlotMap::<StrokeKey, Arc<Stroke>>::with_key();
        let mut chrono_components = SecondaryMap::new();
        let mut chrono_counter = 0;
        for (chrono, stroke) in strokes {
            chrono_counter = chrono_counter.max(chrono.t());
            let key = stroke_components.insert(Arc::new(*stroke));
            chrono_components.insert(key, Arc::new(chrono));
        }

        let snapshot = EngineSnapshot {
            document,
            camera,
            stroke_components: Arc::new(stroke_components),
            chrono_components: Arc::new(chrono_components),
            chrono_counter,
            files: Files::default(),
        };
        Ok((note, snapshot))
    }

    /// Gives the images of Pdf pages that were saved without it what the canvas draws: out of the page cache, or
    /// made from the Pdf in the files of the note.
    ///
    /// A page whose Pdf is not there is left empty. That is a note a sync tool has not brought all of yet.
    fn fill_pdf_pages<'a>(&self, strokes: impl Iterator<Item = &'a mut Stroke>) {
        let cache = &self.device.page_cache;
        // The images that are not in the cache, by their Pdf
        let mut to_convert: HashMap<FileName, Vec<&mut VectorImage>> = HashMap::new();
        for stroke in strokes {
            let Stroke::VectorImage(image) = stroke else {
                continue;
            };
            let Some(pdf_page) = image.pdf_page.clone().filter(|_| image.svg_data.is_empty())
            else {
                continue;
            };
            match cache.get(&pdf_page) {
                Some(svg_data) => image.svg_data = svg_data,
                None => to_convert.entry(pdf_page.file).or_default().push(image),
            }
        }
        if to_convert.is_empty() {
            return;
        }

        for (file, images) in to_convert {
            let pdf = file
                .path_in(&self.files_dir())
                .context("The name of the file is not valid.")
                .and_then(|path| Ok(std::fs::read(path)?))
                .and_then(|bytes| {
                    hayro_syntax::Pdf::new(Arc::new(bytes))
                        .map_err(|e| anyhow::anyhow!("Reading the Pdf failed, Err: {e:?}"))
                });
            let pdf = match pdf {
                Ok(pdf) => pdf,
                Err(e) => {
                    warn!(
                        "The Pdf {file:?} of the note {:?} is not there, Err: {e:?}",
                        self.dir
                    );
                    continue;
                }
            };
            for image in images {
                let Some(pdf_page) = image.pdf_page.as_ref() else {
                    continue;
                };
                let svg_data = pdf
                    .pages()
                    .get(pdf_page.page as usize)
                    .context("The Pdf has no such page.")
                    .and_then(VectorImage::pdf_page_svg_data);
                match svg_data {
                    Ok(svg_data) => {
                        if let Err(e) = cache.put(pdf_page, &svg_data) {
                            warn!("Keeping a page in the page cache failed, Err: {e:?}");
                        }
                        image.svg_data = svg_data;
                    }
                    Err(e) => warn!("Making the page {pdf_page:?} failed, Err: {e:?}"),
                }
            }
        }
        if let Err(e) = cache.trim() {
            warn!("Trimming the page cache failed, Err: {e:?}");
        }
    }

    fn new(dir: &Path, device: Device) -> Self {
        Self {
            dir: dir.to_path_buf(),
            device,
            known: HashMap::new(),
            counter: 0,
            batches: HashSet::new(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn ink_dir(&self) -> PathBuf {
        self.dir.join(Self::INK_DIR_NAME)
    }

    fn files_dir(&self) -> PathBuf {
        self.dir.join(Self::FILES_DIR_NAME)
    }

    /// Whether the note holds the file once the snapshot is saved: it is in its files already, or it is among
    /// the files of the snapshot, which then get written.
    fn has_file(&self, file: &FileName, snapshot: &EngineSnapshot) -> bool {
        let in_files = file
            .path_in(&self.files_dir())
            .is_some_and(|path| path.is_file());
        in_files || snapshot.files.get(file).is_some()
    }

    /// Writes the files of the snapshot that are not in the files of the note yet.
    fn save_files(
        &self,
        files: HashSet<FileName>,
        snapshot: &EngineSnapshot,
    ) -> anyhow::Result<()> {
        let files_dir = self.files_dir();
        for file in files {
            let path = file
                .path_in(&files_dir)
                .with_context(|| format!("{file:?} is not a valid file name."))?;
            if path.is_file() {
                continue;
            }
            let bytes = snapshot
                .files
                .get(&file)
                .with_context(|| format!("The file {file:?} is not there to be saved."))?;
            std::fs::create_dir_all(&files_dir)?;
            crate::utils::atomic_save_to_file(&path, bytes.as_slice())
                .with_context(|| format!("Writing the file {path:?} failed."))?;
        }
        Ok(())
    }

    /// Whether the folder has batches that were not read, that is: whether another device changed the note since
    /// it was loaded. To get them, save and load the note again.
    pub fn has_unread_batches(&self) -> anyhow::Result<bool> {
        Ok(record::list_batches(&self.ink_dir())?
            .iter()
            .any(|batch| !self.batches.contains(batch)))
    }

    /// Saves what changed in the snapshot since the note was loaded or last saved, as one new batch.
    ///
    /// Nothing that is in the folder is touched, so saving can not lose what other devices wrote.
    pub fn save(&mut self, snapshot: &EngineSnapshot) -> anyhow::Result<SaveReport> {
        let stamp = Stamp {
            counter: self.counter + 1,
            device: self.device.id.clone(),
        };
        let mut lines = Vec::new();
        // What `known` becomes once the batch is written
        let mut saved = Vec::new();
        let mut record = |subject: Subject, content: WrittenContent, part: KnownPart| {
            let known = Known {
                hash: content.hash,
                part,
            };
            lines.push(content.into_line(&subject, &stamp)?);
            saved.push((subject, known));
            anyhow::Ok(())
        };

        let mut present = HashSet::<StrokeId>::new();
        // The files the strokes that are written refer to
        let mut files = HashSet::<FileName>::new();
        // The strokes that are as they were saved, but are now known by their allocation as well
        let mut unchanged = Vec::new();
        for (key, stroke) in snapshot.stroke_components.iter() {
            let Some(chrono) = snapshot.chrono_components.get(key) else {
                warn!("A stroke without chrono component is not saved.");
                continue;
            };
            let chrono = **chrono;
            if !present.insert(chrono.id) {
                return Err(anyhow::anyhow!(
                    "Two strokes of the document have the same id."
                ));
            }
            let subject = Subject::Stroke(chrono.id);
            let known = self.known.get(&subject);
            if let Some(Known {
                part: KnownPart::Stroke(Some(saved_stroke)),
                ..
            }) = known
                && saved_stroke.chrono == chrono
                && Arc::ptr_eq(&saved_stroke.stroke, stroke)
            {
                continue;
            }

            // The image of a Pdf page is saved without what is drawn of it when the note holds the Pdf
            let page_image = match stroke.as_ref() {
                Stroke::VectorImage(image) => image
                    .as_pdf_page_image()
                    .filter(|page_image| {
                        // An image that was loaded without its Pdf has nothing else to be saved with
                        image.svg_data.is_empty()
                            || self.has_file(&page_image.pdf_page.file, snapshot)
                    })
                    .map(|page_image| (image, page_image)),
                _ => None,
            };
            let stroke_ref = match page_image {
                Some((image, page_image)) => {
                    if snapshot.files.get(&page_image.pdf_page.file).is_some() {
                        files.insert(page_image.pdf_page.file.clone());
                    }
                    // What is drawn of the page is at hand, so this device need not make it again
                    let cache = &self.device.page_cache;
                    if !image.svg_data.is_empty()
                        && !cache.contains(page_image.pdf_page)
                        && let Err(e) = cache.put(page_image.pdf_page, &image.svg_data)
                    {
                        warn!("Keeping a page in the page cache failed, Err: {e:?}");
                    }
                    StrokeRef::PdfPage(PdfPageStroke::VectorImage(page_image))
                }
                None => StrokeRef::Whole(stroke),
            };
            let content = WrittenContent::new(ContentRef::Stroke {
                chrono: &chrono,
                stroke: stroke_ref,
            })?;
            let saved_stroke = SavedStroke {
                chrono,
                stroke: Arc::clone(stroke),
            };
            let same_as_known = known.is_some_and(|known| {
                matches!(known.part, KnownPart::Stroke(_)) && known.hash == content.hash
            });
            if same_as_known {
                unchanged.push((subject, saved_stroke));
            } else {
                record(subject, content, KnownPart::Stroke(Some(saved_stroke)))?;
            }
        }

        for (subject, known) in self.known.iter() {
            if let (Subject::Stroke(id), KnownPart::Stroke(_)) = (subject, &known.part)
                && !present.contains(id)
            {
                let content = WrittenContent::new(ContentRef::Removed)?;
                record(subject.clone(), content, KnownPart::RemovedStroke)?;
            }
        }

        let others = [
            (
                Subject::Document,
                WrittenContent::new(ContentRef::Document(&snapshot.document))?,
            ),
            (
                Subject::Camera(self.device.id.clone()),
                WrittenContent::new(ContentRef::Camera(&snapshot.camera))?,
            ),
        ];
        for (subject, content) in others {
            if self.known.get(&subject).map(|known| known.hash) != Some(content.hash) {
                record(subject, content, KnownPart::Other)?;
            }
        }

        for (subject, saved_stroke) in unchanged {
            if let Some(known) = self.known.get_mut(&subject) {
                known.part = KnownPart::Stroke(Some(saved_stroke));
            }
        }
        if lines.is_empty() {
            return Ok(SaveReport {
                records: 0,
                batch: None,
            });
        }

        // The files first: a record must not refer to a file that is not there
        self.save_files(files, snapshot)?;
        let batch = self.next_batch()?;
        let file = batch.path(&self.ink_dir());
        record::write_batch(&file, &lines)
            .with_context(|| format!("Writing the batch {file:?} failed."))?;
        self.batches.insert(batch);
        self.counter = stamp.counter;
        self.known.extend(saved);
        Ok(SaveReport {
            records: lines.len(),
            batch: Some(file),
        })
    }

    /// The name of the batch this device writes next.
    ///
    /// The folder is looked at each time, in case the note is open a second time on this device.
    fn next_batch(&self) -> anyhow::Result<BatchName> {
        let last = record::list_batches(&self.ink_dir())?
            .into_iter()
            .filter(|batch| batch.device == self.device.id)
            .map(|batch| batch.seq)
            .max();
        Ok(BatchName {
            device: self.device.id.clone(),
            seq: last.map_or(1, |seq| seq + 1),
        })
    }
}

#[cfg(test)]
mod tests;
