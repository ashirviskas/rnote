// Imports
use super::RnCanvas;
use crate::RnAppWindow;
use futures::channel::oneshot;
use gtk4::{gio, prelude::*, subclass::prelude::*};
use p2d::math::Vector2;
use rnote_compose::ext::Vector2Ext;
use rnote_engine::WidgetFlags;
use rnote_engine::engine::export::{DocExportPrefs, DocPagesExportPrefs, SelectionExportPrefs};
use rnote_engine::engine::{EngineSnapshot, StrokeContent};
use rnote_engine::notefolder::{Device, NoteFolder};
use rnote_engine::strokes::Stroke;
use rnote_engine::strokes::resize::ImageSizeOption;
use std::ops::Range;
use std::path::{Path, PathBuf};
use tracing::{debug, error};

impl RnCanvas {
    /// Load the bytes of a `.rnote` file and imports it into the engine.
    ///
    /// `file_path` is optional but needs to be supplied when the origin file should be tracked.
    ///
    /// The function returns `WidgetFlags` instead of emitting the `handle_signal_flags` signal, because a signal
    /// handler might not yet be connected when this function is called.
    pub(crate) async fn load_in_rnote_bytes<P>(
        &self,
        bytes: Vec<u8>,
        file_path: Option<P>,
    ) -> anyhow::Result<WidgetFlags>
    where
        P: AsRef<Path>,
    {
        let engine_snapshot = EngineSnapshot::load_from_rnote_bytes(bytes).await?;
        let file_path = file_path.map(|path| path.as_ref().to_path_buf());
        Ok(self.load_in_engine_snapshot(engine_snapshot, file_path, None))
    }

    /// Loads a note folder and imports it into the engine. The folder is tidied on the way.
    ///
    /// See [Self::load_in_rnote_bytes] for why the function returns `WidgetFlags`.
    pub(crate) async fn load_in_note_folder(&self, dir: PathBuf) -> anyhow::Result<WidgetFlags> {
        let device = Device::this()?;
        let load = blocking::unblock({
            let dir = dir.clone();
            move || {
                let (mut note_folder, engine_snapshot) = NoteFolder::load(&dir, device)?;
                // What is tidied away was no part of the note any more, so the snapshot stays right
                if let Err(e) = note_folder.tidy() {
                    error!("Tidying the note folder {dir:?} failed, Err: {e:?}");
                }
                anyhow::Ok((note_folder, engine_snapshot))
            }
        });
        let (note_folder, engine_snapshot) = load.await?;
        Ok(self.load_in_engine_snapshot(engine_snapshot, Some(dir), Some(note_folder)))
    }

    /// Imports the snapshot of a note that was loaded from the path into the engine.
    fn load_in_engine_snapshot(
        &self,
        engine_snapshot: EngineSnapshot,
        path: Option<PathBuf>,
        note_folder: Option<NoteFolder>,
    ) -> WidgetFlags {
        let mut widget_flags = self.engine_mut().load_snapshot(engine_snapshot);
        widget_flags |= self
            .engine_mut()
            .set_scale_factor(self.scale_factor() as f64);

        self.imp().note_folder.replace(note_folder);
        self.set_output_file(path.map(gio::File::for_path));
        self.dismiss_output_file_modified_toast();
        self.set_unsaved_changes(false);
        self.set_empty(false);
        if let Some(output_filepath) = self.output_file().and_then(|f| f.path()) {
            crate::indexer::queue(output_filepath);
        }

        widget_flags
    }

    /// Reload the engine from the file that is set as origin file.
    ///
    /// If the origin file is set to None, this does nothing and returns an error.
    pub(crate) async fn reload_from_disk(&self) -> anyhow::Result<()> {
        let Some(output_file) = self.output_file() else {
            return Err(anyhow::anyhow!(
                "Failed to reload file from disk, no file path saved."
            ));
        };
        let note_folder = output_file
            .path()
            .and_then(|path| NoteFolder::folder_of(&path));
        let widget_flags = match note_folder {
            Some(dir) => self.load_in_note_folder(dir).await?,
            None => {
                let (bytes, _) = output_file.load_bytes_future().await?;
                self.load_in_rnote_bytes(bytes.to_vec(), output_file.path())
                    .await?
            }
        };
        self.emit_handle_widget_flags(widget_flags);
        Ok(())
    }

    pub(crate) async fn load_in_xopp_bytes(
        &self,
        appwindow: &RnAppWindow,
        bytes: Vec<u8>,
    ) -> anyhow::Result<()> {
        let xopp_import_prefs = appwindow
            .engine_config()
            .read()
            .import_prefs
            .xopp_import_prefs;
        let engine_snapshot =
            EngineSnapshot::load_from_xopp_bytes(bytes, xopp_import_prefs).await?;
        let widget_flags = self.engine_mut().load_snapshot(engine_snapshot);
        self.emit_handle_widget_flags(widget_flags);

        self.set_output_file(None);
        self.set_unsaved_changes(true);
        self.set_empty(false);
        Ok(())
    }

    /// Loads in bytes from a vector image and imports it.
    ///
    /// `target_pos` is in coordinate space of the doc.
    pub(crate) async fn load_in_vectorimage_bytes(
        &self,
        bytes: Vec<u8>,
        target_pos: Option<Vector2>,
        respect_borders: bool,
    ) -> anyhow::Result<()> {
        let pos = self.determine_stroke_import_pos(target_pos);

        // Splitting the import operation into two parts: a receiver that gets awaited with the content, and
        // the blocking import avoids borrowing the entire engine RefCell while awaiting the content, avoiding panics.
        let vectorimage_receiver =
            self.engine_mut()
                .generate_vectorimage_from_bytes(pos, bytes, respect_borders);
        let vectorimage = vectorimage_receiver.await??;
        let widget_flags = self
            .engine_mut()
            .import_generated_content(vec![(Stroke::VectorImage(vectorimage), None)], false);

        self.emit_handle_widget_flags(widget_flags);
        Ok(())
    }

    /// Loads in bytes from a bitmap image and imports it.
    ///
    /// `target_pos` is in coordinate space of the doc.
    pub(crate) async fn load_in_bitmapimage_bytes(
        &self,
        bytes: Vec<u8>,
        target_pos: Option<Vector2>,
        respect_borders: bool,
    ) -> anyhow::Result<()> {
        let pos = self.determine_stroke_import_pos(target_pos);

        let bitmapimage_receiver =
            self.engine_mut()
                .generate_bitmapimage_from_bytes(pos, bytes, respect_borders);
        let bitmapimage = bitmapimage_receiver.await??;
        let widget_flags = self
            .engine_mut()
            .import_generated_content(vec![(Stroke::BitmapImage(bitmapimage), None)], false);

        self.emit_handle_widget_flags(widget_flags);
        Ok(())
    }

    /// Loads in bytes from a pdf and imports it.
    ///
    /// `target_pos` is in coordinate space of the doc.
    pub(crate) async fn load_in_pdf_bytes(
        &self,
        appwindow: &RnAppWindow,
        bytes: Vec<u8>,
        target_pos: Option<Vector2>,
        page_range: Option<Range<usize>>,
        password: Option<String>,
    ) -> anyhow::Result<()> {
        let pos = self.determine_stroke_import_pos(target_pos);
        let adjust_document = appwindow
            .engine_config()
            .read()
            .import_prefs
            .pdf_import_prefs
            .adjust_document;

        let strokes_receiver = self
            .engine_mut()
            .generate_pdf_pages_from_bytes(bytes, pos, page_range, password);
        let strokes = strokes_receiver.await??;
        let widget_flags = self
            .engine_mut()
            .import_generated_content(strokes, adjust_document);

        self.emit_handle_widget_flags(widget_flags);
        Ok(())
    }

    /// Imports a text.
    ///
    /// `target_pos` is in coordinate space of the doc.
    pub(crate) fn load_in_text(
        &self,
        text: String,
        target_pos: Option<Vector2>,
    ) -> anyhow::Result<()> {
        let pos = self.determine_stroke_import_pos(target_pos);

        let widget_flags = self.engine_mut().insert_text(text, Some(pos));

        self.emit_handle_widget_flags(widget_flags);
        Ok(())
    }

    /// Deserializes the stroke content and inserts it into the engine.
    ///
    /// The data is usually coming from the clipboard, drop source, etc.
    pub(crate) async fn insert_stroke_content(
        &self,
        json_string: String,
        resize_option: ImageSizeOption,
        target_pos: Option<Vector2>,
    ) -> anyhow::Result<()> {
        let (oneshot_sender, oneshot_receiver) =
            oneshot::channel::<anyhow::Result<StrokeContent>>();
        let pos = self.determine_stroke_import_pos(target_pos);

        rayon::spawn(move || {
            let result = || -> Result<StrokeContent, anyhow::Error> {
                Ok(serde_json::from_str(&json_string)?)
            };
            if oneshot_sender.send(result()).is_err() {
                error!(
                    "Sending result to receiver while inserting stroke content failed. Receiver already dropped."
                );
            }
        });
        let content = oneshot_receiver.await??;
        let widget_flags = self
            .engine_mut()
            .insert_stroke_content(content, pos, resize_option);

        self.emit_handle_widget_flags(widget_flags);
        Ok(())
    }

    /// Saves the document to the given file.
    ///
    /// Returns:
    /// - `Ok(true)` if saving was successful
    /// - `Ok(false)` if a save was already in progress (and thus this function didn't do anything)
    /// - `Err(e)` when saving failed in any way
    #[tracing::instrument(skip_all, fields(path = format!("{:?}", file.path())))]
    pub(crate) async fn save_document_to_file(&self, file: &gio::File) -> anyhow::Result<bool> {
        // skip saving when it is already in progress
        if self.save_in_progress() {
            debug!("Returning early, saving file is already in progress");
            return Ok(false);
        }
        self.set_save_in_progress(true);
        debug!("Saving file is now in progress");

        let filepath = file.path().ok_or_else(|| {
            self.set_save_in_progress(false);
            anyhow::anyhow!("Could not get a path for file: `{file:?}`.")
        })?;
        let basename = file.basename().ok_or_else(|| {
            self.set_save_in_progress(false);
            anyhow::anyhow!("Could not retrieve basename for file: `{file:?}`.")
        })?;
        // A note folder that exists, or the name of one that is to be made
        let to_note_folder = NoteFolder::folder_of(&filepath).is_some()
            || (!filepath.exists()
                && filepath
                    .extension()
                    .is_some_and(|ext| ext == NoteFolder::EXTENSION));

        let mut skip_set_output_file = false;
        if let Some(output_filepath) = self.output_file().and_then(|f| f.path())
            && crate::utils::paths_abs_eq(output_filepath, &filepath).unwrap_or(false)
        {
            skip_set_output_file = true;
        }

        self.dismiss_output_file_modified_toast();

        let file_write_operation = async {
            if to_note_folder {
                return self.save_to_note_folder(&filepath).await;
            }
            let rnote_bytes_receiver = self
                .engine_ref()
                .save_as_rnote_bytes(basename.to_string_lossy().to_string());
            let bytes = rnote_bytes_receiver.await??;
            // The `output_file_expect_write` should theoretically be reset to `false` by the file watcher later.
            self.set_output_file_expect_write(true);
            crate::utils::atomic_save_to_file_future(&filepath, bytes).await?;
            self.imp().note_folder.take();
            Ok(())
        };

        if let Err(e) = file_write_operation.await {
            self.set_save_in_progress(false);
            // If the file operations failed in any way, we make sure to clear the expect_write flag
            // because we can't know for sure if the output-file watcher will be able to.
            self.set_output_file_expect_write(false);
            return Err(e);
        }

        debug!("Saving file has finished successfully");

        if !skip_set_output_file {
            // We only create/replace the file watcher once we are sure the file was successfully saved.
            self.set_output_file(Some(file.to_owned()));
            // Required, otherwise `output_file_expect_write` will be stuck on true after saving for the
            // first time or saving as another filename, until the subsequent save at least.
            self.set_output_file_expect_write(false);
        }
        self.set_unsaved_changes(false);
        self.set_save_in_progress(false);
        crate::indexer::queue(filepath);

        Ok(true)
    }

    /// Saves what changed in the document as a new batch of the note folder.
    ///
    /// A note folder that is not there yet is made. One that is there but not the one the document is open
    /// from gets the document as its new state.
    async fn save_to_note_folder(&self, dir: &Path) -> anyhow::Result<()> {
        let engine_snapshot = self.engine_ref().take_snapshot();
        let device = Device::this()?;
        let exists = NoteFolder::folder_of(dir).is_some();
        let open = self.imp().note_folder.take().filter(|note_folder| {
            exists && crate::utils::paths_abs_eq(note_folder.dir(), dir).unwrap_or(false)
        });

        let dir = dir.to_path_buf();
        let save = blocking::unblock(move || {
            let note_folder = match open {
                Some(note_folder) => Ok(note_folder),
                None if exists => {
                    NoteFolder::load(&dir, device).map(|(note_folder, _)| note_folder)
                }
                None => NoteFolder::create(&dir, device),
            };
            let mut note_folder = match note_folder {
                Ok(note_folder) => note_folder,
                Err(e) => return (None, Err(e)),
            };
            let saved = note_folder.save(&engine_snapshot).map(|_| ());
            (Some(note_folder), saved)
        });
        let (note_folder, saved) = save.await;
        self.imp().note_folder.replace(note_folder);
        saved
    }

    pub(crate) async fn export_doc(
        &self,
        file: &gio::File,
        title: String,
        export_prefs_override: Option<DocExportPrefs>,
    ) -> anyhow::Result<()> {
        let export_bytes = self.engine_ref().export_doc(title, export_prefs_override);

        crate::utils::create_replace_file_future(export_bytes.await??, file).await?;

        self.set_last_export_dir(file.parent());

        Ok(())
    }

    /// Exports document pages
    /// `file_stem_name`: the stem name of the created files. This is extended by an enumeration of the page number and
    /// file extension overwrites existing files with the same name!
    pub(crate) async fn export_doc_pages(
        &self,
        appwindow: &RnAppWindow,
        dir: &gio::File,
        file_stem_name: String,
        export_prefs_override: Option<DocPagesExportPrefs>,
    ) -> anyhow::Result<()> {
        if dir.query_file_type(gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
            != gio::FileType::Directory
        {
            return Err(anyhow::anyhow!(
                "Supplied target file `{dir:?}` is not a directory."
            ));
        }
        let export_prefs = export_prefs_override.unwrap_or(
            appwindow
                .engine_config()
                .read()
                .export_prefs
                .doc_pages_export_prefs,
        );
        let file_ext = export_prefs.export_format.file_ext();

        let export_bytes_recv = self.engine_ref().export_doc_pages(export_prefs_override);
        let export_bytes = export_bytes_recv.await??;

        for (i, page_bytes) in export_bytes.into_iter().enumerate() {
            crate::utils::create_replace_file_future(
                page_bytes,
                &dir.child(
                    &(rnote_engine::utils::doc_pages_files_names(file_stem_name.clone(), i + 1)
                        + "."
                        + &file_ext),
                ),
            )
            .await?;
        }

        self.set_last_export_dir(Some(dir.clone()));

        Ok(())
    }

    pub(crate) async fn export_selection(
        &self,
        file: &gio::File,
        export_prefs_override: Option<SelectionExportPrefs>,
    ) -> anyhow::Result<()> {
        let export_bytes = self.engine_ref().export_selection(export_prefs_override);

        if let Some(export_bytes) = export_bytes.await?? {
            crate::utils::create_replace_file_future(export_bytes, file).await?;
        }

        self.set_last_export_dir(file.parent());

        Ok(())
    }

    /// exports and writes the engine state as json into the file.
    /// Only for debugging!
    pub(crate) async fn export_engine_state(&self, file: &gio::File) -> anyhow::Result<()> {
        let exported_engine_state = self.engine_ref().export_state_as_json()?;

        crate::utils::create_replace_file_future(exported_engine_state.into_bytes(), file).await?;

        self.set_last_export_dir(file.parent());

        Ok(())
    }

    fn determine_stroke_import_pos(&self, target_pos: Option<Vector2>) -> Vector2 {
        target_pos.unwrap_or_else(|| {
            self.engine_ref()
                .camera
                .transform()
                .inverse()
                .transform_point2(Stroke::IMPORT_OFFSET_DEFAULT)
                .maxs(&Vector2::new(
                    self.engine_ref().document.x,
                    self.engine_ref().document.y,
                ))
        })
    }
}
