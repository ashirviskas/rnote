// Modules
pub mod chrono_comp;
pub mod keytree;
pub mod render_comp;
pub mod selection_comp;
pub mod stroke_comp;
pub mod trash_comp;

// Re-exports
pub use chrono_comp::{ChronoComponent, StrokeId};
use keytree::KeyTree;
pub use render_comp::RenderComponent;
pub use selection_comp::SelectionComponent;
pub use trash_comp::TrashComponent;

// Imports
use self::chrono_comp::StrokeLayer;
use crate::WidgetFlags;
use crate::engine::EngineSnapshot;
use crate::notefolder::NewStroke;
use crate::strokes::Stroke;
use rnote_compose::shapes::Shapeable;
use serde::{Deserialize, Serialize};
use slotmap::{SecondaryMap, SlotMap};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;
use tracing::debug;

slotmap::new_key_type! {
    pub struct StrokeKey;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename = "history_entry")]
pub struct HistoryEntry {
    #[serde(rename = "stroke_components")]
    pub stroke_components: Arc<SlotMap<StrokeKey, Arc<Stroke>>>,
    #[serde(rename = "trash_components")]
    pub trash_components: Arc<SecondaryMap<StrokeKey, Arc<TrashComponent>>>,
    #[serde(rename = "chrono_components")]
    pub chrono_components: Arc<SecondaryMap<StrokeKey, Arc<ChronoComponent>>>,
    #[serde(rename = "chrono_counter")]
    pub chrono_counter: u32,
}

/// The keys of the strokes that changed when the news of a note were applied.
#[derive(Debug, Clone, Default)]
pub(crate) struct NewsKeys {
    pub(crate) removed: Vec<StrokeKey>,
    pub(crate) inserted: Vec<StrokeKey>,
}

impl HistoryEntry {
    /// Puts in the strokes other devices added or changed and takes out the ones they removed, found by their id.
    /// A changed stroke is taken out and put in again.
    fn apply_note_news(&mut self, strokes: &[NewStroke], removed: &[StrokeId]) -> NewsKeys {
        let keys_by_id = self
            .chrono_components
            .iter()
            .map(|(key, chrono)| (chrono.id, key))
            .collect::<HashMap<StrokeId, StrokeKey>>();
        let mut keys = NewsKeys::default();
        let ids = strokes
            .iter()
            .map(|new| new.chrono.id)
            .chain(removed.iter().copied());
        for key in ids.filter_map(|id| keys_by_id.get(&id).copied()) {
            Arc::make_mut(&mut self.stroke_components).remove(key);
            Arc::make_mut(&mut self.trash_components).remove(key);
            Arc::make_mut(&mut self.chrono_components).remove(key);
            keys.removed.push(key);
        }
        for new in strokes {
            let key = Arc::make_mut(&mut self.stroke_components).insert(Arc::clone(&new.stroke));
            Arc::make_mut(&mut self.trash_components)
                .insert(key, Arc::new(TrashComponent::default()));
            Arc::make_mut(&mut self.chrono_components).insert(key, Arc::new(new.chrono));
            self.chrono_counter = self.chrono_counter.max(new.chrono.t());
            keys.inserted.push(key);
        }
        keys
    }
}

impl Default for HistoryEntry {
    fn default() -> Self {
        Self {
            stroke_components: Arc::new(SlotMap::with_key()),
            trash_components: Arc::new(SecondaryMap::new()),
            chrono_components: Arc::new(SecondaryMap::new()),

            chrono_counter: 0,
        }
    }
}

/// StrokeStore implements a Entity - Component - System pattern.
/// The Entities are the StrokeKey's, which represent a stroke. There are different components for them:
///     * 'stroke_components': Holds state about geometric properties. These components are special in the way that they are the primary map.
///         A new stroke must have this component. (another name for them could be 'geometric_components')
///     * 'trash_components': Holds state whether the strokes are trashed
///     * 'selection_components': Holds state whether the strokes are selected
///     * 'chrono_components': Holds state about the chronological ordering
///     * 'render_components': Holds state about the rendering.
///
/// The systems are implemented as methods on StrokesStore, loosely categorized to the different components (but often modify others as well).
/// Most systems take a key or a slice of keys, and iterate with them over the different components.
/// There also is a different category of methods which return filtered keys.
#[derive(Debug, Serialize, Deserialize)]
#[serde(default, rename = "stroke_store")]
pub struct StrokeStore {
    // Components
    #[serde(rename = "stroke_components")]
    stroke_components: Arc<SlotMap<StrokeKey, Arc<Stroke>>>,
    #[serde(rename = "trash_components")]
    trash_components: Arc<SecondaryMap<StrokeKey, Arc<TrashComponent>>>,
    #[serde(rename = "selection_components")]
    selection_components: Arc<SecondaryMap<StrokeKey, Arc<SelectionComponent>>>,
    #[serde(rename = "chrono_components")]
    chrono_components: Arc<SecondaryMap<StrokeKey, Arc<ChronoComponent>>>,
    /// Incrementing counter for chrono_components.
    ///
    /// Value must be kept equal to the [ChronoComponent] of the newest inserted or modified stroke.
    #[serde(rename = "chrono_counter")]
    chrono_counter: u32,
    #[serde(skip)]
    render_components: SecondaryMap<StrokeKey, RenderComponent>,
    #[serde(skip)]
    history: VecDeque<HistoryEntry>,
    /// The index of the current live document in the history stack.
    #[serde(skip)]
    live_index: usize,
    /// An rtree backed by the slotmap store, for faster spatial queries.
    ///
    /// Needs to be updated with `update_with_key()` when strokes changed their geometry or position!
    #[serde(skip)]
    key_tree: KeyTree,
}

impl Default for StrokeStore {
    fn default() -> Self {
        Self {
            stroke_components: Arc::new(SlotMap::with_key()),
            trash_components: Arc::new(SecondaryMap::new()),
            selection_components: Arc::new(SecondaryMap::new()),
            chrono_components: Arc::new(SecondaryMap::new()),
            render_components: SecondaryMap::new(),

            // Start off with state in the history
            history: VecDeque::from(vec![HistoryEntry::default()]),
            live_index: 0,

            key_tree: KeyTree::default(),

            chrono_counter: 0,
        }
    }
}

impl StrokeStore {
    /// Max length of the history.
    pub(crate) const HISTORY_MAX_LEN: usize = 100;

    /// Import from a engine snapshot. A loaded strokes store should always be imported with this method.
    ///
    /// The store then needs to update its rendering.
    pub(crate) fn import_from_snapshot(&mut self, snapshot: &EngineSnapshot) -> WidgetFlags {
        let mut widget_flags = WidgetFlags::default();

        widget_flags |= self.clear();
        self.stroke_components = Arc::clone(&snapshot.stroke_components);
        self.chrono_components = Arc::clone(&snapshot.chrono_components);
        self.chrono_counter = snapshot.chrono_counter;

        self.update_geometry_for_strokes(&self.keys_unordered());
        self.rebuild_selection_components_slotmap();
        self.rebuild_trash_components_slotmap();
        self.rebuild_render_components_slotmap();
        self.rebuild_rtree();
        widget_flags |= self.clear_history(self.create_history_entry());
        widget_flags
    }

    /// Rebuild the rtree with the current stored strokes keys and bounds.
    fn rebuild_rtree(&mut self) {
        let tree_objects = self
            .stroke_components
            .iter()
            .map(|(key, stroke)| (key, stroke.bounds()))
            .collect();
        self.key_tree.rebuild_from_vec(tree_objects);
    }

    /// Checks the equality of current state to all fields of the given history entry,
    /// doing pointer compares when they are wrapped inside Arc's.
    fn eq_w_history_entry(&self, history_entry: &HistoryEntry) -> bool {
        Arc::ptr_eq(&self.stroke_components, &history_entry.stroke_components)
            && Arc::ptr_eq(&self.trash_components, &history_entry.trash_components)
            && Arc::ptr_eq(&self.chrono_components, &history_entry.chrono_components)
            && self.chrono_counter == history_entry.chrono_counter
    }

    /// Create a history entry from the current state.
    pub(crate) fn create_history_entry(&self) -> HistoryEntry {
        HistoryEntry {
            stroke_components: Arc::clone(&self.stroke_components),
            trash_components: Arc::clone(&self.trash_components),
            chrono_components: Arc::clone(&self.chrono_components),
            chrono_counter: self.chrono_counter,
        }
    }

    /// Import the given history entry and replaces the current state with it.
    fn import_history_entry(&mut self, history_entry: HistoryEntry) {
        self.stroke_components = Arc::clone(&history_entry.stroke_components);
        self.trash_components = Arc::clone(&history_entry.trash_components);
        self.chrono_components = Arc::clone(&history_entry.chrono_components);
        self.chrono_counter = history_entry.chrono_counter;

        // Since we don't store the rtree in the history, we need to rebuild it.
        self.rebuild_rtree();
        self.rebuild_selection_components_slotmap();
        // Rebuild but retain the render components for the strokes that are found in the history entry.
        // This ensures that we are able to continue displaying the strokes after undo/redo while they are rerendered.
        self.rebuild_retain_valid_keys_render_components();

        let all_strokes = self.stroke_keys_unordered();
        self.set_rendering_dirty_for_strokes(&all_strokes);
    }

    /// Record the current state and save it in the history.
    pub(crate) fn record(&mut self, _now: Instant) -> WidgetFlags {
        let mut widget_flags = WidgetFlags::default();

        if self
            .history
            .back()
            .map(|last| !self.eq_w_history_entry(last))
            .unwrap_or(true)
        {
            // as soon as the current state is recorded, remove the future
            self.history.truncate(self.live_index + 1);

            let current = self.create_history_entry();
            self.history.push_back(current);
            self.live_index += 1;

            // truncate history if necessary
            while self.history.len() > Self::HISTORY_MAX_LEN {
                self.history.pop_front();
                self.live_index -= 1;
            }
        } else {
            debug!("State has not changed, no need to record.");
        }

        widget_flags.hide_undo = Some(!self.can_undo());
        widget_flags.hide_redo = Some(!self.can_redo());

        widget_flags
    }

    /// Update the state of the latest history entry with the current document state.
    pub(crate) fn update_latest_history_entry(&mut self, _now: Instant) -> WidgetFlags {
        let mut widget_flags = WidgetFlags::default();

        if self
            .history
            .back()
            .map(|last| !self.eq_w_history_entry(last))
            .unwrap_or(true)
        {
            // as soon as the current state is recorded, remove the future
            self.history.truncate(self.live_index + 1);

            let current = self.create_history_entry();
            self.history[self.live_index] = current;
        } else {
            debug!("State has not changed, no need to update history with current state.");
        }

        widget_flags.hide_undo = Some(!self.can_undo());
        widget_flags.hide_redo = Some(!self.can_redo());

        widget_flags
    }

    /// Undo the latest changes.
    ///
    /// Should only be called from inside the engine undo wrapper function.
    pub(crate) fn undo(&mut self, _now: Instant) -> WidgetFlags {
        let mut widget_flags = WidgetFlags::default();

        if !self.can_undo() {
            return widget_flags;
        }

        let prev = self.history[self.live_index - 1].clone();
        self.import_history_entry(prev);
        self.live_index -= 1;

        widget_flags.hide_undo = Some(!self.can_undo());
        widget_flags.hide_redo = Some(!self.can_redo());
        widget_flags.store_modified = true;

        widget_flags
    }

    /// Redo the latest changes.
    ///
    /// Should only be called from inside the engine redo wrapper function.
    pub(crate) fn redo(&mut self, _now: Instant) -> WidgetFlags {
        let mut widget_flags = WidgetFlags::default();

        if !self.can_redo() {
            return widget_flags;
        }

        let next = self.history[self.live_index + 1].clone();
        self.import_history_entry(next);
        self.live_index += 1;

        widget_flags.hide_undo = Some(!self.can_undo());
        widget_flags.hide_redo = Some(!self.can_redo());
        widget_flags.store_modified = true;

        widget_flags
    }

    pub(crate) fn can_undo(&self) -> bool {
        self.live_index > 0
    }

    pub(crate) fn can_redo(&self) -> bool {
        self.live_index < self.history.len() - 1
    }

    /// Clear the history.
    pub(crate) fn clear_history(&mut self, initial_state: HistoryEntry) -> WidgetFlags {
        let mut widget_flags = WidgetFlags::default();

        self.history = VecDeque::from(vec![initial_state]);
        self.live_index = 0;

        widget_flags.hide_undo = Some(true);
        widget_flags.hide_redo = Some(true);

        widget_flags
    }

    /// Brings in what other devices changed in the note: the strokes they added or changed, without the ones they
    /// removed. Returns the keys of the strokes that were put in.
    ///
    /// The changes are no change of the user, so they are not recorded. They are applied to every state in the
    /// history instead, so that undo and redo only undo and redo what the user did.
    pub(crate) fn apply_note_news(
        &mut self,
        strokes: &[NewStroke],
        removed: &[StrokeId],
    ) -> Vec<StrokeKey> {
        let live_is_recorded = self
            .history
            .get(self.live_index)
            .is_some_and(|entry| self.eq_w_history_entry(entry));
        let mut live_keys = None;
        for (i, entry) in self.history.iter_mut().enumerate() {
            let keys = entry.apply_note_news(strokes, removed);
            if live_is_recorded && i == self.live_index {
                live_keys = Some(keys);
            }
        }
        let keys = match live_keys {
            // The live state is the one in the history, and stays the same allocation
            Some(keys) => {
                let entry = self.history[self.live_index].clone();
                self.stroke_components = entry.stroke_components;
                self.trash_components = entry.trash_components;
                self.chrono_components = entry.chrono_components;
                self.chrono_counter = entry.chrono_counter;
                keys
            }
            None => {
                let mut live = self.create_history_entry();
                let keys = live.apply_note_news(strokes, removed);
                self.stroke_components = live.stroke_components;
                self.trash_components = live.trash_components;
                self.chrono_components = live.chrono_components;
                self.chrono_counter = live.chrono_counter;
                keys
            }
        };

        // The components that are not part of the history
        for &key in &keys.removed {
            Arc::make_mut(&mut self.selection_components).remove(key);
            self.render_components.remove(key);
            self.key_tree.remove_with_key(key);
        }
        for &key in &keys.inserted {
            Arc::make_mut(&mut self.selection_components)
                .insert(key, Arc::new(SelectionComponent::default()));
            self.render_components
                .insert(key, RenderComponent::default());
            if let Some(stroke) = self.stroke_components.get(key) {
                self.key_tree.insert_with_key(key, stroke.bounds());
            }
        }
        self.set_rendering_dirty_for_strokes(&keys.inserted);
        keys.inserted
    }

    /// Insert a new stroke into the store.
    ///
    /// Optionally a desired layer can be specified, or the default stroke layer is used.
    ///
    /// The stroke then needs to update its rendering.
    pub(crate) fn insert_stroke(
        &mut self,
        stroke: Stroke,
        layer: Option<StrokeLayer>,
    ) -> StrokeKey {
        let bounds = stroke.bounds();
        let layer = layer.unwrap_or_else(|| stroke.extract_default_layer());

        let key = Arc::make_mut(&mut self.stroke_components).insert(Arc::new(stroke));
        self.key_tree.insert_with_key(key, bounds);
        self.chrono_counter += 1;

        Arc::make_mut(&mut self.trash_components).insert(key, Arc::new(TrashComponent::default()));
        Arc::make_mut(&mut self.selection_components)
            .insert(key, Arc::new(SelectionComponent::default()));
        Arc::make_mut(&mut self.chrono_components).insert(
            key,
            Arc::new(ChronoComponent::new(self.chrono_counter, layer)),
        );
        self.render_components
            .insert(key, RenderComponent::default());

        key
    }

    /// Permanently remove a stroke with the given key from the store.
    #[allow(unused)]
    pub(crate) fn remove_stroke(&mut self, key: StrokeKey) -> Option<Stroke> {
        Arc::make_mut(&mut self.trash_components).remove(key);
        Arc::make_mut(&mut self.selection_components).remove(key);
        Arc::make_mut(&mut self.chrono_components).remove(key);
        self.render_components.remove(key);

        self.key_tree.remove_with_key(key);
        Arc::make_mut(&mut self.stroke_components)
            .remove(key)
            .map(|stroke| (*stroke).clone())
    }

    /// Clears the entire store.
    pub(super) fn clear(&mut self) -> WidgetFlags {
        Arc::make_mut(&mut self.stroke_components).clear();
        Arc::make_mut(&mut self.trash_components).clear();
        Arc::make_mut(&mut self.selection_components).clear();
        Arc::make_mut(&mut self.chrono_components).clear();

        self.chrono_counter = 0;
        let widget_flags = self.clear_history(HistoryEntry::default());

        self.render_components.clear();
        self.key_tree.clear();

        widget_flags
    }
}
