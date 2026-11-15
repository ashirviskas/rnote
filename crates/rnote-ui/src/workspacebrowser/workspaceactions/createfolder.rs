// Imports
use crate::workspacebrowser::widgethelper;
use crate::{RnAppWindow, RnWorkspaceBrowser};
use gettextrs::gettext;
use gtk4::{Align, Entry, Label, gio, glib, glib::clone, pango, prelude::*};
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

/// Create a new `create_folder` action, for a folder in the folder the browser shows.
pub(crate) fn create_folder(
    workspacebrowser: &RnWorkspaceBrowser,
    appwindow: &RnAppWindow,
) -> gio::SimpleAction {
    let new_folder_action = gio::SimpleAction::new("create-folder", None);

    new_folder_action.connect_activate(clone!(
        #[weak]
        workspacebrowser,
        #[weak]
        appwindow,
        move |_, _| {
            if let Some(parent_path) = workspacebrowser.dir_list_file().and_then(|f| f.path()) {
                new_folder_dialog(
                    parent_path,
                    &workspacebrowser.dir_controls_actions_box(),
                    &appwindow,
                );
            } else {
                warn!("Can't create new folder when there currently is no workspace selected");
            }
        }
    ));

    new_folder_action
}

/// Asks for the name of a new folder in `parent_path` and creates it. The dialog pops up from `host`.
///
/// The browser's folder list follows the file system, so the new folder shows up by itself.
pub(crate) fn new_folder_dialog(parent_path: PathBuf, host: &gtk4::Box, appwindow: &RnAppWindow) {
    let folder_name_entry = create_folder_name_entry();
    let dialog_title_label = create_dialog_title_label(&parent_path);
    let (apply_button, popover) =
        widgethelper::create_entry_dialog(&folder_name_entry, &dialog_title_label);

    // at first don't allow applying, since the user did not enter any text yet.
    apply_button.set_sensitive(false);

    host.append(&popover);

    folder_name_entry.connect_changed(clone!(
        #[weak]
        apply_button,
        #[strong]
        parent_path,
        move |entry| {
            if is_valid_folder_name(&parent_path, &entry.text()) {
                apply_button.set_sensitive(true);
                entry.remove_css_class("error");
            } else {
                apply_button.set_sensitive(false);
                entry.add_css_class("error");
            }
        }
    ));

    // Enter in the entry creates the folder, like the apply button
    folder_name_entry.connect_activate(clone!(
        #[weak]
        apply_button,
        move |_| {
            if apply_button.is_sensitive() {
                apply_button.emit_clicked();
            }
        }
    ));

    apply_button.connect_clicked(clone!(
        #[weak]
        popover,
        #[weak]
        folder_name_entry,
        #[weak]
        appwindow,
        move |_| {
            let name = folder_name_entry.text();
            if !is_valid_folder_name(&parent_path, &name) {
                // Should have been caught earlier, but making sure
                appwindow
                    .overlays()
                    .dispatch_toast_error(&gettext("Can't create a folder with that name"));
                debug!("Couldn't create new folder with name `{name}`, invalid or already exists.");
                return;
            }
            if let Err(e) = fs_extra::dir::create(parent_path.join(name.trim()), false) {
                appwindow
                    .overlays()
                    .dispatch_toast_error(&gettext("Creating new folder failed"));
                debug!("Couldn't create folder, Err: {e:?}");
            }
            popover.popdown();
        }
    ));

    popover.popup();
    folder_name_entry.grab_focus();
}

/// A name for a new folder in `parent_path`: not empty, a single path component, and not taken yet.
fn is_valid_folder_name(parent_path: &Path, name: &str) -> bool {
    let name = name.trim();
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.chars().any(std::path::is_separator)
        && !parent_path.join(name).exists()
}

fn create_folder_name_entry() -> Entry {
    Entry::builder()
        .placeholder_text(gettext("Folder Name"))
        .build()
}

fn create_dialog_title_label(parent_path: &Path) -> Label {
    let parent_name = parent_path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| parent_path.display().to_string());
    let label = Label::builder()
        .margin_bottom(12)
        .halign(Align::Center)
        .label(format!("{} “{parent_name}”", gettext("New Folder in")))
        .width_chars(24)
        .max_width_chars(32)
        .ellipsize(pango::EllipsizeMode::Middle)
        .build();
    label.add_css_class("title-4");
    label
}

#[cfg(test)]
mod tests {
    use super::is_valid_folder_name;
    use std::path::Path;

    #[test]
    fn folder_names_are_single_new_components() {
        // Tests run in the crate folder, which has `src`
        let dir = Path::new(".");
        assert!(is_valid_folder_name(dir, "a-folder-that-does-not-exist"));
        assert!(!is_valid_folder_name(dir, ""));
        assert!(!is_valid_folder_name(dir, "  "));
        assert!(!is_valid_folder_name(dir, ".."));
        assert!(!is_valid_folder_name(dir, "a/b"));
        assert!(!is_valid_folder_name(dir, "src"));
    }
}
