//! Keeps the search index up to date by running `rnote-cli index` in the background.
//!
//! Recognising text loads models and renders pages. Doing that in the cli keeps the memory out of the app and gives it
//! back when the cli exits.

// Imports
use crate::RnApp;
use futures::StreamExt;
use futures::channel::mpsc;
use gtk4::{gio, glib, prelude::*};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use tracing::warn;

thread_local! {
    static QUEUE: mpsc::UnboundedSender<PathBuf> = start();
    /// How many paths are queued or being indexed.
    static PENDING: Cell<usize> = const { Cell::new(0) };
    /// How many queued paths were indexed since the app started.
    static FINISHED: Cell<u64> = const { Cell::new(0) };
    /// The queued paths whose indexing has not started yet.
    static WAITING: RefCell<HashSet<PathBuf>> = RefCell::new(HashSet::new());
}

/// Whether files are still being indexed. Until they are done, search does not find all of their text.
pub(crate) fn is_busy() -> bool {
    PENDING.get() > 0
}

/// How many queued paths were indexed since the app started. When it grew, search may find more than before.
pub(crate) fn finished_count() -> u64 {
    FINISHED.get()
}

/// Queues a rnote file, or a folder with all rnote files in it, to be indexed. They are indexed one at a time, in
/// the order they were queued.
///
/// Queuing a file that did not change since it was indexed costs next to nothing.
pub(crate) fn queue(path: PathBuf) {
    // Queuing a path again while it still waits would change nothing
    if !WAITING.with_borrow_mut(|waiting| waiting.insert(path.clone())) {
        return;
    }
    QUEUE.with(|queue| match queue.unbounded_send(path) {
        Ok(()) => PENDING.set(PENDING.get() + 1),
        Err(e) => warn!("Queuing path for indexing failed, Err: {e:?}"),
    });
}

fn start() -> mpsc::UnboundedSender<PathBuf> {
    let (sender, mut receiver) = mpsc::unbounded::<PathBuf>();
    glib::spawn_future_local(async move {
        while let Some(path) = receiver.next().await {
            WAITING.with_borrow_mut(|waiting| waiting.remove(&path));
            if let Err(e) = index(&path).await {
                warn!("Indexing {path:?} failed, Err: {e:?}");
            }
            PENDING.set(PENDING.get() - 1);
            FINISHED.set(FINISHED.get() + 1);
        }
    });
    sender
}

async fn index(path: &Path) -> anyhow::Result<()> {
    // The cli is installed next to the app
    let cli_name = format!("rnote-cli{}", std::env::consts::EXE_SUFFIX);
    let cli = std::env::current_exe()?.with_file_name(&cli_name);
    let cli = if cli.exists() {
        cli
    } else {
        PathBuf::from(cli_name)
    };

    let mut args = vec![cli.as_os_str(), OsStr::new("index"), path.as_os_str()];
    if search_zhuyin() {
        args.push(OsStr::new("--zhuyin"));
    }
    gio::Subprocess::newv(&args, gio::SubprocessFlags::STDOUT_SILENCE)?
        .wait_check_future()
        .await?;
    Ok(())
}

/// The experimental setting to also read zhuyin.
fn search_zhuyin() -> bool {
    gio::Application::default()
        .and_downcast::<RnApp>()
        .and_then(|app| app.app_settings())
        .is_some_and(|settings| settings.boolean("search-zhuyin"))
}
