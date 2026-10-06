// Imports
use crate::RnCanvas;
use crate::appwindow::RnAppWindow;
use crate::doctext::{self, DocumentLine};
use crate::workspacebrowser::RnWorkspaceBrowser;
use adw::prelude::*;
use gettextrs::gettext;
use gtk4::{gio, glib, glib::clone, subclass::prelude::*};
use p2d::bounding_volume::Aabb;
use p2d::math::Vector2;
use rnote_ocr::{Hit, Index, Query};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use tracing::error;

/// Rows for more hits than this are not created. All hits are still highlighted on the canvas.
const MAX_ROWS: usize = 200;

/// Where a search looks. The names are the ones of the scope toggles and of the `search-scope` setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchScope {
    /// The open document.
    Document,
    /// The folder the files list shows, with the folders in it.
    Folder,
    /// All notes that are indexed.
    All,
}

impl SearchScope {
    fn from_name(name: &str) -> Self {
        match name {
            "document" => Self::Document,
            "folder" => Self::Folder,
            _ => Self::All,
        }
    }
}

/// The part of the index a search looks through.
#[derive(Debug, Clone, PartialEq, Eq)]
enum IndexScope {
    /// No part of it: the scope is a document that was never saved, which the index does not know.
    Nothing,
    /// One file, or the files below a folder.
    Path(PathBuf),
    Everything,
}

/// A hit in the list of search results.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SearchResult {
    hit: Hit,
    /// Where the document is, for display.
    title: String,
    /// The open document the hit was found in. A hit from the index has none, its file gets opened.
    canvas: Option<glib::WeakRef<RnCanvas>>,
}

impl SearchResult {
    fn bounds(&self) -> Aabb {
        let mins = Vector2::new(self.hit.bounds.x, self.hit.bounds.y);
        Aabb::new(
            mins,
            mins + Vector2::new(self.hit.bounds.w, self.hit.bounds.h),
        )
    }

    /// Whether both hits are in the same document. A document that was never saved has no path to tell it by.
    fn same_document(&self, other: &Self) -> bool {
        if self.hit.path.as_os_str().is_empty() {
            self.canvas.is_some() && self.canvas == other.canvas
        } else {
            self.hit.path == other.hit.path
        }
    }
}

impl RnWorkspaceBrowser {
    pub(super) fn setup_search(&self, appwindow: &RnAppWindow) {
        let imp = self.imp();

        imp.search_entry.connect_search_changed(clone!(
            #[weak(rename_to=workspacebrowser)]
            self,
            #[weak]
            appwindow,
            move |_| workspacebrowser.refresh_search(&appwindow)
        ));
        imp.search_scope_togglegroup
            .connect_active_name_notify(clone!(
                #[weak(rename_to=workspacebrowser)]
                self,
                #[weak]
                appwindow,
                move |_| workspacebrowser.refresh_search(&appwindow)
            ));
        // The folder scope follows the files list
        imp.dir_list.connect_file_notify(clone!(
            #[weak(rename_to=workspacebrowser)]
            self,
            #[weak]
            appwindow,
            move |_| {
                let searching = !workspacebrowser.imp().search_entry.text().is_empty();
                if searching && workspacebrowser.search_scope() == SearchScope::Folder {
                    workspacebrowser.refresh_search(&appwindow);
                }
            }
        ));

        // While files are being indexed, more and more of their text can be found. The results follow along,
        // and are brought up to date once more when the indexer is done. A short run can start and end between
        // two looks at the indexer, so what it finished is counted.
        let seen_finished = Cell::new(crate::indexer::finished_count());
        glib::timeout_add_seconds_local(
            2,
            clone!(
                #[weak(rename_to=workspacebrowser)]
                self,
                #[weak]
                appwindow,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || {
                    let finished = crate::indexer::finished_count();
                    if crate::indexer::is_busy() || finished != seen_finished.get() {
                        workspacebrowser.refresh_search(&appwindow);
                    }
                    seen_finished.set(finished);
                    glib::ControlFlow::Continue
                }
            ),
        );

        imp.search_results_listbox.connect_row_activated(clone!(
            #[weak(rename_to=workspacebrowser)]
            self,
            #[weak]
            appwindow,
            move |_, row| {
                let results = workspacebrowser.imp().search_results.borrow();
                let Some(result) = results.get(row.index() as usize).cloned() else {
                    return;
                };
                let highlights = results
                    .iter()
                    .filter(|other| other.same_document(&result))
                    .map(SearchResult::bounds)
                    .collect::<Vec<Aabb>>();
                glib::spawn_future_local(clone!(
                    #[weak]
                    appwindow,
                    async move {
                        show_result(&appwindow, result, highlights).await;
                    }
                ));
            }
        ));
    }

    fn search_scope(&self) -> SearchScope {
        let name = self.imp().search_scope_togglegroup.active_name();
        SearchScope::from_name(name.as_deref().unwrap_or_default())
    }

    /// Searches for the text in the search entry and shows the results. Shows the files when it is empty.
    fn refresh_search(&self, appwindow: &RnAppWindow) {
        let text = self.imp().search_entry.text().to_string();
        let Some(query) = Query::new(&text) else {
            self.show_search_results(None, appwindow);
            return;
        };
        let scope = self.search_scope();
        let canvas = appwindow.active_tab_canvas();
        let document_path = canvas.as_ref().and_then(doctext::index_path);
        // The index identifies files by their canonical path
        let folder = self
            .dir_list_dir()
            .map(|dir| dir.canonicalize().unwrap_or(dir));
        let index_scope = match scope {
            SearchScope::Document => document_path
                .clone()
                .map_or(IndexScope::Nothing, IndexScope::Path),
            SearchScope::Folder => folder.clone().map_or(IndexScope::Nothing, IndexScope::Path),
            SearchScope::All => IndexScope::Everything,
        };
        let document_in_scope = match scope {
            SearchScope::Document | SearchScope::All => true,
            SearchScope::Folder => document_path
                .as_ref()
                .zip(folder.as_ref())
                .is_some_and(|(path, folder)| path.starts_with(folder)),
        };
        // Results say where their note is: below the library, else below the shown folder
        let roots = crate::library::dir()
            .into_iter()
            .chain(folder)
            .collect::<Vec<PathBuf>>();

        // The open document is searched as it is right now, whether it was saved or not. Everything else, and
        // its handwriting, is found through the index.
        let in_document = canvas
            .filter(|_| document_in_scope)
            .map(|canvas| document_results(&canvas, &query, &roots))
            .unwrap_or_default();
        glib::spawn_future_local(clone!(
            #[weak(rename_to=workspacebrowser)]
            self,
            #[weak]
            appwindow,
            async move {
                let search = blocking::unblock(clone!(
                    #[strong]
                    text,
                    move || match index_scope {
                        IndexScope::Nothing => Ok(Vec::new()),
                        IndexScope::Path(path) => Index::open()?.search(&text, Some(&path)),
                        IndexScope::Everything => Index::open()?.search(&text, None),
                    }
                ));
                let indexed = search.await.unwrap_or_else(|e| {
                    error!("Searching the index failed, Err: {e:?}");
                    appwindow
                        .overlays()
                        .dispatch_toast_error(&gettext("Searching the notes failed"));
                    Vec::new()
                });
                // The query or the scope might have changed while searching
                if workspacebrowser.imp().search_entry.text() == text
                    && workspacebrowser.search_scope() == scope
                {
                    let results = merge_results(in_document, indexed, &roots);
                    workspacebrowser.show_search_results(Some(results), &appwindow);
                }
            }
        ));
    }

    /// Puts the keyboard focus into the search entry.
    pub(crate) fn focus_search(&self) {
        self.imp().search_entry.grab_focus();
    }

    /// Shows the results in place of the files. `None` shows the files again and clears the highlights on all tabs.
    fn show_search_results(&self, results: Option<Vec<SearchResult>>, appwindow: &RnAppWindow) {
        let imp = self.imp();

        let Some(results) = results else {
            // Not `remove_all()`: that takes the placeholder out as well
            while let Some(row) = imp.search_results_listbox.row_at_index(0) {
                imp.search_results_listbox.remove(&row);
            }
            imp.search_results.take();
            imp.files_scroller.set_child(Some(&*imp.files_listview));
            for canvas in appwindow.get_all_tabs().into_iter().map(|tab| tab.canvas()) {
                let widget_flags = canvas.engine_mut().set_search_highlights(Vec::new());
                appwindow.handle_widget_flags(widget_flags, &canvas);
            }
            return;
        };

        // Shown when there are no results. Text that has to be recognised is only found once the document is
        // saved and the background indexer is through with it.
        let unsaved = appwindow
            .active_tab_canvas()
            .is_some_and(|canvas| canvas.output_file().is_none());
        imp.search_results_placeholder
            .set_label(&if crate::indexer::is_busy() {
                gettext("No results yet. Notes are still being read in the background.")
            } else if unsaved {
                gettext("No results. Handwriting and images are found once the document is saved.")
            } else {
                gettext("No Results")
            });

        // The rows are left alone when nothing changed, as is usual when the results are refreshed while files
        // are indexed
        if results != *imp.search_results.borrow() {
            while let Some(row) = imp.search_results_listbox.row_at_index(0) {
                imp.search_results_listbox.remove(&row);
            }
            for result in results.iter().take(MAX_ROWS) {
                let row = adw::ActionRow::builder()
                    .title_lines(2)
                    .activatable(true)
                    .build();
                // Recognised text is full of `<` and `&`. Markup has to be off before the texts are set, which
                // the builder does not guarantee when it is given all of them at once.
                row.set_use_markup(false);
                row.set_title(&result.hit.text);
                row.set_subtitle(&format!(
                    "{} · {} {}",
                    result.title,
                    gettext("Page"),
                    result.hit.page + 1
                ));
                imp.search_results_listbox.append(&row);
            }
            imp.search_results.replace(results);
        }
        if imp.search_results_box.parent().is_none() {
            imp.files_scroller.set_child(Some(&*imp.search_results_box));
        }
    }
}

/// Searches the text the document carries: typed text and the text of imported Pdf pages. That needs no
/// recognition, so it is done on the spot.
fn document_results(canvas: &RnCanvas, query: &Query, roots: &[PathBuf]) -> Vec<SearchResult> {
    let path = doctext::index_path(canvas).unwrap_or_default();
    let title = if path.as_os_str().is_empty() {
        canvas.doc_title_display()
    } else {
        note_location(&path, roots)
    };

    let mut results = Vec::new();
    for DocumentLine { page, line } in doctext::document_lines(canvas) {
        results.extend(query.find(&line).into_iter().map(|found| SearchResult {
            hit: Hit {
                path: path.clone(),
                page,
                bounds: found.bounds,
                text: line.text(),
                score: found.score,
            },
            title: title.clone(),
            canvas: Some(canvas.downgrade()),
        }));
    }
    results
}

/// Puts the hits of the open document and the hits from the index together, the best first.
///
/// When the open document is saved, the index knows its typed text and Pdf text too. Those hits are left out.
fn merge_results(
    mut in_document: Vec<SearchResult>,
    indexed: Vec<Hit>,
    roots: &[PathBuf],
) -> Vec<SearchResult> {
    let is_duplicate = |hit: &Hit| {
        in_document.iter().any(|known| {
            known.hit.path == hit.path && doctext::same_place(known.hit.bounds, hit.bounds)
        })
    };
    let indexed = indexed
        .into_iter()
        .filter(|hit| !is_duplicate(hit))
        .map(|hit| SearchResult {
            title: note_location(&hit.path, roots),
            hit,
            canvas: None,
        })
        .collect::<Vec<SearchResult>>();
    in_document.extend(indexed);
    in_document.sort_by(|a, b| b.hit.score.total_cmp(&a.hit.score));
    in_document
}

/// Where a note is, for display: its folders below the first of the roots it is in, and its name. A note that is in
/// none of them is shown by its name alone.
fn note_location(path: &Path, roots: &[PathBuf]) -> String {
    let below_root = roots.iter().find_map(|root| path.strip_prefix(root).ok());
    let shown = below_root.unwrap_or_else(|| Path::new(path.file_name().unwrap_or_default()));
    shown
        .with_extension("")
        .iter()
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" / ")
}

/// Shows the document of the result, highlights the hits in it and moves the view to the result.
async fn show_result(appwindow: &RnAppWindow, result: SearchResult, highlights: Vec<Aabb>) {
    let canvas = match result.canvas.as_ref().and_then(|canvas| canvas.upgrade()) {
        // The document is open: go to its tab
        Some(canvas) => {
            let tabs = appwindow.get_all_tabs();
            let Some(wrapper) = tabs.into_iter().find(|tab| tab.canvas() == canvas) else {
                return;
            };
            let tabview = appwindow.overlays().tabview();
            tabview.set_selected_page(&tabview.page(&wrapper));
            canvas
        }
        None if result.hit.path.as_os_str().is_empty() => return,
        None => {
            appwindow
                .open_file_w_dialogs(gio::File::for_path(&result.hit.path), None, true)
                .await;
            let Some(canvas) = appwindow.active_tab_canvas() else {
                return;
            };
            let opened = canvas
                .output_file()
                .and_then(|file| file.path())
                .is_some_and(|path| {
                    crate::utils::paths_abs_eq(path, &result.hit.path).unwrap_or(false)
                });
            if !opened {
                return;
            }
            canvas
        }
    };

    let widget_flags = canvas.engine_mut().set_search_highlights(highlights);
    appwindow.handle_widget_flags(widget_flags, &canvas);
    // The view can only be centered once the canvas has a size, which a new tab gets with its first frame.
    // The camera of a just loaded file still has the size it was saved with.
    canvas.add_tick_callback(move |canvas, _| {
        if canvas.width() == 0 {
            return glib::ControlFlow::Continue;
        }
        let size = Vector2::new(canvas.width() as f64, canvas.height() as f64);
        let mut widget_flags = canvas.engine_mut().camera_set_size(size);
        widget_flags |= canvas
            .engine_mut()
            .camera
            .set_viewport_center(result.bounds().center());
        canvas.emit_handle_widget_flags(widget_flags);
        glib::ControlFlow::Break
    });
}
