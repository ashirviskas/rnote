// Imports
use crate::appwindow::RnAppWindow;
use crate::workspacebrowser::RnWorkspaceBrowser;
use adw::prelude::*;
use gettextrs::gettext;
use gtk4::{gio, glib, glib::clone, subclass::prelude::*};
use p2d::bounding_volume::Aabb;
use p2d::math::Vector2;
use rnote_ocr::{Hit, Index};
use tracing::error;

/// Rows for more hits than this are not created. All hits are still highlighted on the canvas.
const MAX_ROWS: usize = 200;

impl RnWorkspaceBrowser {
    pub(super) fn setup_search(&self, appwindow: &RnAppWindow) {
        let imp = self.imp();

        imp.search_entry.connect_search_changed(clone!(
            #[weak(rename_to=workspacebrowser)]
            self,
            #[weak]
            appwindow,
            move |search_entry| {
                let query = search_entry.text().to_string();
                if query.trim().is_empty() {
                    workspacebrowser.show_search_hits(None, &appwindow);
                    return;
                }
                glib::spawn_future_local(clone!(
                    #[weak]
                    workspacebrowser,
                    #[weak]
                    appwindow,
                    async move {
                        let search = blocking::unblock(clone!(
                            #[strong]
                            query,
                            move || Index::open()?.search(&query)
                        ));
                        match search.await {
                            // The query might have changed while searching
                            Ok(hits) if workspacebrowser.imp().search_entry.text() == query => {
                                workspacebrowser.show_search_hits(Some(hits), &appwindow);
                            }
                            Ok(_) => {}
                            Err(e) => {
                                error!("Searching the notes failed, Err: {e:?}");
                                appwindow
                                    .overlays()
                                    .dispatch_toast_error(&gettext("Searching the notes failed"));
                            }
                        }
                    }
                ));
            }
        ));

        imp.search_results_listbox.connect_row_activated(clone!(
            #[weak(rename_to=workspacebrowser)]
            self,
            #[weak]
            appwindow,
            move |_, row| {
                let hits = workspacebrowser.imp().search_hits.borrow();
                let Some(hit) = hits.get(row.index() as usize).cloned() else {
                    return;
                };
                let highlights = hits
                    .iter()
                    .filter(|other| other.path == hit.path)
                    .map(hit_bounds)
                    .collect::<Vec<Aabb>>();
                glib::spawn_future_local(clone!(
                    #[weak]
                    appwindow,
                    async move {
                        open_hit(&appwindow, hit, highlights).await;
                    }
                ));
            }
        ));
    }

    /// Shows the hits in place of the files. `None` shows the files again and clears the highlights on all tabs.
    fn show_search_hits(&self, hits: Option<Vec<Hit>>, appwindow: &RnAppWindow) {
        let imp = self.imp();

        imp.search_results_listbox.remove_all();
        let Some(hits) = hits else {
            imp.search_hits.take();
            imp.files_scroller.set_child(Some(&*imp.files_listview));
            for canvas in appwindow.get_all_tabs().into_iter().map(|tab| tab.canvas()) {
                let widget_flags = canvas.engine_mut().set_search_highlights(Vec::new());
                appwindow.handle_widget_flags(widget_flags, &canvas);
            }
            return;
        };

        for hit in hits.iter().take(MAX_ROWS) {
            let file_name = hit
                .path
                .file_stem()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default();
            let row = adw::ActionRow::builder()
                .use_markup(false)
                .title(hit.text.as_str())
                .subtitle(format!(
                    "{file_name} · {} {}",
                    gettext("Page"),
                    hit.page + 1
                ))
                .title_lines(2)
                .activatable(true)
                .build();
            imp.search_results_listbox.append(&row);
        }
        imp.search_hits.replace(hits);
        imp.files_scroller
            .set_child(Some(&*imp.search_results_listbox));
    }
}

fn hit_bounds(hit: &Hit) -> Aabb {
    let mins = Vector2::new(hit.bounds.x, hit.bounds.y);
    Aabb::new(mins, mins + Vector2::new(hit.bounds.w, hit.bounds.h))
}

/// Opens the file of the hit in a tab, highlights the hits of the file and moves the view to the hit.
async fn open_hit(appwindow: &RnAppWindow, hit: Hit, highlights: Vec<Aabb>) {
    appwindow
        .open_file_w_dialogs(gio::File::for_path(&hit.path), None, true)
        .await;
    let Some(canvas) = appwindow.active_tab_canvas() else {
        return;
    };
    let opened = canvas
        .output_file()
        .and_then(|file| file.path())
        .is_some_and(|path| crate::utils::paths_abs_eq(path, &hit.path).unwrap_or(false));
    if !opened {
        return;
    }

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
            .set_viewport_center(hit_bounds(&hit).center());
        canvas.emit_handle_widget_flags(widget_flags);
        glib::ControlFlow::Break
    });
}
