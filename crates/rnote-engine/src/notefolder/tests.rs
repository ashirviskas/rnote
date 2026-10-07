use super::record::{ContentKind, RecordHead};
use super::*;
use crate::store::chrono_comp::StrokeLayer;
use crate::strokes::{BrushStroke, VectorImage};
use p2d::math::Vector2;
use rnote_compose::Style;
use rnote_compose::penpath::Element;

/// A device with a page cache of its own, which is never looked at by the tests that have no Pdf.
fn device(id: &str) -> Device {
    Device {
        id: DeviceId::new(id).unwrap(),
        page_cache: PageCache {
            dir: std::env::temp_dir().join("rnote-test-no-page-cache"),
            limit: 0,
        },
    }
}

/// A stroke that is told from others by where it starts.
fn stroke_at(x: f64) -> Stroke {
    let start = Element::new(Vector2::new(x, 0.0), 0.5);
    Stroke::BrushStroke(BrushStroke::new(start, Style::default()))
}

/// A note that is open on one device: its folder and the document as it is being edited.
struct Desk {
    note: NoteFolder,
    snapshot: EngineSnapshot,
}

impl Desk {
    fn create(dir: &Path, id: &str) -> Self {
        Self::create_on(dir, device(id))
    }

    fn create_on(dir: &Path, device: Device) -> Self {
        Self {
            note: NoteFolder::create(dir, device).unwrap(),
            snapshot: EngineSnapshot::default(),
        }
    }

    fn open(dir: &Path, id: &str) -> Self {
        Self::open_on(dir, device(id))
    }

    fn open_on(dir: &Path, device: Device) -> Self {
        let (note, snapshot) = NoteFolder::load(dir, device).unwrap();
        Self { note, snapshot }
    }

    fn reload(&mut self) {
        let device = self.note.device.clone();
        let (note, snapshot) = NoteFolder::load(&self.note.dir, device).unwrap();
        self.note = note;
        self.snapshot = snapshot;
    }

    fn save(&mut self) -> SaveReport {
        self.note.save(&self.snapshot).unwrap()
    }

    fn add(&mut self, x: f64) -> StrokeId {
        self.add_stroke(stroke_at(x))
    }

    fn add_stroke(&mut self, stroke: Stroke) -> StrokeId {
        let stroke = Arc::new(stroke);
        let key = Arc::make_mut(&mut self.snapshot.stroke_components).insert(stroke);
        self.snapshot.chrono_counter += 1;
        let chrono = ChronoComponent::new(self.snapshot.chrono_counter, StrokeLayer::default());
        Arc::make_mut(&mut self.snapshot.chrono_components).insert(key, Arc::new(chrono));
        chrono.id
    }

    fn key_of(&self, id: StrokeId) -> StrokeKey {
        let mut chronos = self.snapshot.chrono_components.iter();
        chronos.find(|(_, chrono)| chrono.id == id).unwrap().0
    }

    fn remove(&mut self, id: StrokeId) {
        let key = self.key_of(id);
        Arc::make_mut(&mut self.snapshot.stroke_components).remove(key);
        Arc::make_mut(&mut self.snapshot.chrono_components).remove(key);
    }

    fn change(&mut self, id: StrokeId, x: f64) {
        let key = self.key_of(id);
        Arc::make_mut(&mut self.snapshot.stroke_components)[key] = Arc::new(stroke_at(x));
    }

    /// The images of the document.
    fn images(&self) -> Vec<&VectorImage> {
        let strokes = self.snapshot.stroke_components.values();
        strokes
            .filter_map(|stroke| match stroke.as_ref() {
                Stroke::VectorImage(image) => Some(image),
                _ => None,
            })
            .collect()
    }

    /// Where the strokes of the document start, sorted.
    fn xs(&self) -> Vec<f64> {
        let mut xs = self
            .snapshot
            .stroke_components
            .values()
            .map(|stroke| match stroke.as_ref() {
                Stroke::BrushStroke(brushstroke) => brushstroke.path.start.pos[0],
                _ => f64::NAN,
            })
            .collect::<Vec<f64>>();
        xs.sort_by(f64::total_cmp);
        xs
    }
}

/// A record in a batch of a note folder.
#[derive(Debug, PartialEq)]
struct OnDisk {
    device: String,
    subject: Subject,
    removed: bool,
}

fn on_disk(dir: &Path) -> Vec<OnDisk> {
    let ink_dir = dir.join("ink");
    let mut records = Vec::new();
    for batch in record::list_batches(&ink_dir).unwrap() {
        for line in record::read_batch(&batch.path(&ink_dir)).unwrap() {
            let head = serde_json::from_str::<RecordHead>(&line).unwrap();
            records.push(OnDisk {
                device: batch.device.as_str().to_string(),
                subject: head.subject,
                removed: matches!(head.content, ContentKind::Removed),
            });
        }
    }
    records
}

/// The records about the stroke in the batches of the folder.
fn on_disk_of(dir: &Path, id: StrokeId) -> Vec<OnDisk> {
    let mut records = on_disk(dir);
    records.retain(|record| record.subject == Subject::Stroke(id));
    records
}

fn n_batches(dir: &Path) -> usize {
    record::list_batches(&dir.join("ink")).unwrap().len()
}

/// A place for a note folder, which is not made yet.
fn note_dir(tmp: &tempfile::TempDir) -> PathBuf {
    tmp.path().join("Lesson 1.rnoted")
}

#[test]
fn a_saved_note_loads_as_it_was() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = note_dir(&tmp);
    let mut a = Desk::create(&dir, "aaaa");
    let ids = [a.add(1.0), a.add(2.0), a.add(3.0)];
    a.snapshot.document.width = 1234.0;
    a.snapshot.camera = serde_json::from_str(r#"{"zoom":2.0}"#).unwrap();
    a.save();

    let same_device = Desk::open(&dir, "aaaa");
    assert_eq!(same_device.xs(), [1.0, 2.0, 3.0]);
    assert_eq!(same_device.snapshot.document.width, 1234.0);
    assert_eq!(same_device.snapshot.chrono_counter, 3);
    assert_eq!(same_device.snapshot.camera.zoom(), 2.0);
    // The strokes keep their ids and their order
    let mut chronos = same_device.snapshot.chrono_components.values();
    assert!(ids.iter().all(|id| chronos.next().unwrap().id == *id));

    // Another device gets the note, but not the view of the first
    let other_device = Desk::open(&dir, "bbbb");
    assert_eq!(other_device.xs(), [1.0, 2.0, 3.0]);
    assert_eq!(
        other_device.snapshot.camera.zoom(),
        Camera::default().zoom()
    );
}

#[test]
fn a_note_folder_is_found_by_its_folder_and_by_its_entry_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = note_dir(&tmp);
    Desk::create(&dir, "aaaa");

    assert_eq!(NoteFolder::folder_of(&dir), Some(dir.clone()));
    let entry_file = dir.join(NoteFolder::ENTRY_FILE_NAME);
    assert_eq!(NoteFolder::folder_of(&entry_file), Some(dir.clone()));
    assert_eq!(NoteFolder::folder_of(tmp.path()), None);
    assert_eq!(NoteFolder::folder_of(&dir.join("ink")), None);
    assert!(NoteFolder::load(tmp.path(), device("aaaa")).is_err());
    assert!(NoteFolder::create(&dir, device("aaaa")).is_err());
}

#[test]
fn saving_writes_only_what_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = note_dir(&tmp);
    let mut a = Desk::create(&dir, "aaaa");
    let first = a.add(1.0);
    a.add(2.0);
    // Two strokes, the document and the view
    assert_eq!(a.save().records, 4);
    assert_eq!(
        a.save(),
        SaveReport {
            records: 0,
            batch: None
        }
    );
    assert_eq!(n_batches(&dir), 1);

    a.add(3.0);
    assert_eq!(a.save().records, 1);
    a.change(first, 1.5);
    assert_eq!(a.save().records, 1);
    assert_eq!(n_batches(&dir), 3);

    // After loading, the strokes are known by their hash
    let mut again = Desk::open(&dir, "aaaa");
    assert_eq!(again.save().records, 0);
    again.remove(first);
    assert_eq!(again.save().records, 1);
    assert_eq!(again.save().records, 0);
    assert_eq!(Desk::open(&dir, "aaaa").xs(), [2.0, 3.0]);
}

#[test]
fn strokes_added_on_two_devices_are_all_there() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = note_dir(&tmp);
    let mut a = Desk::create(&dir, "aaaa");
    a.add(1.0);
    a.save();
    let mut b = Desk::open(&dir, "bbbb");

    // Neither knows of what the other does
    a.add(2.0);
    b.add(3.0);
    a.save();
    b.save();

    a.reload();
    b.reload();
    assert_eq!(a.xs(), [1.0, 2.0, 3.0]);
    assert_eq!(b.xs(), [1.0, 2.0, 3.0]);
    // Strokes that came from the other device are not written again
    assert_eq!(a.save().records, 0);
    assert_eq!(b.save().records, 0);
}

#[test]
fn a_removal_and_an_addition_both_happen() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = note_dir(&tmp);
    let mut a = Desk::create(&dir, "aaaa");
    let first = a.add(1.0);
    a.save();
    let mut b = Desk::open(&dir, "bbbb");

    a.remove(first);
    b.add(2.0);
    a.save();
    b.save();

    a.reload();
    b.reload();
    assert_eq!(a.xs(), [2.0]);
    assert_eq!(b.xs(), [2.0]);
}

#[test]
fn of_two_changes_to_a_stroke_the_later_save_wins() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = note_dir(&tmp);
    let mut a = Desk::create(&dir, "aaaa");
    let id = a.add(1.0);
    a.save();
    let mut b = Desk::open(&dir, "bbbb");

    // B saves after having seen what A did
    a.change(id, 5.0);
    a.save();
    b.reload();
    b.change(id, 7.0);
    b.save();
    a.reload();
    assert_eq!(a.xs(), [7.0]);

    // A saves after having seen what B did
    a.change(id, 8.0);
    a.save();
    b.reload();
    assert_eq!(b.xs(), [8.0]);

    // Neither has seen what the other did: both devices settle on the same one
    a.change(id, 10.0);
    b.change(id, 11.0);
    b.save();
    a.save();
    a.reload();
    b.reload();
    assert_eq!(a.xs(), [11.0]);
    assert_eq!(b.xs(), [11.0]);
}

#[test]
fn a_removal_after_a_change_removes_the_stroke() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = note_dir(&tmp);
    let mut a = Desk::create(&dir, "aaaa");
    let (_, removed) = (a.add(1.0), a.add(2.0));
    a.save();
    let mut b = Desk::open(&dir, "bbbb");

    // Changed on B, then removed on A, which has seen the change
    b.change(removed, 2.5);
    b.save();
    a.reload();
    assert_eq!(a.xs(), [1.0, 2.5]);
    a.remove(removed);
    a.save();
    b.reload();
    assert_eq!(b.xs(), [1.0]);
}

#[test]
fn a_change_wins_over_a_removal_it_did_not_know_of() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = note_dir(&tmp);
    let mut a = Desk::create(&dir, "aaaa");
    let id = a.add(1.0);
    a.save();
    let mut b = Desk::open(&dir, "bbbb");

    // Both stamps have the same counter, the device decides
    a.remove(id);
    b.change(id, 1.5);
    a.save();
    b.save();
    a.reload();
    b.reload();
    assert_eq!(a.xs(), [1.5]);
    assert_eq!(b.xs(), [1.5]);
}

#[test]
fn the_document_settings_of_the_later_save_win() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = note_dir(&tmp);
    let mut a = Desk::create(&dir, "aaaa");
    a.snapshot.document.width = 111.0;
    a.save();

    let mut b = Desk::open(&dir, "bbbb");
    b.snapshot.document.width = 222.0;
    b.save();
    a.reload();
    assert_eq!(a.snapshot.document.width, 222.0);
}

#[test]
fn batches_of_other_devices_are_news_until_they_are_loaded() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = note_dir(&tmp);
    let mut a = Desk::create(&dir, "aaaa");
    a.add(1.0);
    a.save();
    let mut b = Desk::open(&dir, "bbbb");
    assert!(!a.note.has_unread_batches().unwrap());
    assert!(!b.note.has_unread_batches().unwrap());

    b.add(2.0);
    b.save();
    assert!(!b.note.has_unread_batches().unwrap());
    assert!(a.note.has_unread_batches().unwrap());
    a.reload();
    assert!(!a.note.has_unread_batches().unwrap());
}

#[test]
fn a_stroke_removed_on_its_own_device_leaves_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = note_dir(&tmp);
    let mut a = Desk::create(&dir, "aaaa");
    let (_, removed) = (a.add(1.0), a.add(2.0));
    a.save();
    a.remove(removed);
    a.save();
    // The stroke and the mark that it was removed
    assert_eq!(on_disk_of(&dir, removed).len(), 2);

    let report = a.note.tidy().unwrap();
    assert_eq!(report.dropped_records, 2);
    assert_eq!(on_disk_of(&dir, removed), []);
    assert_eq!(n_batches(&dir), 1);
    assert_eq!(Desk::open(&dir, "aaaa").xs(), [1.0]);
    // Nothing is left to do, and the device goes on as before
    assert_eq!(a.note.tidy().unwrap(), TidyReport::default());
    assert_eq!(a.save().records, 0);
    a.add(3.0);
    assert_eq!(a.save().records, 1);
    assert_eq!(Desk::open(&dir, "aaaa").xs(), [1.0, 3.0]);
}

#[test]
fn a_stroke_removed_on_another_device_leaves_nothing_once_both_tidied() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = note_dir(&tmp);
    let mut a = Desk::create(&dir, "aaaa");
    let (_, removed) = (a.add(1.0), a.add(2.0));
    a.save();
    let mut b = Desk::open(&dir, "bbbb");
    b.remove(removed);
    b.save();

    // The mark waits for as long as the other device holds the stroke
    assert_eq!(b.note.tidy().unwrap(), TidyReport::default());
    let records = on_disk_of(&dir, removed);
    assert_eq!(records.len(), 2);
    assert!(records.iter().any(|r| r.device == "bbbb" && r.removed));

    // The device that wrote the stroke takes it out
    a.reload();
    a.note.tidy().unwrap();
    let records = on_disk_of(&dir, removed);
    assert_eq!(records.len(), 1);
    assert!(records[0].removed);
    assert_eq!(Desk::open(&dir, "aaaa").xs(), [1.0]);

    // Then the mark can go
    b.note.tidy().unwrap();
    assert_eq!(on_disk_of(&dir, removed), []);
    assert_eq!(Desk::open(&dir, "aaaa").xs(), [1.0]);
    assert_eq!(Desk::open(&dir, "bbbb").xs(), [1.0]);
}

#[test]
fn tidying_never_touches_the_batches_of_other_devices() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = note_dir(&tmp);
    let mut a = Desk::create(&dir, "aaaa");
    let id = a.add(1.0);
    a.save();
    a.change(id, 2.0);
    a.save();
    let of_a = |dir: &Path| {
        let mut records = on_disk(dir);
        records.retain(|record| record.device == "aaaa");
        records.len()
    };
    let before = of_a(&dir);

    // To B the first version of the stroke is dead, but it is not B's to take out
    let mut b = Desk::open(&dir, "bbbb");
    b.add(3.0);
    b.save();
    b.note.tidy().unwrap();
    assert_eq!(of_a(&dir), before);
    a.note.tidy().unwrap();
    assert_eq!(of_a(&dir), before - 1);
    assert_eq!(Desk::open(&dir, "bbbb").xs(), [2.0, 3.0]);
}

#[test]
fn many_small_batches_are_put_together() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = note_dir(&tmp);
    let mut a = Desk::create(&dir, "aaaa");
    for i in 0..20 {
        a.add(f64::from(i));
        a.save();
    }
    assert_eq!(n_batches(&dir), 20);

    let report = a.note.tidy().unwrap();
    assert_eq!(report.removed_batches, 20);
    assert_eq!(report.written_batches, 1);
    assert_eq!(report.dropped_records, 0);
    assert_eq!(n_batches(&dir), 1);
    assert_eq!(Desk::open(&dir, "aaaa").xs().len(), 20);
    // The device that tidied has read all there is
    assert!(!a.note.has_unread_batches().unwrap());
}

#[test]
fn files_that_are_no_batches_are_left_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = note_dir(&tmp);
    let mut a = Desk::create(&dir, "aaaa");
    a.add(1.0);
    a.save();
    // A file a sync tool is still writing, and a batch it only brought half of
    std::fs::write(dir.join("ink").join(".tmpAbC123"), b"half").unwrap();
    std::fs::write(dir.join("ink").join("bbbb-000001.jsonl.gz"), b"half").unwrap();

    let mut again = Desk::open(&dir, "aaaa");
    assert_eq!(again.xs(), [1.0]);
    // The batch that could not be read is asked for again, and nothing is tidied before it is there
    assert!(again.note.has_unread_batches().unwrap());
    again.add(2.0);
    again.save();
    again.remove(again.snapshot.chrono_components.values().next().unwrap().id);
    again.save();
    assert_eq!(again.note.tidy().unwrap(), TidyReport::default());
}

#[test]
fn the_id_of_a_stroke_is_kept_in_a_single_file_note() {
    let chrono = ChronoComponent::new(7, StrokeLayer::default());
    let value = ijson::to_value(chrono).unwrap();
    assert_eq!(
        ijson::from_value::<ChronoComponent>(&value).unwrap(),
        chrono
    );
    // A note from before strokes had ids
    let old = serde_json::from_str::<ChronoComponent>(r#"{"t":7,"layer":{"user_layer":0}}"#);
    assert_eq!(old.unwrap().t(), 7);
}

/// A Pdf with one page, see `imported_pdf_pages_carry_their_text`.
const PDF: &[u8] = include_bytes!("../../../../misc/file-tests/text.pdf");

/// A device with the directory as its page cache.
fn device_with_cache(id: &str, dir: PathBuf) -> Device {
    Device {
        id: DeviceId::new(id).unwrap(),
        page_cache: PageCache {
            dir,
            limit: PageCache::LIMIT_DEFAULT,
        },
    }
}

/// Imports the Pdf into the document, the way the engine does.
fn import_pdf(desk: &mut Desk) {
    use crate::document::Format;
    use crate::engine::import::PdfImportPrefs;

    let file = desk.snapshot.files.insert(Arc::new(PDF.to_vec()), "pdf");
    let (prefs, format) = (PdfImportPrefs::default(), Format::default());
    let pages =
        VectorImage::from_pdf_bytes(PDF, prefs, Vector2::ZERO, None, &format, None, Some(file));
    for page in pages.unwrap() {
        desk.add_stroke(Stroke::VectorImage(page));
    }
}

#[test]
fn a_pdf_is_kept_as_it_was_and_its_pages_are_made_from_it() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = note_dir(&tmp);
    let cache = |name: &str| tmp.path().join(name);
    let mut a = Desk::create_on(&dir, device_with_cache("aaaa", cache("cache a")));
    a.add(1.0);
    import_pdf(&mut a);
    a.save();
    let drawn = a.images()[0].svg_data.clone();
    assert!(drawn.starts_with("<svg"));

    // The Pdf is in the files of the note, byte for byte
    let file = FileName::of(PDF, "pdf");
    assert_eq!(
        std::fs::read(dir.join("files").join(file.as_str())).unwrap(),
        PDF
    );
    // The record of the page says which page it is, not what is drawn of it
    let batch = &record::list_batches(&dir.join("ink")).unwrap()[0];
    let lines = record::read_batch(&batch.path(&dir.join("ink"))).unwrap();
    assert!(lines.iter().any(|line| line.contains(file.as_str())));
    assert!(lines.iter().all(|line| !line.contains("svg")));
    // Saved again, the Pdf is not written again, and neither is the page
    import_pdf(&mut a);
    assert_eq!(a.save().records, 1);
    assert_eq!(std::fs::read_dir(dir.join("files")).unwrap().count(), 1);

    // The device that saved the page has what is drawn of it in its cache
    let mut same_device = Desk::open_on(&dir, device_with_cache("aaaa", cache("cache a")));
    assert_eq!(same_device.images()[0].svg_data, drawn);
    assert_eq!(same_device.save().records, 0);

    // Another device makes it from the Pdf, once
    assert!(!cache("cache b").exists());
    let mut other_device = Desk::open_on(&dir, device_with_cache("bbbb", cache("cache b")));
    let made = other_device.images()[0].svg_data.clone();
    assert!(made.starts_with("<svg"));
    assert_eq!(
        other_device.images()[0].text_lines,
        a.images()[0].text_lines
    );
    assert_eq!(std::fs::read_dir(cache("cache b")).unwrap().count(), 1);
    other_device.reload();
    assert_eq!(other_device.images()[0].svg_data, made);
    // Only the view of the device is new
    assert_eq!(other_device.save().records, 1);
    assert_eq!(other_device.save().records, 0);
}

#[test]
fn a_page_whose_pdf_is_not_there_yet_stays_a_page() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = note_dir(&tmp);
    let mut a = Desk::create_on(&dir, device_with_cache("aaaa", tmp.path().join("cache a")));
    import_pdf(&mut a);
    a.save();

    // A sync tool has brought the batches, but not the Pdf
    let files_dir = dir.join("files");
    std::fs::rename(&files_dir, tmp.path().join("later")).unwrap();
    let cache_b = tmp.path().join("cache b");
    let mut b = Desk::open_on(&dir, device_with_cache("bbbb", cache_b.clone()));
    assert_eq!(b.images()[0].svg_data, "");
    assert_eq!(b.images()[0].pdf_page, a.images()[0].pdf_page);
    // Saving does not make the page something else
    b.add(1.0);
    assert_eq!(b.save().records, 2);

    // Once the Pdf is there the page is drawn
    std::fs::rename(tmp.path().join("later"), &files_dir).unwrap();
    b.reload();
    assert!(b.images()[0].svg_data.starts_with("<svg"));
}

#[test]
fn without_its_pdf_a_page_is_saved_whole() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = note_dir(&tmp);
    let mut a = Desk::create_on(&dir, device_with_cache("aaaa", tmp.path().join("cache a")));
    import_pdf(&mut a);
    // As in a document that was loaded from a single-file note: the pages are there, the Pdf is not
    a.snapshot.files = Files::default();
    a.save();

    assert!(!dir.join("files").exists());
    let b = Desk::open_on(&dir, device_with_cache("bbbb", tmp.path().join("cache b")));
    assert_eq!(b.images()[0].svg_data, a.images()[0].svg_data);
}

#[test]
fn a_packed_note_holds_the_note_and_nothing_that_was_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = note_dir(&tmp);
    let cache = |name: &str| tmp.path().join(name);
    let mut a = Desk::create_on(&dir, device_with_cache("aaaa", cache("cache a")));
    let (_, removed) = (a.add(1.0), a.add(2.0));
    import_pdf(&mut a);
    a.save();
    let mut b = Desk::open_on(&dir, device_with_cache("bbbb", cache("cache b")));
    b.remove(removed);
    b.add(3.0);
    b.save();
    // The folder still holds the removed stroke and the mark of its removal
    assert_eq!(on_disk_of(&dir, removed).len(), 2);

    let packed = tmp.path().join("Lesson 1.rnotez");
    NoteFolder::pack(&dir, device_with_cache("bbbb", cache("cache b")), &packed).unwrap();
    let unpacked = tmp.path().join("Unpacked.rnoted");
    NoteFolder::unpack(&packed, &unpacked).unwrap();

    // The note is all there for someone who has nothing of it: strokes, the Pdf, its page
    assert_eq!(NoteFolder::folder_of(&unpacked), Some(unpacked.clone()));
    let c = Desk::open_on(&unpacked, device_with_cache("cccc", cache("cache c")));
    assert_eq!(c.xs().iter().filter(|x| !x.is_nan()).count(), 2);
    assert!(c.images()[0].svg_data.starts_with("<svg"));
    let file = FileName::of(PDF, "pdf");
    assert_eq!(
        std::fs::read(unpacked.join("files").join(file.as_str())).unwrap(),
        PDF
    );
    // What was removed is not in it, and neither are the batches of two devices
    assert_eq!(on_disk_of(&unpacked, removed), []);
    assert_eq!(n_batches(&unpacked), 1);

    // Loaded without unpacking, the note is all there too, with its Pdf among the files
    let loaded =
        NoteFolder::load_packed(&packed, device_with_cache("dddd", cache("cache d"))).unwrap();
    assert_eq!(loaded.stroke_components.len(), 3);
    assert!(loaded.files.get(&file).is_some());

    // An existing folder is not written into, and other files are no packed notes
    assert!(NoteFolder::unpack(&packed, &unpacked).is_err());
    let no_note = tmp.path().join("other.rnotez");
    std::fs::write(&no_note, b"not a zip").unwrap();
    assert!(NoteFolder::unpack(&no_note, &tmp.path().join("Other.rnoted")).is_err());
}

#[test]
fn the_hash_of_a_pdf_page_unit_does_not_depend_on_what_is_drawn_of_it() {
    use crate::engine::StrokeContent;
    use crate::engine::export::{TextLayer, TextUnit};

    let tmp = tempfile::tempdir().unwrap();
    let mut a = Desk::create_on(
        &note_dir(&tmp),
        device_with_cache("aaaa", tmp.path().join("cache")),
    );
    import_pdf(&mut a);
    let page = a.images()[0].clone();
    let unit = |image: VectorImage| TextUnit {
        page: 0,
        layer: TextLayer::Image,
        content: StrokeContent::default().with_strokes(vec![Arc::new(Stroke::VectorImage(image))]),
    };

    // What another device makes of the page is not the same, byte for byte
    let mut drawn_elsewhere = page.clone();
    drawn_elsewhere
        .svg_data
        .push_str("<!-- drawn elsewhere -->");
    let hash = unit(page.clone()).content_hash().unwrap();
    assert_eq!(unit(drawn_elsewhere).content_hash().unwrap(), hash);

    // Another page of the Pdf, or a page that does not know its Pdf, is something else
    let mut other_page = page.clone();
    other_page.pdf_page.as_mut().unwrap().page = 1;
    assert_ne!(unit(other_page).content_hash().unwrap(), hash);
    let mut whole = page.clone();
    whole.pdf_page = None;
    assert_ne!(unit(whole).content_hash().unwrap(), hash);
}
