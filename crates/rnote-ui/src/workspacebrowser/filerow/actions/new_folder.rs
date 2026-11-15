// Imports
use crate::RnAppWindow;
use crate::workspacebrowser::RnFileRow;
use crate::workspacebrowser::workspaceactions::new_folder_dialog;
use gtk4::{gio, glib, glib::clone, prelude::*};

/// Create a new `new-folder` action: a folder inside the row's folder, or next to the row's file.
pub(crate) fn new_folder(filerow: &RnFileRow, appwindow: &RnAppWindow) -> gio::SimpleAction {
    let action = gio::SimpleAction::new("new-folder", None);
    action.connect_activate(clone!(
        #[weak]
        filerow,
        #[weak]
        appwindow,
        move |_, _| {
            let Some(path) = filerow.current_file().and_then(|file| file.path()) else {
                return;
            };
            let parent_path = if path.is_dir() {
                path
            } else {
                let Some(parent) = path.parent() else {
                    return;
                };
                parent.to_path_buf()
            };
            new_folder_dialog(parent_path, &filerow.menubutton_box(), &appwindow);
        }
    ));
    action
}
