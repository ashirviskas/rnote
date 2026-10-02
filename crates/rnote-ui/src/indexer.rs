//! Keeps the search index up to date by running `rnote-cli index` in the background.
//!
//! Recognising text loads models and renders pages. Doing that in the cli keeps the memory out of the app and gives it
//! back when the cli exits.

// Imports
use crate::RnApp;
use futures::StreamExt;
use futures::channel::mpsc;
use gtk4::{gio, glib, prelude::*};
use std::cell::Cell;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use tracing::warn;

thread_local! {
    static QUEUE: mpsc::UnboundedSender<PathBuf> = start();
    /// How many files are queued or being indexed.
    static PENDING: Cell<usize> = const { Cell::new(0) };
}

/// Whether files are still being indexed. Until they are done, search does not find all of their text.
pub(crate) fn is_busy() -> bool {
    PENDING.get() > 0
}

/// Queues a rnote file to be indexed. The files are indexed one at a time, in the order they were queued.
///
/// Queuing a file that did not change since it was indexed costs next to nothing.
pub(crate) fn queue(rnote_file: PathBuf) {
    QUEUE.with(|queue| match queue.unbounded_send(rnote_file) {
        Ok(()) => PENDING.set(PENDING.get() + 1),
        Err(e) => warn!("Queuing file for indexing failed, Err: {e:?}"),
    });
}

fn start() -> mpsc::UnboundedSender<PathBuf> {
    let (sender, mut receiver) = mpsc::unbounded::<PathBuf>();
    glib::spawn_future_local(async move {
        while let Some(rnote_file) = receiver.next().await {
            if let Err(e) = index(&rnote_file).await {
                warn!("Indexing file {rnote_file:?} failed, Err: {e:?}");
            }
            PENDING.set(PENDING.get() - 1);
        }
    });
    sender
}

async fn index(rnote_file: &Path) -> anyhow::Result<()> {
    // The cli is installed next to the app
    let cli_name = format!("rnote-cli{}", std::env::consts::EXE_SUFFIX);
    let cli = std::env::current_exe()?.with_file_name(&cli_name);
    let cli = if cli.exists() {
        cli
    } else {
        PathBuf::from(cli_name)
    };

    let mut args = vec![cli.as_os_str(), OsStr::new("index"), rnote_file.as_os_str()];
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
