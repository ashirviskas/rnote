// Modules
mod workspacelist;
mod workspacelistentry;
mod workspacerow;

use gtk4::ConstantExpression;
// Re-exports
pub(crate) use workspacelist::RnWorkspaceList;
pub(crate) use workspacelistentry::RnWorkspaceListEntry;
pub(crate) use workspacerow::RnWorkspaceRow;

// Imports
use crate::appwindow::RnAppWindow;
use crate::dialogs;
use gtk4::{
    Button, CompositeTemplate, ListBox, ScrolledWindow, Widget, gdk, gio, glib, glib::clone,
    prelude::*, subclass::prelude::*,
};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use tracing::{error, warn};

mod imp {
    use super::*;

    #[derive(Debug, CompositeTemplate)]
    #[template(resource = "/com/github/flxzt/rnote/ui/workspacesbar/workspacesbar.ui")]
    pub(crate) struct RnWorkspacesBar {
        pub(crate) action_group: gio::SimpleActionGroup,
        pub(crate) workspace_list: RnWorkspaceList,
        /// The folder of the library, while its entry is the first of the list.
        pub(crate) library_dir: RefCell<Option<PathBuf>>,

        #[template_child]
        pub(crate) workspaces_scroller: TemplateChild<ScrolledWindow>,
        #[template_child]
        pub(crate) workspaces_listbox: TemplateChild<ListBox>,
        #[template_child]
        pub(crate) move_selected_workspace_up_button: TemplateChild<Button>,
        #[template_child]
        pub(crate) move_selected_workspace_down_button: TemplateChild<Button>,
        #[template_child]
        pub(crate) add_workspace_button: TemplateChild<Button>,
        #[template_child]
        pub(crate) remove_selected_workspace_button: TemplateChild<Button>,
        #[template_child]
        pub(crate) edit_selected_workspace_button: TemplateChild<Button>,
    }

    impl Default for RnWorkspacesBar {
        fn default() -> Self {
            Self {
                action_group: gio::SimpleActionGroup::new(),
                workspace_list: RnWorkspaceList::default(),
                library_dir: RefCell::default(),

                workspaces_scroller: Default::default(),
                workspaces_listbox: Default::default(),
                move_selected_workspace_up_button: Default::default(),
                move_selected_workspace_down_button: Default::default(),
                add_workspace_button: Default::default(),
                remove_selected_workspace_button: Default::default(),
                edit_selected_workspace_button: Default::default(),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RnWorkspacesBar {
        const NAME: &'static str = "RnWorkspacesBar";
        type Type = super::RnWorkspacesBar;
        type ParentType = Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for RnWorkspacesBar {
        fn constructed(&self) {
            let obj = self.obj();
            self.parent_constructed();

            obj.insert_action_group("workspacesbar", Some(&self.action_group));
        }

        fn dispose(&self) {
            self.dispose_template();
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for RnWorkspacesBar {}
}

glib::wrapper! {
    pub(crate) struct RnWorkspacesBar(ObjectSubclass<imp::RnWorkspacesBar>)
        @extends Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl Default for RnWorkspacesBar {
    fn default() -> Self {
        Self::new()
    }
}

impl RnWorkspacesBar {
    pub(crate) fn new() -> Self {
        glib::Object::new()
    }

    pub(crate) fn action_group(&self) -> gio::SimpleActionGroup {
        self.imp().action_group.clone()
    }

    pub(crate) fn workspaces_scroller(&self) -> ScrolledWindow {
        self.imp().workspaces_scroller.clone()
    }

    pub(crate) fn push_workspace(&self, entry: RnWorkspaceListEntry) {
        self.imp().workspace_list.push(entry);

        let n_items = self.imp().workspace_list.n_items();
        self.select_workspace_by_index(n_items.saturating_sub(1));
    }

    pub(crate) fn insert_workspace_entry(&self, i: u32, entry: RnWorkspaceListEntry) {
        self.imp().workspace_list.insert(i as usize, entry);
        self.select_workspace_by_index(i);
    }

    /// Puts the entry of the library first in the list. Without a library there is no such entry.
    ///
    /// The entry is not saved with the workspaces, and it can not be removed, edited or moved.
    pub(crate) fn set_library(&self, dir: Option<PathBuf>) {
        let imp = self.imp();
        let selected = self.selected_workspace_index().unwrap_or(0);

        let had_entry = imp.library_dir.take().is_some();
        if had_entry {
            imp.workspace_list.remove(0);
        }
        if let Some(dir) = dir.as_ref() {
            imp.workspace_list
                .insert(0, RnWorkspaceListEntry::library(dir));
        }
        let has_entry = dir.is_some();
        imp.library_dir.replace(dir);

        // The workspace that was selected stays selected
        self.select_workspace_by_index(
            (selected + has_entry as u32).saturating_sub(had_entry as u32),
        );
        self.update_buttons();
    }

    /// The index of the first entry that is a workspace of the user.
    fn first_workspace_index(&self) -> u32 {
        self.imp().library_dir.borrow().is_some() as u32
    }

    fn library_selected(&self) -> bool {
        self.selected_workspace_index()
            .is_some_and(|i| i < self.first_workspace_index())
    }

    fn update_buttons(&self) {
        let imp = self.imp();
        let n_items = imp.workspace_list.n_items();
        let is_workspace = n_items > 0 && !self.library_selected();

        imp.move_selected_workspace_up_button
            .set_sensitive(is_workspace);
        imp.move_selected_workspace_down_button
            .set_sensitive(is_workspace);
        imp.remove_selected_workspace_button
            .set_sensitive(is_workspace && n_items > 1);
        imp.edit_selected_workspace_button
            .set_sensitive(is_workspace);
    }

    pub(crate) fn remove_selected_workspace(&self) {
        let n_items = self.imp().workspace_list.n_items();

        // never remove the last row
        if n_items > 0 && !self.library_selected() {
            let i = self
                .selected_workspace_index()
                .unwrap_or_else(|| n_items.saturating_sub(1));

            self.imp().workspace_list.remove(i as usize);

            self.select_workspace_by_index(i);
        }
    }

    pub(crate) fn move_selected_workspace_up(&self) {
        let n_items = self.imp().workspace_list.n_items();

        let i = self
            .selected_workspace_index()
            .unwrap_or_else(|| n_items.saturating_sub(1));
        // The library stays first
        if n_items > 1 && i > self.first_workspace_index() {
            let entry = self.imp().workspace_list.remove(i as usize);
            self.insert_workspace_entry(i - 1, entry);
        }
    }

    pub(crate) fn move_selected_workspace_down(&self) {
        let n_items = self.imp().workspace_list.n_items();
        let i_max = n_items.saturating_sub(1);

        let i = self.selected_workspace_index().unwrap_or(i_max);
        if n_items > 1 && i >= self.first_workspace_index() {
            let entry = self.imp().workspace_list.remove(i as usize);
            let insert_i = (i + 1).min(i_max);
            self.insert_workspace_entry(insert_i, entry);
        }
    }

    pub(crate) fn select_workspace_by_index(&self, index: u32) {
        let n_items = self.imp().workspace_list.n_items();

        self.imp().workspaces_listbox.select_row(
            self.imp()
                .workspaces_listbox
                .row_at_index(index.min(n_items.saturating_sub(1)) as i32)
                .as_ref(),
        );
    }

    pub(crate) fn selected_workspace_index(&self) -> Option<u32> {
        self.imp()
            .workspaces_listbox
            .selected_row()
            .map(|r| r.index() as u32)
    }

    pub(crate) fn selected_workspacelistentry(&self) -> Option<RnWorkspaceListEntry> {
        self.selected_workspace_index().and_then(|i| {
            self.imp()
                .workspace_list
                .item(i)
                .map(|o| o.downcast::<RnWorkspaceListEntry>().unwrap())
        })
    }

    pub(crate) fn replace_selected_workspacelistentry(&self, entry: RnWorkspaceListEntry) {
        if let Some(i) = self.selected_workspace_index() {
            self.imp().workspace_list.replace(i as usize, entry);

            self.select_workspace_by_index(i);
        }
    }

    #[allow(unused)]
    pub(crate) fn set_selected_workspace_dir(&self, dir: PathBuf) {
        if let Some(i) = self.selected_workspace_index() {
            let entry = self.imp().workspace_list.remove(i as usize);
            entry.set_dir(dir.to_string_lossy().into());
            self.imp().workspace_list.insert(i as usize, entry);

            self.select_workspace_by_index(i);
        }
    }

    #[allow(unused)]
    pub(crate) fn set_selected_workspace_icon(&self, icon: String) {
        if let Some(i) = self.selected_workspace_index() {
            let row = self.imp().workspace_list.remove(i as usize);
            row.set_icon(icon);
            self.imp().workspace_list.insert(i as usize, row);

            self.select_workspace_by_index(i);
        }
    }

    #[allow(unused)]
    pub(crate) fn set_selected_workspace_color(&self, color: gdk::RGBA) {
        if let Some(i) = self.selected_workspace_index() {
            let row = self.imp().workspace_list.remove(i as usize);
            row.set_color(color);
            self.imp().workspace_list.insert(i as usize, row);

            self.select_workspace_by_index(i);
        }
    }

    #[allow(unused)]
    pub(crate) fn set_selected_workspace_name(&self, name: String) {
        if let Some(i) = self.selected_workspace_index() {
            let row = self.imp().workspace_list.remove(i as usize);
            row.set_name(name);
            self.imp().workspace_list.insert(i as usize, row);

            self.select_workspace_by_index(i);
        }
    }

    pub(crate) fn save_to_settings(&self, settings: &gio::Settings) {
        let workspaces = self
            .imp()
            .workspace_list
            .to_vec()
            .split_off(self.first_workspace_index() as usize);
        if let Err(e) = settings.set("workspace-list", workspaces.to_variant()) {
            error!("Saving `workspace-list` to settings failed , Err: {e:?}");
        }

        if let Err(e) = settings.set(
            "selected-workspace-index",
            self.selected_workspace_index().unwrap_or(0),
        ) {
            error!("Saving `selected-workspace-index` to settings failed , Err: {e:?}");
        }
    }

    pub(crate) fn load_from_settings(&self, settings: &gio::Settings) {
        let workspace_list = settings.get::<RnWorkspaceList>("workspace-list");
        // Be sure to get the index before loading the workspaces, else the setting gets overridden
        let selected_workspace_index = settings.uint("selected-workspace-index");

        // don't canonicalize on windows, because that would convert the path to one with extended length syntax
        if !cfg!(target_os = "windows") {
            for entry in &workspace_list.iter() {
                if let Err(err) = entry.ensure_dir() {
                    warn!(
                        dir = entry.dir(),
                        name = entry.name(),
                        ?err,
                        "Failed to ensure dir",
                    );
                }
            }
        }

        self.imp().library_dir.take();
        self.imp().workspace_list.replace_self(workspace_list);
        self.set_library(crate::library::dir());
        self.select_workspace_by_index(selected_workspace_index);
    }

    pub(crate) fn init(&self, appwindow: &RnAppWindow) {
        self.setup_actions(appwindow);

        self.imp().workspace_list.connect_items_changed(clone!(
            #[weak(rename_to=workspacesbar)]
            self,
            move |_, _, _, _| workspacesbar.update_buttons()
        ));

        let workspace_listbox = self.imp().workspaces_listbox.get();
        workspace_listbox.connect_selected_rows_changed(clone!(
            #[weak]
            appwindow,
            #[weak(rename_to=workspacesbar)]
            self,
            move |_| {
                workspacesbar.update_buttons();
                if let Some(entry) = workspacesbar.selected_workspacelistentry() {
                    let dir = entry.dir();
                    let name = entry.name();
                    appwindow
                        .sidebar()
                        .workspacebrowser()
                        .active_workspace_name_label()
                        .set_label(&name);
                    appwindow
                        .sidebar()
                        .workspacebrowser()
                        .active_workspace_dir_label()
                        .set_label(&dir);
                    appwindow
                        .sidebar()
                        .workspacebrowser()
                        .set_dir_list_file(Some(&gio::File::for_path(dir)));
                }
            }
        ));

        // A click on the library shows the library itself again, wherever its entry was moved to by opening folders
        workspace_listbox.connect_row_activated(clone!(
            #[weak(rename_to=workspacesbar)]
            self,
            move |_, row| {
                let library_dir = workspacesbar.imp().library_dir.borrow().clone();
                let (0, Some(library_dir)) = (row.index(), library_dir) else {
                    return;
                };
                // Later, because setting the directory replaces the row that is being activated
                glib::idle_add_local_once(clone!(
                    #[weak]
                    workspacesbar,
                    move || {
                        let shows_library = workspacesbar
                            .selected_workspacelistentry()
                            .is_some_and(|entry| library_dir == Path::new(&entry.dir()));
                        if workspacesbar.library_selected() && !shows_library {
                            workspacesbar.set_selected_workspace_dir(library_dir);
                        }
                    }
                ));
            }
        ));

        workspace_listbox.bind_model(
            Some(&self.imp().workspace_list),
            clone!(
                #[weak]
                appwindow,
                #[upgrade_or_panic]
                move |obj| {
                    let entry = obj.to_owned().downcast::<RnWorkspaceListEntry>().unwrap();
                    let workspacerow = RnWorkspaceRow::new(&entry);
                    workspacerow.init(&appwindow);

                    let entry_expr = ConstantExpression::new(&entry);
                    entry_expr.bind(&workspacerow, "entry", None::<&glib::Object>);

                    workspacerow.upcast::<Widget>()
                }
            ),
        );

        self.imp()
            .move_selected_workspace_up_button
            .get()
            .connect_clicked(clone!(
                #[weak(rename_to=workspacesbar)]
                self,
                move |_| {
                    adw::prelude::ActionGroupExt::activate_action(
                        &workspacesbar.action_group(),
                        "move-selected-workspace-up",
                        None,
                    );
                }
            ));

        self.imp()
            .move_selected_workspace_down_button
            .get()
            .connect_clicked(clone!(
                #[weak(rename_to=workspacesbar)]
                self,
                move |_| {
                    adw::prelude::ActionGroupExt::activate_action(
                        &workspacesbar.action_group(),
                        "move-selected-workspace-down",
                        None,
                    );
                }
            ));

        self.imp()
            .add_workspace_button
            .get()
            .connect_clicked(clone!(
                #[weak(rename_to=workspacesbar)]
                self,
                move |_| {
                    adw::prelude::ActionGroupExt::activate_action(
                        &workspacesbar.action_group(),
                        "add-workspace",
                        None,
                    );
                }
            ));

        self.imp()
            .remove_selected_workspace_button
            .get()
            .connect_clicked(clone!(
                #[weak(rename_to=workspacesbar)]
                self,
                move |_| {
                    adw::prelude::ActionGroupExt::activate_action(
                        &workspacesbar.action_group(),
                        "remove-selected-workspace",
                        None,
                    );
                }
            ));

        self.imp()
            .edit_selected_workspace_button
            .get()
            .connect_clicked(clone!(
                #[weak(rename_to=workspacesbar)]
                self,
                move |_| {
                    adw::prelude::ActionGroupExt::activate_action(
                        &workspacesbar.action_group(),
                        "edit-selected-workspace",
                        None,
                    );
                }
            ));

        // Add initial entry
        self.insert_workspace_entry(0, RnWorkspaceListEntry::default());
    }

    fn setup_actions(&self, appwindow: &RnAppWindow) {
        let imp = self.imp();

        let action_move_selected_workspace_up =
            gio::SimpleAction::new("move-selected-workspace-up", None);
        imp.action_group
            .add_action(&action_move_selected_workspace_up);
        let action_move_selected_workspace_down =
            gio::SimpleAction::new("move-selected-workspace-down", None);
        imp.action_group
            .add_action(&action_move_selected_workspace_down);
        let action_add_workspace = gio::SimpleAction::new("add-workspace", None);
        imp.action_group.add_action(&action_add_workspace);
        let action_remove_selected_workspace =
            gio::SimpleAction::new("remove-selected-workspace", None);
        imp.action_group
            .add_action(&action_remove_selected_workspace);
        let action_edit_selected_workspace =
            gio::SimpleAction::new("edit-selected-workspace", None);
        imp.action_group.add_action(&action_edit_selected_workspace);

        // Move selected workspace up
        action_move_selected_workspace_up.connect_activate(clone!(
            #[weak(rename_to=workspacesbar)]
            self,
            move |_, _| {
                workspacesbar.move_selected_workspace_up();
            }
        ));

        // Move selected workspace down
        action_move_selected_workspace_down.connect_activate(clone!(
            #[weak(rename_to=workspacesbar)]
            self,
            move |_, _| {
                workspacesbar.move_selected_workspace_down();
            }
        ));

        // Add workspace
        action_add_workspace.connect_activate(clone!(
            #[weak(rename_to=workspacesbar)]
            self,
            #[weak]
            appwindow,
            move |_, _| {
                glib::spawn_future_local(clone!(
                    #[weak]
                    workspacesbar,
                    #[weak]
                    appwindow,
                    async move {
                        // The new workspace starts as a copy of the selected entry
                        let entry = RnWorkspaceListEntry::default();
                        if let Some(selected) = workspacesbar.selected_workspacelistentry() {
                            entry.replace_data(&selected);
                        }
                        workspacesbar.push_workspace(entry);

                        // Popup the edit dialog after creation
                        dialogs::dialog_edit_selected_workspace(&appwindow).await;
                    }
                ));
            }
        ));

        // Remove selected workspace
        action_remove_selected_workspace.connect_activate(clone!(
            #[weak(rename_to=workspacesbar)]
            self,
            move |_, _| {
                workspacesbar.remove_selected_workspace();
            }
        ));

        // Edit selected workspace
        action_edit_selected_workspace.connect_activate(clone!(
            #[weak]
            appwindow,
            move |_, _| {
                glib::spawn_future_local(clone!(
                    #[weak]
                    appwindow,
                    async move {
                        dialogs::dialog_edit_selected_workspace(&appwindow).await;
                    }
                ));
            }
        ));
    }
}
