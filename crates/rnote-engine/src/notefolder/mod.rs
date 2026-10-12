//! A note as a folder of files, made for folders that are synced between devices.
//!
//! ```text
//! Lesson 1.rnoted/
//! ├── note.rnoted                   the entry file: opens the note, written once
//! ├── files/
//! │   ├── 9f2c…e1.pdf               an imported Pdf as it was, named by the hash of its content
//! │   └── 41d0…7a.jpeg              an imported picture as it was
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
//! the Pdf and keeps it in its [PageCache], or draws them from the Pdf, as its [Device::pdf_pages] says. Pictures
//! in `files` are saved without their pixels, which are decoded from the file.
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
    BatchName, Content, ContentRef, LeanStroke, Record, StrokeRef, Subject, WrittenContent,
};
use crate::engine::EngineSnapshot;
use crate::store::{ChronoComponent, StrokeId, StrokeKey};
use crate::strokes::vectorimage::{ParsedPdf, PdfPageDrawing};
use crate::strokes::{Stroke, VectorImage};
use crate::{Camera, Document, Image};
use anyhow::Context;
use hayro::hayro_syntax;
use rayon::iter::{IntoParallelIterator, ParallelIterator};
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
    /// How the pages of the Pdfs of a note are drawn. The page cache is only used for [PdfPageDrawing::Converted].
    pub pdf_pages: PdfPageDrawing,
}

impl Device {
    /// This installation of the app, with its id, its page cache and Pdf pages that are converted.
    pub fn this() -> anyhow::Result<Self> {
        Ok(Self {
            id: DeviceId::of_this_installation()?,
            page_cache: PageCache::default(),
            pdf_pages: PdfPageDrawing::default(),
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
    stamp: Stamp,
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

/// A stroke as another device saved it.
#[derive(Debug, Clone)]
pub struct NewStroke {
    pub chrono: ChronoComponent,
    pub stroke: Arc<Stroke>,
}

/// What other devices changed in the note since this device read it, see [NoteFolder::read_news].
#[derive(Debug, Clone, Default)]
pub struct News {
    /// The strokes that were added or changed.
    pub strokes: Vec<NewStroke>,
    pub removed: Vec<StrokeId>,
    pub document: Option<Document>,
    /// The view of this device, which is only news to a note that is loaded.
    pub camera: Option<Camera>,
}

impl News {
    pub fn is_empty(&self) -> bool {
        self.strokes.is_empty()
            && self.removed.is_empty()
            && self.document.is_none()
            && self.camera.is_none()
    }
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
    /// The directory of the pictures of the first page of the note, for showing the note among others without
    /// opening it. Each device writes its own, as with batches; the newest is the note's.
    pub const THUMBNAILS_DIR_NAME: &'static str = "thumbnails";
    /// The longer side of a thumbnail in pixels.
    pub const THUMBNAIL_SIZE: u32 = 256;

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
        let files = Files::default();
        // To a note that was not read yet everything is news
        let news = note.read_news(&files)?;

        let mut strokes = news.strokes;
        // In a fixed order, so that the same note gives the same snapshot
        strokes.sort_by_key(|new| (new.chrono.t(), new.chrono.id));
        let mut stroke_components = SlotMap::<StrokeKey, Arc<Stroke>>::with_key();
        let mut chrono_components = SecondaryMap::new();
        let mut chrono_counter = 0;
        for new in strokes {
            chrono_counter = chrono_counter.max(new.chrono.t());
            let key = stroke_components.insert(new.stroke);
            chrono_components.insert(key, Arc::new(new.chrono));
        }

        let snapshot = EngineSnapshot {
            document: news.document.unwrap_or_default(),
            camera: news.camera.unwrap_or_default(),
            stroke_components: Arc::new(stroke_components),
            chrono_components: Arc::new(chrono_components),
            chrono_counter,
            files,
        };
        Ok((note, snapshot))
    }

    /// Reads the batches that were not read yet, and tells what in them is newer than what this device knows of
    /// the note. That is what other devices changed since the note was loaded, read or saved; to be read after
    /// saving, so that what this device changed is known.
    ///
    /// What the strokes need of the files of the note is made, as in [Self::load]. A Pdf that is kept in memory
    /// for drawing its pages goes among `files`.
    pub fn read_news(&mut self, files: &Files) -> anyhow::Result<News> {
        let mut news = News::default();
        let mut strokes = Vec::new();
        for (subject, record) in self.read_unread_batches()? {
            let is_news = self
                .known
                .get(&subject)
                .is_none_or(|known| record.stamp > known.stamp);
            if !is_news {
                continue;
            }
            let part = match record.content {
                Content::Stroke { chrono, stroke } => {
                    strokes.push((chrono, stroke));
                    // The stroke is only known by its hash until it is saved for the first time. Holding on to it
                    // here would make the engine copy every stroke it touches while loading.
                    KnownPart::Stroke(None)
                }
                Content::Removed => {
                    if let Subject::Stroke(id) = subject {
                        news.removed.push(id);
                    }
                    KnownPart::RemovedStroke
                }
                Content::Document(document) => {
                    news.document = Some(*document);
                    KnownPart::Other
                }
                Content::Camera(camera) => {
                    // Every device has its own view
                    if subject == Subject::Camera(self.device.id.clone()) {
                        news.camera = Some(*camera);
                    }
                    KnownPart::Other
                }
            };
            let known = Known {
                hash: record.hash,
                stamp: record.stamp,
                part,
            };
            self.known.insert(subject, known);
        }

        self.fill_pictures(strokes.iter_mut().map(|(_, stroke)| stroke.as_mut()));
        self.fill_pdf_pages(strokes.iter_mut().map(|(_, stroke)| stroke.as_mut()), files);
        news.strokes = strokes
            .into_iter()
            .map(|(chrono, stroke)| NewStroke {
                chrono,
                stroke: Arc::new(*stroke),
            })
            .collect();
        Ok(news)
    }

    /// Reads the batches that were not read yet. Of the records about each part of the note, the one with the
    /// highest stamp is returned.
    fn read_unread_batches(&mut self) -> anyhow::Result<HashMap<Subject, Record>> {
        let ink_dir = self.ink_dir();
        let mut newest: HashMap<Subject, Record> = HashMap::new();
        for batch in record::list_batches(&ink_dir)? {
            if self.batches.contains(&batch) {
                continue;
            }
            // A batch that can not be read is one that a sync tool is still writing. It stays unread, so that
            // `has_unread_batches()` keeps telling of it.
            let lines = match record::read_batch(&batch.path(&ink_dir)) {
                Ok(lines) => lines,
                Err(e) => {
                    warn!(
                        "Reading the batch {batch:?} of {:?} failed, Err: {e:?}",
                        self.dir
                    );
                    continue;
                }
            };
            for line in lines {
                let record = match serde_json::from_str::<Record>(&line) {
                    Ok(record) => record,
                    Err(e) => {
                        warn!(
                            "Reading a record of the batch {batch:?} of {:?} failed, Err: {e:?}",
                            self.dir
                        );
                        continue;
                    }
                };
                self.counter = self.counter.max(record.stamp.counter);
                let is_newer = newest
                    .get(&record.subject)
                    .is_none_or(|other| record.stamp > other.stamp);
                if is_newer {
                    newest.insert(record.subject.clone(), record);
                }
            }
            self.batches.insert(batch);
        }
        Ok(newest)
    }

    /// Gives the images of Pdf pages that were saved without it what the canvas draws: out of the page cache, or
    /// made from the Pdf in the files of the note. When the device draws pages from the Pdf, the images get the
    /// Pdf instead, and the Pdf goes among the `files`: it is in memory anyway.
    ///
    /// A page whose Pdf is not there is left empty. That is a note a sync tool has not brought all of yet.
    fn fill_pdf_pages<'a>(&self, strokes: impl Iterator<Item = &'a mut Stroke>, files: &Files) {
        let cache = &self.device.page_cache;
        let from_pdf = self.device.pdf_pages == PdfPageDrawing::FromPdf;
        // The images that need their Pdf, by their Pdf
        let mut to_fill: HashMap<FileName, Vec<&mut VectorImage>> = HashMap::new();
        for stroke in strokes {
            let Stroke::VectorImage(image) = stroke else {
                continue;
            };
            let Some(pdf_page) = image.pdf_page.clone().filter(|_| image.svg_data.is_empty())
            else {
                continue;
            };
            match cache.get(&pdf_page).filter(|_| !from_pdf) {
                Some(svg_data) => image.svg_data = svg_data,
                None => to_fill.entry(pdf_page.file).or_default().push(image),
            }
        }
        if to_fill.is_empty() {
            return;
        }

        for (file, images) in to_fill {
            let pdf = Self::read_file(&self.dir, &file)
                .map(Arc::new)
                .and_then(|bytes| {
                    let pdf = hayro_syntax::Pdf::new(Arc::clone(&bytes))
                        .map_err(|e| anyhow::anyhow!("Reading the Pdf failed, Err: {e:?}"))?;
                    Ok((bytes, pdf))
                });
            let (bytes, pdf) = match pdf {
                Ok(pdf) => pdf,
                Err(e) => {
                    warn!(
                        "The Pdf {file:?} of the note {:?} is not there, Err: {e:?}",
                        self.dir
                    );
                    continue;
                }
            };
            if from_pdf {
                files.insert(bytes, file.extension());
                let pdf = ParsedPdf(Arc::new(pdf));
                for image in images {
                    image.pdf = Some(pdf.clone());
                }
                continue;
            }
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
        if !from_pdf && let Err(e) = cache.trim() {
            warn!("Trimming the page cache failed, Err: {e:?}");
        }
    }

    /// Decodes the pictures that were saved without their pixels from their files.
    ///
    /// A picture whose file is not there is left empty, as a page whose Pdf is not there.
    fn fill_pictures<'a>(&self, strokes: impl Iterator<Item = &'a mut Stroke>) {
        let pictures = strokes
            .filter_map(|stroke| match stroke {
                Stroke::BitmapImage(picture) if picture.image.data.is_empty() => Some(picture),
                _ => None,
            })
            .collect::<Vec<_>>();
        pictures.into_par_iter().for_each(|picture| {
            let Some(file) = picture.file.as_ref() else {
                return;
            };
            let image = Self::read_file(&self.dir, file)
                .and_then(|bytes| Image::try_from_encoded_bytes(&bytes));
            match image {
                Ok(image) => picture.image = image,
                Err(e) => warn!(
                    "The picture {file:?} of the note {:?} is not there, Err: {e:?}",
                    self.dir
                ),
            }
        });
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

    /// The newest picture of the first page of the note folder at the directory, when a device wrote one.
    pub fn thumbnail(dir: &Path) -> Option<PathBuf> {
        let entries = std::fs::read_dir(dir.join(Self::THUMBNAILS_DIR_NAME)).ok()?;
        entries
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "png"))
            .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
            .max()
            .map(|(_, path)| path)
    }

    /// Writes the picture of the first page of the note, as this device's.
    pub fn write_thumbnail(&self, png: &[u8]) -> anyhow::Result<()> {
        let thumbnails_dir = self.dir.join(Self::THUMBNAILS_DIR_NAME);
        std::fs::create_dir_all(&thumbnails_dir)?;
        crate::utils::atomic_save_to_file(
            thumbnails_dir.join(format!("{}.png", self.device.id.as_str())),
            png,
        )
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
    /// it was loaded. To get them, save and [read the news](Self::read_news).
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
                stamp: stamp.clone(),
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

            // A stroke that knows its file is saved without what is made of it when the note holds the file
            let lean = LeanStroke::of(stroke).filter(|lean| {
                // A stroke that was loaded without its file has nothing else to be saved with
                !holds_what_is_made_of_its_file(stroke) || self.has_file(lean.file(), snapshot)
            });
            let stroke_ref = match lean {
                Some(lean) => {
                    if snapshot.files.get(lean.file()).is_some() {
                        files.insert(lean.file().clone());
                    }
                    // What is drawn of the page is at hand, so this device need not make it again
                    let cache = &self.device.page_cache;
                    if let (LeanStroke::VectorImage(page_image), Stroke::VectorImage(image)) =
                        (&lean, stroke.as_ref())
                        && !image.svg_data.is_empty()
                        && !cache.contains(page_image.pdf_page)
                        && let Err(e) = cache.put(page_image.pdf_page, &image.svg_data)
                    {
                        warn!("Keeping a page in the page cache failed, Err: {e:?}");
                    }
                    StrokeRef::Lean(lean)
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

/// Whether the stroke holds what is made of its file, the drawing of a Pdf page or the pixels of a picture, so that
/// it can be saved whole when the note does not hold the file.
fn holds_what_is_made_of_its_file(stroke: &Stroke) -> bool {
    match stroke {
        Stroke::VectorImage(image) => !image.svg_data.is_empty(),
        Stroke::BitmapImage(picture) => !picture.image.data.is_empty(),
        _ => true,
    }
}

#[cfg(test)]
mod tests;
