// SPDX-License-Identifier: GPL-3.0-only

pub mod config;
mod localize;
pub mod save_state;
pub mod storage;
pub mod ui;

use config::{ScratchpadConfig, load_config};
use cosmic::cosmic_config::CosmicConfigEntry;
use cosmic::iced::core::SmolStr;
use cosmic::iced::core::keyboard::{
    Key, Modifiers,
    key::{Named, Physical},
};
use cosmic::iced::widget::{column, row};
use cosmic::iced::{Alignment, Length, Limits, Subscription, window};
use cosmic::widget::text_editor::{Action, Binding, Content, Edit, KeyPress, Motion};
use cosmic::widget::{
    button, container, icon, scrollable, space, text, text_editor, text_input, tooltip,
};
use cosmic::{Application, Element, app, theme};
use save_state::{NoteId, SaveState};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, Ordering},
};
use std::time::{Duration, SystemTime};
use tokio::time::Instant;

static ALTGR_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Ctrl/Shift/Alt/Super state tracked from key press and release events.
///
/// When cosmic-panel leaves keyboard focus on the panel-button surface, the
/// `modifiers` field of key events delivered there goes stale: after Ctrl+F every
/// later key still reports Ctrl as held, although the produced text is correct.
/// Keys forwarded from that surface use this state instead.
static HELD_MODIFIERS: AtomicU32 = AtomicU32::new(0);

fn modifier_of(key: &Key) -> Option<Modifiers> {
    match key {
        Key::Named(Named::Control) => Some(Modifiers::CTRL),
        Key::Named(Named::Shift) => Some(Modifiers::SHIFT),
        Key::Named(Named::Alt) => Some(Modifiers::ALT),
        Key::Named(Named::Super | Named::Meta) => Some(Modifiers::LOGO),
        _ => None,
    }
}

fn track_modifier(key: &Key, pressed: bool) {
    if let Some(modifier) = modifier_of(key) {
        if pressed {
            HELD_MODIFIERS.fetch_or(modifier.bits(), Ordering::SeqCst);
        } else {
            HELD_MODIFIERS.fetch_and(!modifier.bits(), Ordering::SeqCst);
        }
    }
}

fn held_modifiers() -> Modifiers {
    Modifiers::from_bits_truncate(HELD_MODIFIERS.load(Ordering::SeqCst))
}

/// Clears tracked key state; releases that happen while the popup is closed are
/// not observed.
fn reset_key_state() {
    ALTGR_ACTIVE.store(false, Ordering::SeqCst);
    HELD_MODIFIERS.store(0, Ordering::SeqCst);
}

pub const APP_ID: &str = "io.github.hasmolam.cosmic-ext-applet-scratchpad";
const POPUP_WIDTH: f32 = 380.0;
const POPUP_HEIGHT: f32 = 440.0;
const DEBOUNCE_MILLIS: u64 = 400;
const UNDO_BANNER_SECS: u64 = 5;

const SEARCH_INPUT_ID: &str = "nv_search_input";

fn focus_search_input() -> app::Task<Message> {
    text_input::focus(cosmic::iced::core::widget::Id::new(SEARCH_INPUT_ID))
}

/// Builds editor content with the cursor at the end of the document, so freshly
/// loaded notes accept appended text without a click.
fn content_at_end(text: &str) -> Content {
    let mut content = Content::with_text(text);
    content.perform(Action::Move(Motion::DocumentEnd));
    content
}

/// AltGr is tracked through `ALTGR_ACTIVE` because cosmic-panel drops the level-3
/// modifier before keys reach the applet; Ctrl+Alt is the X11-style equivalent.
fn altgr_held(modifiers: Modifiers) -> bool {
    (ALTGR_ACTIVE.load(Ordering::SeqCst) && !modifiers.control() && !modifiers.logo())
        || (modifiers.control() && modifiers.alt() && !modifiers.logo())
}

/// Application shortcuts, shared by the editor key binding and uncaptured keys.
fn shortcut_message(key: &Key, modifiers: Modifiers) -> Option<Message> {
    if *key == Key::Named(Named::Escape) {
        return Some(Message::EscapePressed);
    }
    if !modifiers.command() || modifiers.alt() {
        return None;
    }
    match key.as_ref() {
        Key::Character("f" | "F" | "k" | "K") => Some(Message::ToggleSearch),
        Key::Character("n" | "N") => Some(Message::NewNote(None)),
        Key::Character("[") | Key::Named(Named::ArrowLeft) => Some(Message::PrevNote),
        Key::Character("]") | Key::Named(Named::ArrowRight) => Some(Message::NextNote),
        Key::Character("c" | "C") if modifiers.shift() => Some(Message::CopyAll),
        _ => None,
    }
}

/// Key binding for the note editor: AltGr symbols, app shortcuts, then the
/// default text editor bindings.
fn editor_binding(altgr_map: &ui::AltGrMap, keypress: KeyPress) -> Option<Binding<Message>> {
    if ui::is_altgr_key(&keypress.key, keypress.physical_key) {
        ALTGR_ACTIVE.store(true, Ordering::SeqCst);
        return None;
    }
    if altgr_held(keypress.modifiers)
        && let Some((_, altgr_ch)) = altgr_map.resolve_key(
            &keypress.key,
            keypress.physical_key,
            keypress.modifiers.shift(),
        )
    {
        return Some(Binding::Insert(altgr_ch));
    }
    if let Some(message) = shortcut_message(&keypress.key, keypress.modifiers) {
        return Some(Binding::Custom(message));
    }
    Binding::from_key_press(keypress)
}

/// A key press that no widget captured, with the window it was delivered to.
#[derive(Debug, Clone)]
pub struct KeyInput {
    key: Key,
    modified_key: Key,
    physical_key: Physical,
    modifiers: Modifiers,
    text: Option<SmolStr>,
}

pub fn run() -> cosmic::iced::Result {
    localize::localize();
    cosmic::applet::run::<ScratchpadApp>(())
}

pub struct NoteItem {
    pub id: NoteId,
    pub path: PathBuf,
    pub filename: String,
    pub title: String,
    pub content: Content,
    pub word_chars: (usize, usize),
    pub last_edit_time: Option<Instant>,
    pub last_mtime: Option<SystemTime>,
    pub save: SaveState,
}

impl NoteItem {
    /// Builds a note whose in-memory text matches what is on disk at `path`.
    fn from_text(id: NoteId, path: PathBuf, text: &str, fallback_title: &str) -> Self {
        let filename = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let last_mtime = storage::note_mtime(&path);
        Self {
            id,
            path,
            filename,
            title: storage::derive_title(text, fallback_title),
            content: content_at_end(text),
            word_chars: ui::count_words_and_chars(text),
            last_edit_time: None,
            last_mtime,
            save: SaveState::default(),
        }
    }
}

/// A delete that waits for an in-flight save of the same note to finish, so the
/// write cannot recreate the file after it was moved to the trash.
struct PendingDelete {
    id: NoteId,
    path: PathBuf,
    save: SaveState,
}

/// Result of a background note write.
#[derive(Debug, Clone, Copy)]
pub enum SaveOutcome {
    Saved(Option<SystemTime>),
    Failed,
}

#[derive(Debug, Clone)]
pub enum Message {
    TogglePopup,
    CloseRequested(window::Id),
    PrevNote,
    NextNote,
    NewNote(Option<String>),
    SelectNote(usize),
    DeleteActiveNote,
    UndoDelete,
    DismissUndoBanner(u64),
    ToggleSearch,
    SearchInputChanged(String),
    SubmitSearch,
    EditorAction(Action),
    DebounceTimeout(NoteId),
    NoteSaved(NoteId, u64, SaveOutcome),
    CopyAll,
    ResetCopyStatus(u64),
    ToggleSettings,
    ToggleWrapLines,
    AdjustFontSize(f32),
    EscapePressed,
    KeyInput(window::Id, KeyInput),
    Surface(cosmic::surface::Action<Message>),
    PopupFocusGained(window::Id),
}

pub struct ScratchpadApp {
    core: cosmic::app::Core,
    config: ScratchpadConfig,
    config_handler: Option<cosmic::cosmic_config::Config>,
    popup: Option<window::Id>,
    notes: Vec<NoteItem>,
    next_note_id: NoteId,
    pending_deletes: Vec<PendingDelete>,
    active_index: usize,
    copied_recently: bool,
    copy_generation: u64,
    show_settings: bool,
    undo_generation: u64,
    undo_cache: Option<(u64, PathBuf, String)>,
    is_searching: bool,
    search_query: String,
    filtered_indices: Vec<usize>,
    altgr_map: Arc<ui::AltGrMap>,
    /// Must be `Id::unique()`, not a named `Id::new(..)`. libcosmic's `TextEditor`
    /// reports the inner editor's id from its context-menu wrapper as well, so a
    /// named id appears twice in the widget tree. `Tree::take_all_named` then
    /// overwrites one state with the other and the editor panics with
    /// "Downcast on stateless state" on the next rebuild.
    editor_id: cosmic::iced::core::widget::Id,
}

impl ScratchpadApp {
    fn current_text(&self, index: usize) -> String {
        self.notes
            .get(index)
            .map(|n| n.content.text())
            .unwrap_or_default()
    }

    fn update_note_meta(&mut self, index: usize) {
        if let Some(note) = self.notes.get_mut(index) {
            let text = note.content.text();
            let fallback = fl!("note-title-fallback", index = (index + 1));
            note.title = storage::derive_title(&text, &fallback);
            note.word_chars = ui::count_words_and_chars(&text);
        }
    }

    fn update_search_filter(&mut self) {
        let query = self.search_query.trim().to_lowercase();
        if query.is_empty() {
            self.filtered_indices = (0..self.notes.len()).collect();
        } else {
            self.filtered_indices = self
                .notes
                .iter()
                .enumerate()
                .filter(|(_, note)| {
                    note.title.to_lowercase().contains(&query)
                        || note.content.text().to_lowercase().contains(&query)
                })
                .map(|(idx, _)| idx)
                .collect();
        }
    }

    fn alloc_note_id(&mut self) -> NoteId {
        let id = self.next_note_id;
        self.next_note_id = self.next_note_id.wrapping_add(1);
        id
    }

    fn note_index(&self, id: NoteId) -> Option<usize> {
        self.notes.iter().position(|note| note.id == id)
    }

    /// Starts a background write of the note at `index` if it has unsaved
    /// changes. At most one write per note runs at a time; a request made while
    /// one is running is replayed from `NoteSaved` with the latest content.
    fn save_note_async(&mut self, index: usize) -> app::Task<Message> {
        let Some(note) = self.notes.get_mut(index) else {
            return app::Task::none();
        };
        let Some(revision) = note.save.begin() else {
            return app::Task::none();
        };
        note.last_edit_time = None;
        let id = note.id;
        let text = note.content.text();
        let path = note.path.clone();
        app::Task::future(async move {
            let res = tokio::task::spawn_blocking(move || {
                storage::save_note_atomic(&path, &text).map(|()| storage::note_mtime(&path))
            })
            .await;
            let outcome = match res {
                Ok(Ok(mtime)) => SaveOutcome::Saved(mtime),
                Ok(Err(err)) => {
                    tracing::error!(?err, id, "Failed to save note atomically in background");
                    SaveOutcome::Failed
                }
                Err(err) => {
                    tracing::error!(?err, id, "Failed to join save task");
                    SaveOutcome::Failed
                }
            };
            cosmic::Action::App(Message::NoteSaved(id, revision, outcome))
        })
    }

    fn handle_note_saved(
        &mut self,
        id: NoteId,
        revision: u64,
        outcome: SaveOutcome,
    ) -> app::Task<Message> {
        let ok = matches!(outcome, SaveOutcome::Saved(_));

        if let Some(index) = self.note_index(id) {
            let note = &mut self.notes[index];
            let follow_up = note.save.finish(revision, ok);
            if let SaveOutcome::Saved(Some(mtime)) = outcome {
                note.last_mtime = Some(mtime);
            }
            if follow_up {
                return self.save_note_async(index);
            }
            return app::Task::none();
        }

        // The note was deleted while this write ran; finish the delete now that
        // no write for it is outstanding. This stays synchronous like the
        // immediate path in `DeleteActiveNote`: a background delete could run
        // after an `UndoDelete` write of the same path and remove the restored file.
        if let Some(pos) = self.pending_deletes.iter().position(|p| p.id == id) {
            let pending = self.pending_deletes.remove(pos);
            if let Err(err) = storage::delete_note(&pending.path) {
                tracing::error!(?err, path = ?pending.path, "Failed to delete note");
            }
        }
        app::Task::none()
    }

    fn select_note_internal(&mut self, target: usize) -> app::Task<Message> {
        if target < self.notes.len() && target != self.active_index {
            let prev_idx = self.active_index;
            self.active_index = target;
            self.is_searching = false;
            self.config.active_tab = target;
            self.config.active_file = Some(self.notes[target].filename.clone());
            return app::Task::batch([self.save_note_async(prev_idx), self.focus_editor()]);
        }
        if self.is_searching {
            self.is_searching = false;
        }
        self.focus_editor()
    }

    fn active_selection(&self) -> Option<String> {
        self.notes
            .get(self.active_index)
            .and_then(|note| note.content.selection())
    }

    /// Applies an editor binding outside the widget, mirroring what
    /// `TextEditor::update` does for a focused editor.
    fn apply_binding(&mut self, binding: Binding<Message>) -> app::Task<Message> {
        let action = match binding {
            Binding::Unfocus => return app::Task::none(),
            Binding::Copy => {
                return self
                    .active_selection()
                    .map_or_else(app::Task::none, cosmic::iced::clipboard::write);
            }
            Binding::Cut => {
                let Some(selection) = self.active_selection() else {
                    return app::Task::none();
                };
                let delete = self.update(Message::EditorAction(Action::Edit(Edit::Delete)));
                return app::Task::batch([cosmic::iced::clipboard::write(selection), delete]);
            }
            Binding::Paste => {
                return cosmic::iced::clipboard::read().and_then(|text| {
                    cosmic::task::message(cosmic::Action::App(Message::EditorAction(Action::Edit(
                        Edit::Paste(Arc::new(text)),
                    ))))
                });
            }
            Binding::Move(motion) => Action::Move(motion),
            Binding::Select(motion) => Action::Select(motion),
            Binding::SelectWord => Action::SelectWord,
            Binding::SelectLine => Action::SelectLine,
            Binding::SelectAll => Action::SelectAll,
            Binding::Insert(ch) => Action::Edit(Edit::Insert(ch)),
            Binding::Enter => Action::Edit(Edit::Enter),
            Binding::Backspace => Action::Edit(Edit::Backspace),
            Binding::Delete => Action::Edit(Edit::Delete),
            Binding::Sequence(bindings) => {
                let mut tasks = Vec::with_capacity(bindings.len());
                for binding in bindings {
                    tasks.push(self.apply_binding(binding));
                }
                return app::Task::batch(tasks);
            }
            Binding::Custom(message) => return self.update(message),
        };
        self.update(Message::EditorAction(action))
    }

    /// Handles key presses that no widget captured while the popup is open.
    ///
    /// cosmic-panel hands keyboard focus back to the applet's panel-button surface
    /// right after the popup maps, so keys typed immediately after opening arrive
    /// on the main window rather than the popup, and Wayland gives the client no
    /// way to take focus back. Those keys are applied to the editor or the search
    /// query here, so typing works without clicking into the popup first. The same
    /// path covers an unfocused editor inside the popup.
    fn handle_uncaptured_key(
        &mut self,
        window: window::Id,
        mut input: KeyInput,
    ) -> app::Task<Message> {
        let Some(popup) = self.popup else {
            return app::Task::none();
        };
        let in_popup = window == popup;
        if !in_popup {
            input.modifiers = held_modifiers();
        }

        if self.is_searching {
            return self.search_key(in_popup, input);
        }

        let keypress = KeyPress {
            key: input.key,
            modified_key: input.modified_key,
            physical_key: input.physical_key,
            modifiers: input.modifiers,
            text: input.text,
            status: text_editor::Status::Focused { is_hovered: false },
        };
        let Some(binding) = editor_binding(&self.altgr_map, keypress) else {
            return app::Task::none();
        };
        let is_editing = !matches!(binding, Binding::Custom(_) | Binding::Unfocus);
        let task = self.apply_binding(binding);
        if in_popup && is_editing {
            // The editor lost widget focus inside the popup (e.g. a button was
            // clicked); give it back so the cursor is shown again.
            app::Task::batch([task, self.focus_editor()])
        } else {
            task
        }
    }

    fn search_key(&mut self, in_popup: bool, input: KeyInput) -> app::Task<Message> {
        if altgr_held(input.modifiers)
            && let Some((normal_ch, altgr_ch)) =
                self.altgr_map
                    .resolve_key(&input.key, input.physical_key, input.modifiers.shift())
        {
            if in_popup {
                // The focused search input may already have inserted the
                // level-1 character for this key; replace it.
                let text_ch = input.text.as_deref().and_then(|s| s.chars().next());
                if text_ch == Some(altgr_ch) {
                    return app::Task::none();
                }
                if let Some(norm) = normal_ch
                    && self.search_query.ends_with(norm)
                {
                    self.search_query.pop();
                }
            }
            self.search_query.push(altgr_ch);
            self.update_search_filter();
            return app::Task::none();
        }

        if let Some(message) = shortcut_message(&input.key, input.modifiers) {
            return self.update(message);
        }
        if in_popup {
            // The search input handles its own typing.
            return app::Task::none();
        }

        match input.key.as_ref() {
            Key::Named(Named::Enter) => return self.update(Message::SubmitSearch),
            Key::Named(Named::Backspace) => {
                self.search_query.pop();
                self.update_search_filter();
                return app::Task::none();
            }
            _ => {}
        }
        if !input.modifiers.control()
            && !input.modifiers.logo()
            && let Some(text) = input.text.as_deref()
        {
            let printable: String = text.chars().filter(|c| !c.is_control()).collect();
            if !printable.is_empty() {
                self.search_query.push_str(&printable);
                self.update_search_filter();
            }
        }
        app::Task::none()
    }

    /// Gives keyboard focus to the note editor. `text_input::focus` emits the generic
    /// iced focus operation, which matches any focusable widget by id, including
    /// `text_editor`.
    fn focus_editor(&self) -> app::Task<Message> {
        text_input::focus(self.editor_id.clone())
    }

    fn persist_config(&self) {
        if let Some(handler) = &self.config_handler
            && let Err(err) = self.config.write_entry(handler)
        {
            tracing::warn!(?err, "Could not persist scratchpad config");
        }
    }
}

impl cosmic::Application for ScratchpadApp {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = APP_ID;

    fn core(&self) -> &cosmic::app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::app::Core {
        &mut self.core
    }

    fn init(core: cosmic::app::Core, _flags: Self::Flags) -> (Self, app::Task<Self::Message>) {
        let (config_handler, mut config) = load_config();
        let note_paths = storage::list_notes().unwrap_or_else(|_| vec![storage::pad_path(0)]);

        let mut notes = Vec::with_capacity(note_paths.len());
        for (i, path) in note_paths.into_iter().enumerate() {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let fallback = fl!("note-title-fallback", index = (i + 1));
            notes.push(NoteItem::from_text(i as NoteId, path, &text, &fallback));
        }
        let next_note_id = notes.len() as NoteId;

        let active_index = if let Some(ref file) = config.active_file {
            notes.iter().position(|n| n.filename == *file).unwrap_or(0)
        } else {
            config.active_tab.min(notes.len().saturating_sub(1))
        };
        config.active_tab = active_index;
        config.active_file = notes.get(active_index).map(|n| n.filename.clone());

        let filtered_indices = (0..notes.len()).collect();

        let app = Self {
            core,
            config,
            config_handler,
            popup: None,
            notes,
            next_note_id,
            pending_deletes: Vec::new(),
            active_index,
            copied_recently: false,
            copy_generation: 0,
            show_settings: false,
            undo_generation: 0,
            undo_cache: None,
            is_searching: false,
            search_query: String::new(),
            filtered_indices,
            altgr_map: Arc::new(ui::AltGrMap::load_system()),
            editor_id: cosmic::iced::core::widget::Id::unique(),
        };

        (app, app::Task::none())
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Self::Message> {
        Some(Message::CloseRequested(id))
    }

    fn update(&mut self, message: Self::Message) -> app::Task<Self::Message> {
        match message {
            Message::TogglePopup => {
                tracing::info!("TogglePopup invoked, current popup={:?}", self.popup);
                reset_key_state();
                if let Some(p) = self.popup.take() {
                    let save_task = self.save_note_async(self.active_index);
                    self.persist_config();
                    let destroy_task =
                        cosmic::surface::surface_task(cosmic::surface::action::destroy_popup(p));
                    return app::Task::batch([save_task, destroy_task]);
                }

                self.altgr_map = Arc::new(ui::AltGrMap::load_system());

                // If opening, check for external disk edits on all notes
                for (i, note) in self.notes.iter_mut().enumerate() {
                    if note.save.is_saved()
                        && let Ok(Some((reloaded, new_mtime))) =
                            storage::load_note_if_modified(&note.path, note.last_mtime)
                    {
                        let fallback = fl!("note-title-fallback", index = (i + 1));
                        note.title = storage::derive_title(&reloaded, &fallback);
                        note.word_chars = ui::count_words_and_chars(&reloaded);
                        note.content = content_at_end(&reloaded);
                        note.last_mtime = Some(new_mtime);
                    }
                }
                self.update_search_filter();

                // Focus is applied from `PopupFocusGained` once the popup surface
                // exists; a focus operation issued here would find no widget tree.
                cosmic::surface::surface_task(cosmic::surface::action::app_popup(
                    |_| Default::default(),
                    |app: &mut Self| {
                        let new_id = window::Id::unique();
                        app.popup.replace(new_id);

                        let mut popup_settings = app.core.applet.get_popup_settings(
                            app.core.main_window_id().unwrap_or(window::Id::RESERVED),
                            new_id,
                            Some((POPUP_WIDTH as u32, POPUP_HEIGHT as u32)),
                            None,
                            None,
                        );
                        popup_settings.positioner.size_limits = Limits::NONE
                            .min_width(POPUP_WIDTH)
                            .max_width(POPUP_WIDTH)
                            .min_height(POPUP_HEIGHT)
                            .max_height(POPUP_HEIGHT);
                        popup_settings
                    },
                    None,
                ))
            }

            Message::CloseRequested(id) => {
                if self.popup == Some(id) {
                    reset_key_state();
                    let save_task = self.save_note_async(self.active_index);
                    self.persist_config();
                    self.popup = None;
                    return save_task;
                }
                app::Task::none()
            }

            Message::Surface(a) => cosmic::task::message(cosmic::Action::Surface(a)),

            Message::PrevNote => {
                if self.notes.len() > 1 {
                    let prev_index = if self.active_index == 0 {
                        self.notes.len() - 1
                    } else {
                        self.active_index - 1
                    };
                    return self.select_note_internal(prev_index);
                }
                app::Task::none()
            }

            Message::NextNote => {
                if self.notes.len() > 1 {
                    let next_index = if self.active_index + 1 >= self.notes.len() {
                        0
                    } else {
                        self.active_index + 1
                    };
                    return self.select_note_internal(next_index);
                }
                app::Task::none()
            }

            Message::SelectNote(target) => self.select_note_internal(target),

            Message::NewNote(title_opt) => {
                let save_task = self.save_note_async(self.active_index);

                match storage::create_new_note(title_opt.as_deref()) {
                    Ok(new_path) => {
                        let text = std::fs::read_to_string(&new_path).unwrap_or_default();
                        let fallback = fl!("note-title-fallback", index = (self.notes.len() + 1));
                        let id = self.alloc_note_id();
                        self.notes
                            .push(NoteItem::from_text(id, new_path, &text, &fallback));

                        self.active_index = self.notes.len() - 1;
                        self.is_searching = false;
                        self.search_query.clear();
                        self.config.active_tab = self.active_index;
                        self.config.active_file = self
                            .notes
                            .get(self.active_index)
                            .map(|n| n.filename.clone());
                        self.update_search_filter();
                    }
                    Err(err) => {
                        tracing::error!(?err, "Failed to create new note");
                    }
                }

                app::Task::batch([save_task, self.focus_editor()])
            }

            Message::DeleteActiveNote => {
                if self.notes.is_empty() {
                    return app::Task::none();
                }

                let deleted_idx = self.active_index;
                let target_note = self.notes.remove(deleted_idx);
                let deleted_text = target_note.content.text();

                if target_note.save.is_in_flight() {
                    // Deleting now would let the running write recreate the file.
                    self.pending_deletes.push(PendingDelete {
                        id: target_note.id,
                        path: target_note.path.clone(),
                        save: target_note.save,
                    });
                } else if let Err(err) = storage::delete_note(&target_note.path) {
                    tracing::error!(?err, path = ?target_note.path, "Failed to delete note");
                }

                self.undo_generation = self.undo_generation.wrapping_add(1);
                let generation = self.undo_generation;
                self.undo_cache = Some((generation, target_note.path, deleted_text));

                if self.notes.is_empty()
                    && let Ok(new_path) = storage::create_new_note(None)
                {
                    let fallback = fl!("note-title-fallback", index = 1);
                    let id = self.alloc_note_id();
                    self.notes
                        .push(NoteItem::from_text(id, new_path, "", &fallback));
                }

                self.active_index = self.active_index.min(self.notes.len().saturating_sub(1));
                self.config.active_tab = self.active_index;
                self.config.active_file = self
                    .notes
                    .get(self.active_index)
                    .map(|n| n.filename.clone());
                self.update_search_filter();

                let banner_task = app::Task::future(async move {
                    tokio::time::sleep(Duration::from_secs(UNDO_BANNER_SECS)).await;
                    cosmic::Action::App(Message::DismissUndoBanner(generation))
                });
                app::Task::batch([self.focus_editor(), banner_task])
            }

            Message::UndoDelete => {
                if let Some((_, path, content_text)) = self.undo_cache.take() {
                    let fallback = fl!("note-title-fallback", index = (self.notes.len() + 1));

                    // If the deleted note's last write is still running, reuse its id
                    // and save state so that completion is matched to the restored
                    // note and the restored text is written after it, not raced.
                    let mut restored = if let Some(pos) =
                        self.pending_deletes.iter().position(|p| p.path == path)
                    {
                        let pending = self.pending_deletes.remove(pos);
                        let mut note =
                            NoteItem::from_text(pending.id, path.clone(), &content_text, &fallback);
                        note.save = pending.save;
                        note
                    } else {
                        let id = self.alloc_note_id();
                        NoteItem::from_text(id, path.clone(), &content_text, &fallback)
                    };
                    // The file is gone or about to be; write it back in the background.
                    restored.save.mark_edited();

                    if let Some(pos) = self.notes.iter().position(|n| n.path == path) {
                        self.notes[pos] = restored;
                        self.active_index = pos;
                    } else if self.notes.len() == 1
                        && self.notes[0].content.text().trim().is_empty()
                        && self.notes[0].save.is_saved()
                    {
                        if self.notes[0].path != path {
                            let _ = std::fs::remove_file(&self.notes[0].path);
                        }
                        self.notes[0] = restored;
                        self.active_index = 0;
                    } else {
                        self.notes.push(restored);
                        self.active_index = self.notes.len() - 1;
                    }

                    self.config.active_tab = self.active_index;
                    self.config.active_file = self
                        .notes
                        .get(self.active_index)
                        .map(|n| n.filename.clone());
                    self.update_search_filter();
                    // Starts the write, or queues it behind the running one.
                    let save_task = self.save_note_async(self.active_index);
                    return app::Task::batch([save_task, self.focus_editor()]);
                }
                app::Task::none()
            }

            Message::DismissUndoBanner(target_gen) => {
                if let Some((g, _, _)) = self.undo_cache
                    && g == target_gen
                {
                    self.undo_cache = None;
                }
                app::Task::none()
            }

            Message::ToggleSearch => {
                self.is_searching = !self.is_searching;
                if self.is_searching {
                    self.update_search_filter();
                    return focus_search_input();
                }
                self.focus_editor()
            }

            Message::SearchInputChanged(query) => {
                self.search_query = query;
                self.update_search_filter();
                app::Task::none()
            }

            Message::SubmitSearch => {
                let query = self.search_query.trim().to_string();
                if query.is_empty() {
                    if let Some(&first_idx) = self.filtered_indices.first() {
                        return self.select_note_internal(first_idx);
                    }
                    self.is_searching = false;
                    return app::Task::none();
                }

                // If exact title match exists, select it
                let query_lower = query.to_lowercase();
                if let Some(pos) = self
                    .notes
                    .iter()
                    .position(|n| n.title.to_lowercase() == query_lower)
                {
                    return self.select_note_internal(pos);
                }

                // If there are partial matches, select the first match
                if let Some(&first_idx) = self.filtered_indices.first() {
                    return self.select_note_internal(first_idx);
                }

                // Otherwise, create new note with query as title
                self.update(Message::NewNote(Some(query)))
            }

            Message::NoteSaved(id, revision, outcome) => {
                self.handle_note_saved(id, revision, outcome)
            }

            Message::EditorAction(action) => {
                let idx = self.active_index;
                let is_edit = action.is_edit();
                let mut edited_id = None;
                if let Some(note) = self.notes.get_mut(idx) {
                    note.content.perform(action);
                    if is_edit {
                        note.save.mark_edited();
                        note.last_edit_time = Some(Instant::now());
                        edited_id = Some(note.id);
                    }
                }

                if let Some(id) = edited_id {
                    self.update_note_meta(idx);
                    app::Task::future(async move {
                        tokio::time::sleep(Duration::from_millis(DEBOUNCE_MILLIS)).await;
                        cosmic::Action::App(Message::DebounceTimeout(id))
                    })
                } else {
                    app::Task::none()
                }
            }

            Message::DebounceTimeout(id) => {
                if let Some(idx) = self.note_index(id)
                    && let Some(last_time) = self.notes[idx].last_edit_time
                    && last_time.elapsed() >= Duration::from_millis(DEBOUNCE_MILLIS - 50)
                {
                    return self.save_note_async(idx);
                }
                app::Task::none()
            }

            Message::CopyAll => {
                let text_to_copy = self.current_text(self.active_index);
                self.copied_recently = true;
                self.copy_generation = self.copy_generation.wrapping_add(1);
                let generation = self.copy_generation;

                let copy_task = cosmic::iced::clipboard::write(text_to_copy);
                let reset_task = app::Task::future(async move {
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                    cosmic::Action::App(Message::ResetCopyStatus(generation))
                });

                app::Task::batch([copy_task, reset_task])
            }

            Message::ResetCopyStatus(target_gen) => {
                if self.copy_generation == target_gen {
                    self.copied_recently = false;
                }
                app::Task::none()
            }

            Message::ToggleSettings => {
                self.show_settings = !self.show_settings;
                app::Task::none()
            }

            Message::ToggleWrapLines => {
                self.config.wrap_lines = !self.config.wrap_lines;
                self.persist_config();
                app::Task::none()
            }

            Message::AdjustFontSize(delta) => {
                let new_size = (self.config.font_size + delta).clamp(10.0, 24.0);
                if (new_size - self.config.font_size).abs() > f32::EPSILON {
                    self.config.font_size = new_size;
                    self.persist_config();
                }
                app::Task::none()
            }

            Message::EscapePressed => {
                ALTGR_ACTIVE.store(false, Ordering::SeqCst);
                if self.is_searching {
                    self.is_searching = false;
                    self.search_query.clear();
                    return self.focus_editor();
                }

                if let Some(p) = self.popup.take() {
                    let save_task = self.save_note_async(self.active_index);
                    self.persist_config();
                    let destroy_task =
                        cosmic::surface::surface_task(cosmic::surface::action::destroy_popup(p));
                    return app::Task::batch([save_task, destroy_task]);
                }
                app::Task::none()
            }

            Message::KeyInput(window, input) => self.handle_uncaptured_key(window, input),

            Message::PopupFocusGained(id) => {
                if self.popup != Some(id) {
                    return app::Task::none();
                }
                if self.is_searching {
                    focus_search_input()
                } else {
                    self.focus_editor()
                }
            }
        }
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        // `Opened` fires on the popup's first configure, after its widget tree
        // is built. `PopupEvent::Focused` fires when the compositor hands the
        // popup keyboard focus. Both can arrive before a subscription gated on
        // `self.popup` is registered, so this one stays active at all times.
        let focus_sub = cosmic::iced::event::listen_with(|event, _status, window_id| match event {
            cosmic::iced::core::Event::PlatformSpecific(
                cosmic::iced::core::event::PlatformSpecific::Wayland(
                    cosmic::iced::core::event::wayland::Event::Popup(
                        cosmic::iced::core::event::wayland::PopupEvent::Focused,
                        _,
                        _,
                    ),
                ),
            )
            | cosmic::iced::core::Event::Window(cosmic::iced::core::window::Event::Opened {
                ..
            }) => Some(Message::PopupFocusGained(window_id)),
            _ => None,
        });

        if self.popup.is_some() {
            let keyboard_sub =
                cosmic::iced::event::listen_with(|event, status, window_id| match event {
                    cosmic::iced::core::Event::Keyboard(
                        cosmic::iced::core::keyboard::Event::KeyPressed {
                            key,
                            modified_key,
                            physical_key,
                            modifiers,
                            text,
                            ..
                        },
                    ) => {
                        if ui::is_altgr_key(&key, physical_key) {
                            ALTGR_ACTIVE.store(true, Ordering::SeqCst);
                            return None;
                        }
                        track_modifier(&key, true);
                        if matches!(status, cosmic::iced::core::event::Status::Captured) {
                            return None;
                        }
                        Some(Message::KeyInput(
                            window_id,
                            KeyInput {
                                key,
                                modified_key,
                                physical_key,
                                modifiers,
                                text,
                            },
                        ))
                    }
                    cosmic::iced::core::Event::Keyboard(
                        cosmic::iced::core::keyboard::Event::KeyReleased {
                            key, physical_key, ..
                        },
                    ) => {
                        if ui::is_altgr_key(&key, physical_key) {
                            ALTGR_ACTIVE.store(false, Ordering::SeqCst);
                        }
                        track_modifier(&key, false);
                        None
                    }
                    _ => None,
                });

            Subscription::batch([keyboard_sub, focus_sub])
        } else {
            focus_sub
        }
    }

    fn view(&self) -> Element<'_, Self::Message> {
        self.core
            .applet
            .icon_button("io.github.hasmolam.cosmic-ext-applet-scratchpad-symbolic")
            .on_press_down(Message::TogglePopup)
            .into()
    }

    fn view_window(&self, id: window::Id) -> Element<'_, Self::Message> {
        if matches!(self.popup, Some(p) if p == id) {
            let cosmic_theme = theme::active();
            let spacing = cosmic_theme.cosmic().spacing;

            if self.is_searching {
                // Notational Velocity Search View
                let search_bar =
                    text_input::search_input(fl!("search-placeholder"), &self.search_query)
                        .id(cosmic::iced::core::widget::Id::new(SEARCH_INPUT_ID))
                        .on_input(Message::SearchInputChanged)
                        .on_submit(|_| Message::SubmitSearch)
                        .width(Length::Fill);

                let close_search_btn = button::custom(
                    icon::from_name("window-close-symbolic")
                        .size(16)
                        .symbolic(true),
                )
                .on_press(Message::ToggleSearch)
                .class(theme::Button::Text)
                .padding([4, 6]);

                let search_header = row![search_bar, close_search_btn]
                    .align_y(Alignment::Center)
                    .spacing(4)
                    .width(Length::Fill);

                let mut results_items = Vec::new();

                let trimmed_query = self.search_query.trim();
                let has_exact_match = self
                    .notes
                    .iter()
                    .any(|n| n.title.eq_ignore_ascii_case(trimmed_query));

                if !trimmed_query.is_empty() && !has_exact_match {
                    let create_card = button::custom(
                        row![
                            icon::from_name("list-add-symbolic").size(16).symbolic(true),
                            text(fl!("search-create", title = trimmed_query)).size(13),
                        ]
                        .spacing(8)
                        .align_y(Alignment::Center),
                    )
                    .on_press(Message::NewNote(Some(trimmed_query.to_string())))
                    .class(theme::Button::Suggested)
                    .width(Length::Fill)
                    .padding([8, 10]);

                    results_items.push(create_card.into());
                }

                if self.filtered_indices.is_empty() && self.search_query.trim().is_empty() {
                    results_items.push(
                        container(text::caption(fl!("search-no-results")))
                            .padding(16)
                            .align_x(Alignment::Center)
                            .width(Length::Fill)
                            .into(),
                    );
                } else {
                    for &idx in &self.filtered_indices {
                        let note = &self.notes[idx];
                        let is_active = idx == self.active_index;
                        let title_text = text(&note.title).size(13);
                        let count_text = text::caption(format!(
                            "{}/{} · {}w",
                            idx + 1,
                            self.notes.len(),
                            note.word_chars.0
                        ));
                        let preview_text = text::caption(ui::get_preview_snippet(
                            &note.content.text(),
                            &self.search_query,
                        ));

                        let card = button::custom(
                            column![
                                row![
                                    title_text,
                                    space::horizontal().width(Length::Fill),
                                    count_text
                                ]
                                .align_y(Alignment::Center),
                                preview_text,
                            ]
                            .spacing(2)
                            .width(Length::Fill),
                        )
                        .on_press(Message::SelectNote(idx))
                        .class(if is_active {
                            theme::Button::Suggested
                        } else {
                            theme::Button::Standard
                        })
                        .width(Length::Fill)
                        .padding([6, 10]);

                        results_items.push(card.into());
                    }
                }

                let results_col = column::with_children(results_items)
                    .spacing(4)
                    .width(Length::Fill);

                let results_scroll = scrollable(results_col)
                    .height(Length::Fill)
                    .width(Length::Fill);

                let popover_col = column![search_header, results_scroll]
                    .spacing(spacing.space_xs)
                    .padding([8, 12])
                    .width(Length::Fixed(POPUP_WIDTH))
                    .height(Length::Fixed(POPUP_HEIGHT));

                return self.core.applet.popup_container(popover_col).into();
            }

            // Normal Editor View
            // 1. Navigation Header: < [1/N Title] > [+] (Left) and Actions (Right)
            let prev_btn = button::custom(
                icon::from_name("go-previous-symbolic")
                    .size(16)
                    .symbolic(true),
            )
            .on_press(Message::PrevNote)
            .class(theme::Button::Text)
            .padding([4, 6]);

            let prev_with_tooltip = tooltip(
                prev_btn,
                text::caption(format!("{} (Ctrl+[)", fl!("action-prev"))),
                tooltip::Position::Bottom,
            );

            let active_note = &self.notes[self.active_index];
            let note_counter_title = format!(
                "{}/{} · {}",
                self.active_index + 1,
                self.notes.len(),
                active_note.title
            );

            // The title takes whatever width the icons leave and ellipsizes on
            // one line, so a long title cannot push the action icons off the
            // popup.
            let title_label = text::body(note_counter_title)
                .wrapping(cosmic::iced::core::text::Wrapping::None)
                .ellipsize(cosmic::iced::core::text::Ellipsize::End(
                    cosmic::iced::core::text::EllipsizeHeightLimit::Lines(1),
                ))
                .width(Length::Fill);

            let title_btn = button::custom(title_label)
                .on_press(Message::ToggleSearch)
                .class(theme::Button::Text)
                .padding([4, 8])
                .width(Length::Fill);

            let title_with_tooltip = tooltip(
                title_btn,
                text::caption(format!("{} (Ctrl+F)", fl!("action-search"))),
                tooltip::Position::Bottom,
            );

            let next_btn =
                button::custom(icon::from_name("go-next-symbolic").size(16).symbolic(true))
                    .on_press(Message::NextNote)
                    .class(theme::Button::Text)
                    .padding([4, 6]);

            let next_with_tooltip = tooltip(
                next_btn,
                text::caption(format!("{} (Ctrl+])", fl!("action-next"))),
                tooltip::Position::Bottom,
            );

            let new_btn =
                button::custom(icon::from_name("list-add-symbolic").size(16).symbolic(true))
                    .on_press(Message::NewNote(None))
                    .class(theme::Button::Text)
                    .padding([4, 6]);

            let new_with_tooltip = tooltip(
                new_btn,
                text::caption(format!("{} (Ctrl+N)", fl!("action-new"))),
                tooltip::Position::Bottom,
            );

            let nav_group = row![
                prev_with_tooltip,
                next_with_tooltip,
                new_with_tooltip,
                title_with_tooltip,
            ]
            .spacing(2)
            .align_y(Alignment::Center)
            .width(Length::Fill);

            // Right Quick Actions
            let search_btn = button::custom(
                icon::from_name("system-search-symbolic")
                    .size(16)
                    .symbolic(true),
            )
            .on_press(Message::ToggleSearch)
            .class(theme::Button::Text)
            .padding([4, 6]);

            let search_with_tooltip = tooltip(
                search_btn,
                text::caption(format!("{} (Ctrl+F)", fl!("action-search"))),
                tooltip::Position::Bottom,
            );

            let copy_icon = if self.copied_recently {
                icon::from_name("emblem-ok-symbolic")
                    .size(16)
                    .symbolic(true)
            } else {
                icon::from_name("edit-copy-symbolic")
                    .size(16)
                    .symbolic(true)
            };

            let copy_tooltip_text = if self.copied_recently {
                fl!("action-copied")
            } else {
                format!("{} (Ctrl+Shift+C)", fl!("action-copy"))
            };

            let copy_btn = button::custom(copy_icon)
                .on_press(Message::CopyAll)
                .class(if self.copied_recently {
                    theme::Button::Suggested
                } else {
                    theme::Button::Text
                })
                .padding([4, 6]);

            let copy_with_tooltip = tooltip(
                copy_btn,
                text::caption(copy_tooltip_text),
                tooltip::Position::Bottom,
            );

            let delete_btn = button::custom(
                icon::from_name("user-trash-symbolic")
                    .size(16)
                    .symbolic(true),
            )
            .on_press(Message::DeleteActiveNote)
            .class(theme::Button::Text)
            .padding([4, 6]);

            let delete_with_tooltip = tooltip(
                delete_btn,
                text::caption(fl!("action-delete")),
                tooltip::Position::Bottom,
            );

            let settings_btn = button::custom(
                icon::from_name("preferences-system-symbolic")
                    .size(16)
                    .symbolic(true),
            )
            .on_press(Message::ToggleSettings)
            .class(if self.show_settings {
                theme::Button::Suggested
            } else {
                theme::Button::Text
            })
            .padding([4, 6]);

            let settings_with_tooltip = tooltip(
                settings_btn,
                text::caption(fl!("action-settings")),
                tooltip::Position::Bottom,
            );

            let actions_row = row![
                search_with_tooltip,
                copy_with_tooltip,
                delete_with_tooltip,
                settings_with_tooltip
            ]
            .spacing(2)
            .align_y(Alignment::Center);

            let header = row![nav_group, actions_row]
                .spacing(2)
                .align_y(Alignment::Center)
                .width(Length::Fill);

            // 2. Settings Drawer (Collapsible)
            let maybe_settings_drawer: Option<Element<_>> = if self.show_settings {
                let wrap_btn = button::text(fl!("setting-word-wrap"))
                    .on_press(Message::ToggleWrapLines)
                    .class(if self.config.wrap_lines {
                        theme::Button::Suggested
                    } else {
                        theme::Button::Text
                    })
                    .padding([2, 8]);

                let dec_btn = button::text("-")
                    .on_press(Message::AdjustFontSize(-1.0))
                    .class(theme::Button::Text)
                    .padding([2, 6]);

                let font_label = text::caption(fl!(
                    "setting-font-size",
                    size = (self.config.font_size as u32)
                ));

                let inc_btn = button::text("+")
                    .on_press(Message::AdjustFontSize(1.0))
                    .class(theme::Button::Text)
                    .padding([2, 6]);

                let font_controls = row![dec_btn, font_label, inc_btn]
                    .align_y(Alignment::Center)
                    .spacing(2);

                Some(
                    container(
                        row![
                            wrap_btn,
                            space::horizontal().width(Length::Fill),
                            font_controls
                        ]
                        .align_y(Alignment::Center)
                        .width(Length::Fill),
                    )
                    .padding([4, 8])
                    .class(theme::Container::Card)
                    .into(),
                )
            } else {
                None
            };

            // 3. Undo Notification Banner (discreet inline banner)
            let maybe_undo_banner: Option<Element<_>> = if self.undo_cache.is_some() {
                Some(
                    container(
                        row![
                            text::caption(fl!("banner-deleted")),
                            space::horizontal().width(Length::Fill),
                            button::text(fl!("action-undo"))
                                .on_press(Message::UndoDelete)
                                .class(theme::Button::Suggested)
                                .padding([2, 8]),
                        ]
                        .align_y(Alignment::Center)
                        .width(Length::Fill),
                    )
                    .padding([4, 8])
                    .class(theme::Container::Card)
                    .into(),
                )
            } else {
                None
            };

            // 4. Text Editor with Font Size & Line Wrapping
            let altgr_map = Arc::clone(&self.altgr_map);
            let editor = text_editor::text_editor(&self.notes[self.active_index].content)
                .id(self.editor_id.clone())
                .placeholder(fl!("placeholder"))
                .size(self.config.font_size)
                .wrapping(if self.config.wrap_lines {
                    cosmic::iced::core::text::Wrapping::Word
                } else {
                    cosmic::iced::core::text::Wrapping::None
                })
                .key_binding(move |keypress| editor_binding(&altgr_map, keypress))
                .height(Length::Fill)
                .padding(10)
                .on_action(Message::EditorAction);

            let editor_container = container(editor)
                .width(Length::Fill)
                .height(Length::Fill)
                .class(theme::Container::Card);

            // 5. Footer: Word & Character Counter (Left), Save Status (Right)
            let (words, chars) = self.notes[self.active_index].word_chars;

            let counter_label = text::caption(fl!("status-count", words = words, chars = chars));

            let save = &self.notes[self.active_index].save;
            let status_label = if save.has_failed() {
                text::caption(format!("● {}", fl!("status-save-failed"))).class(
                    theme::Text::Custom(|theme| cosmic::iced::widget::text::Style {
                        color: Some(theme.cosmic().destructive_text_color().into()),
                        ..Default::default()
                    }),
                )
            } else if save.is_saved() {
                text::caption(format!("● {}", fl!("status-saved")))
            } else {
                text::caption(format!("● {}", fl!("status-editing"))).class(theme::Text::Accent)
            };

            let footer = row![
                counter_label,
                space::horizontal().width(Length::Fill),
                status_label
            ]
            .align_y(Alignment::Center)
            .width(Length::Fill);

            // Assemble Popover Content
            let mut content_elements = Vec::with_capacity(5);
            content_elements.push(header.into());
            if let Some(settings) = maybe_settings_drawer {
                content_elements.push(settings);
            }
            if let Some(banner) = maybe_undo_banner {
                content_elements.push(banner);
            }
            content_elements.push(editor_container.into());
            content_elements.push(footer.into());

            let popover_col = column::with_children(content_elements)
                .spacing(spacing.space_xs)
                .padding([8, 12])
                .width(Length::Fixed(POPUP_WIDTH))
                .height(Length::Fixed(POPUP_HEIGHT));

            self.core.applet.popup_container(popover_col).into()
        } else {
            cosmic::widget::text("").into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic::iced::core::widget::{Id, Tree, tree};

    fn editor_column<'a>(content: &'a Content, id: Id) -> cosmic::Element<'a, Message> {
        cosmic::widget::column::Column::new()
            .push(text_editor::text_editor(content).id(id))
            .into()
    }

    /// Mirrors `UserInterface::build`: named states are taken out of the previous
    /// tree, then restored while diffing against the new view.
    fn rebuild_keeps_editor_state(id: Id) -> bool {
        let content = Content::with_text("hello");
        let mut tree = Tree::new(editor_column(&content, id.clone()));
        let named = tree.take_all_named();
        tree::NAMED.with(|n| *n.borrow_mut() = named);
        tree.diff(editor_column(&content, id).as_widget_mut());
        tree::NAMED.with(|n| n.borrow_mut().clear());
        // column -> context-menu wrapper -> inner iced editor
        !matches!(tree.children[0].children[0].state, tree::State::None)
    }

    #[test]
    fn unique_editor_id_survives_rebuild() {
        assert!(rebuild_keeps_editor_state(Id::unique()));
    }

    /// Documents the upstream behavior that forces `editor_id` to be unique. If this
    /// starts failing, libcosmic fixed the wrapper and a named id becomes usable.
    #[test]
    fn named_editor_id_loses_state_on_rebuild() {
        assert!(!rebuild_keeps_editor_state(Id::new("scratchpad_editor")));
    }

    fn char_key(c: &str) -> Key {
        Key::Character(SmolStr::new(c))
    }

    #[test]
    fn shortcut_message_maps_app_shortcuts() {
        let ctrl = Modifiers::CTRL;
        assert!(matches!(
            shortcut_message(&char_key("f"), ctrl),
            Some(Message::ToggleSearch)
        ));
        assert!(matches!(
            shortcut_message(&char_key("n"), ctrl),
            Some(Message::NewNote(None))
        ));
        assert!(matches!(
            shortcut_message(&char_key("C"), ctrl | Modifiers::SHIFT),
            Some(Message::CopyAll)
        ));
        assert!(matches!(
            shortcut_message(&Key::Named(Named::Escape), Modifiers::empty()),
            Some(Message::EscapePressed)
        ));
        // Plain typing and Ctrl+C (editor copy) are not app shortcuts.
        assert!(shortcut_message(&char_key("f"), Modifiers::empty()).is_none());
        assert!(shortcut_message(&char_key("c"), ctrl).is_none());
        // Ctrl+Alt is AltGr on some setups and must not trigger shortcuts.
        assert!(shortcut_message(&char_key("f"), ctrl | Modifiers::ALT).is_none());
    }

    fn keypress(key: Key, modifiers: Modifiers, text: Option<&str>) -> KeyPress {
        KeyPress {
            modified_key: key.clone(),
            key,
            physical_key: Physical::Unidentified(
                cosmic::iced::core::keyboard::key::NativeCode::Unidentified,
            ),
            modifiers,
            text: text.map(SmolStr::new),
            status: text_editor::Status::Focused { is_hovered: false },
        }
    }

    #[test]
    fn editor_binding_inserts_text_and_routes_shortcuts() {
        let map = ui::AltGrMap::default();
        let typed = editor_binding(&map, keypress(char_key("a"), Modifiers::empty(), Some("a")));
        assert!(matches!(typed, Some(Binding::Insert('a'))));

        let search = editor_binding(&map, keypress(char_key("f"), Modifiers::CTRL, None));
        assert!(matches!(
            search,
            Some(Binding::Custom(Message::ToggleSearch))
        ));

        let enter = editor_binding(
            &map,
            keypress(Key::Named(Named::Enter), Modifiers::empty(), Some("\r")),
        );
        assert!(matches!(enter, Some(Binding::Enter)));
    }

    #[test]
    fn tracked_modifiers_follow_press_and_release() {
        reset_key_state();
        track_modifier(&Key::Named(Named::Control), true);
        track_modifier(&Key::Named(Named::Shift), true);
        assert_eq!(held_modifiers(), Modifiers::CTRL | Modifiers::SHIFT);
        track_modifier(&Key::Named(Named::Control), false);
        assert_eq!(held_modifiers(), Modifiers::SHIFT);
        track_modifier(&char_key("a"), true);
        assert_eq!(held_modifiers(), Modifiers::SHIFT);
        reset_key_state();
        assert_eq!(held_modifiers(), Modifiers::empty());
    }

    #[test]
    fn content_at_end_places_cursor_after_last_char() {
        let content = content_at_end("one\ntwo");
        let cursor = content.cursor();
        assert_eq!(cursor.position.line, 1);
        assert_eq!(cursor.position.column, 3);
    }
}
