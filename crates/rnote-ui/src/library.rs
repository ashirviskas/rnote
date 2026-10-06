//! The library: the folder that holds all notes.
//!
//! It is indexed as a whole, and again whenever a note in it changes on disk. That way notes that were put there
//! from outside, for example by a sync tool, are found by search without having been opened here.

// Imports
use crate::RnApp;
use futures::StreamExt;
use futures::channel::mpsc;
use gtk4::{gio, glib, prelude::*};
use notify::EventKind;
use notify::event::{AccessKind, AccessMode};
use notify_debouncer_full::notify;
use rnote_engine::notefolder::NoteFolder;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::{error, warn};

/// How long the library has to be quiet before it is indexed. A sync tool brings many files in a row.
const QUIET_TIME: Duration = Duration::from_secs(3);

thread_local! {
    static WATCH: RefCell<Option<Watch>> = const { RefCell::new(None) };
}

/// The watch on the library.
struct Watch {
    dir: PathBuf,
    task: glib::JoinHandle<()>,
}

fn app_settings() -> Option<gio::Settings> {
    gio::Application::default()
        .and_downcast::<RnApp>()
        .and_then(|app| app.app_settings())
}

/// The folder of the library, when there is one.
pub(crate) fn dir() -> Option<PathBuf> {
    let dir = app_settings()?.string("library-dir");
    (!dir.is_empty()).then(|| PathBuf::from(dir.as_str()))
}

/// Makes the folder the library. `None` leaves the app without one.
pub(crate) fn set_dir(dir: Option<&Path>) {
    // The index identifies files by their canonical path, and the library is compared with those
    let dir = dir.map(|dir| dir.canonicalize().unwrap_or(dir.to_path_buf()));
    let dir = dir
        .as_deref()
        .map(Path::to_string_lossy)
        .unwrap_or_default();
    let Some(app_settings) = app_settings() else {
        return;
    };
    if let Err(e) = app_settings.set_string("library-dir", &dir) {
        error!("Failed to set setting `library-dir`, Err: {e:?}");
    }
}

/// Indexes the library and starts to watch it for changes. To be called when the app starts and whenever another
/// folder becomes the library. Does nothing when the library is watched already.
pub(crate) fn watch() {
    let dir = dir();
    if WATCH.with_borrow(|watch| watch.as_ref().map(|watch| &watch.dir) == dir.as_ref()) {
        return;
    }
    if let Some(watch) = WATCH.take() {
        watch.task.abort();
    }
    let Some(dir) = dir else {
        return;
    };

    crate::indexer::queue(dir.clone());
    let task = glib::spawn_future_local(watch_changes(dir.clone()));
    WATCH.set(Some(Watch { dir, task }));
}

/// Whether the path is a rnote file, or a note folder or a file in one.
fn is_in_note(path: &Path) -> bool {
    let is_note_folder = |name: &Path| {
        name.extension()
            .is_some_and(|ext| ext == NoteFolder::EXTENSION)
    };
    path.extension().is_some_and(|ext| ext == "rnote")
        || path
            .components()
            .any(|component| is_note_folder(Path::new(component.as_os_str())))
}

/// Queues the library for indexing whenever notes in it changed.
async fn watch_changes(dir: PathBuf) {
    let (sender, mut receiver) = mpsc::unbounded();
    let debouncer = notify_debouncer_full::new_debouncer(QUIET_TIME, None, move |result| {
        if let Err(e) = sender.unbounded_send(result) {
            error!("Library watcher reported changes, but sending them failed, Err: {e:?}");
        }
    });
    let mut debouncer = match debouncer {
        Ok(debouncer) => debouncer,
        Err(e) => {
            error!("Failed to create the library watcher, Err: {e:?}");
            return;
        }
    };
    if let Err(e) = debouncer.watch(&dir, notify::RecursiveMode::Recursive) {
        error!("Failed to start watching the library {dir:?}, Err: {e:?}");
        return;
    }

    while let Some(result) = receiver.next().await {
        match result {
            Ok(events) => {
                let notes_changed = events.iter().any(|event| {
                    // Reading a note changes nothing, and indexing reads them
                    let changes = match event.kind {
                        EventKind::Access(kind) => kind == AccessKind::Close(AccessMode::Write),
                        _ => true,
                    };
                    changes && event.paths.iter().any(|path| is_in_note(path))
                });
                if notes_changed {
                    // Files that did not change are skipped when the library is indexed
                    crate::indexer::queue(dir.clone());
                }
            }
            Err(e) => warn!("Library watcher sent errors, Err: {e:?}"),
        }
    }
}
