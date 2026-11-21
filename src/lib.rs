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
use std::path::{Path, PathBuf};
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

fn shortcut_to_message(shortcut: ui::Shortcut) -> Message {
    match shortcut {
        ui::Shortcut::Escape => Message::EscapePressed,
        ui::Shortcut::ToggleSearch => Message::ToggleSearch,
        ui::Shortcut::NewNote => Message::NewNote(None),
        ui::Shortcut::PrevNote => Message::PrevNote,
        ui::Shortcut::NextNote => Message::NextNote,
        ui::Shortcut::CopyAll => Message::CopyAll,
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
    if let Some(shortcut) = ui::shortcut_for(&keypress.key, keypress.modifiers) {
        return Some(Binding::Custom(shortcut_to_message(shortcut)));
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
    pub plain_text: Arc<str>,
    pub title_lower: String,
    pub text_lower: String,
    pub content: Content,
    pub word_chars: (usize, usize),
    pub last_edit_time: Option<Instant>,
    pub last_mtime: Option<SystemTime>,
    pub save: SaveState,
}

impl NoteItem {
    fn from_loaded(
        id: NoteId,
        path: PathBuf,
        text: &str,
        fallback_title: &str,
        last_mtime: Option<SystemTime>,
    ) -> Self {
        let filename = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let title = storage::derive_title(text, fallback_title);
        let plain_text: Arc<str> = Arc::from(text);
        let title_lower = title.to_lowercase();
        let text_lower = text.to_lowercase();
        Self {
            id,
            path,
            filename,
            title,
            plain_text,
            title_lower,
            text_lower,
            content: content_at_end(text),
            word_chars: ui::count_words_and_chars(text),
            last_edit_time: None,
            last_mtime,
            save: SaveState::default(),
        }
    }

    /// Builds a note whose in-memory text matches what is on disk at `path`.
    fn from_text(id: NoteId, path: PathBuf, text: &str, fallback_title: &str) -> Self {
        let last_mtime = storage::note_mtime(&path);
        Self::from_loaded(id, path, text, fallback_title, last_mtime)
    }

    /// Replaces in-memory note state from externally loaded or conflict-reloaded disk text.
    fn apply_text(&mut self, text: &str, fallback_title: &str, mtime: Option<SystemTime>) {
        let title = storage::derive_title(text, fallback_title);
        self.title_lower = title.to_lowercase();
        self.title = title;
        self.plain_text = Arc::from(text);
        self.text_lower = text.to_lowercase();
        self.word_chars = ui::count_words_and_chars(text);
        self.content = content_at_end(text);
        self.last_mtime = mtime;
        self.last_edit_time = None;
    }
}

struct SearchResult {
    index: usize,
    preview: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoticeBanner {
    Conflict { filename: String },
    Error(String),
}

struct DeletedBackup {
    id: NoteId,
    path: PathBuf,
    filename: String,
    title: String,
    plain_text: Arc<str>,
    last_mtime: Option<SystemTime>,
    save: SaveState,
}

/// A delete that waits for an in-flight save of the same note to finish, so the
/// write cannot recreate the file after it was moved to the trash.
struct PendingDelete {
    id: NoteId,
    path: PathBuf,
    backup: DeletedBackup,
}

#[derive(Debug, Clone)]
pub struct LoadedNoteData {
    pub path: PathBuf,
    pub text: String,
    pub mtime: Option<SystemTime>,
}

#[derive(Debug, Clone)]
pub struct InitPayload {
    pub data_dir: PathBuf,
    pub notes: Vec<LoadedNoteData>,
    pub altgr_map: Arc<ui::AltGrMap>,
}

#[derive(Debug, Clone)]
pub struct ExternalNoteUpdate {
    pub id: NoteId,
    pub content: String,
    pub mtime: SystemTime,
}

/// Result of a background note write.
#[derive(Debug, Clone)]
pub enum SaveOutcome {
    Saved(Option<SystemTime>),
    Conflict {
        conflict_path: PathBuf,
        disk_content: String,
        disk_mtime: SystemTime,
    },
    Failed,
}

#[derive(Debug, Clone)]
pub enum Message {
    Initialized(Result<InitPayload, String>),
    TogglePopup,
    PopupSyncDone {
        altgr_map: Arc<ui::AltGrMap>,
        updates: Vec<ExternalNoteUpdate>,
    },
    CloseRequested(window::Id),
    PrevNote,
    NextNote,
    NewNote(Option<String>),
    SelectNote(usize),
    DeleteActiveNote,
    UndoDelete,
    DismissUndoBanner(u64),
    DismissNotice,
    ToggleSearch,
    SearchInputChanged(String),
    SubmitSearch,
    EditorAction(Action),
    DebounceTimeout(NoteId),
    NoteSaved(NoteId, u64, SaveOutcome),
    DataDirChanged,
    ExternalNotesChecked(Vec<ExternalNoteUpdate>),
    CopyAll,
    PasteClipboard,
    ResetCopyStatus(u64),
    ToggleSettings,
    ToggleWrapLines,
    AdjustFontSize(f32),
    ConfigUpdated(ScratchpadConfig),
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
    data_dir: Option<PathBuf>,
    notes: Vec<NoteItem>,
    notes_loaded: bool,
    next_note_id: NoteId,
    pending_deletes: Vec<PendingDelete>,
    active_index: usize,
    copied_recently: bool,
    copy_generation: u64,
    show_settings: bool,
    undo_generation: u64,
    undo_cache: Option<(u64, PathBuf, Arc<str>)>,
    notice_banner: Option<NoticeBanner>,
    is_searching: bool,
    search_query: String,
    search_results: Vec<SearchResult>,
    altgr_map: Arc<ui::AltGrMap>,
    /// Must be `Id::unique()`, not a named `Id::new(..)`. libcosmic's `TextEditor`
    /// reports the inner editor's id from its context-menu wrapper as well, so a
    /// named id appears twice in the widget tree. `Tree::take_all_named` then
    /// overwrites one state with the other and the editor panics with
    /// "Downcast on stateless state" on the next rebuild.
    editor_id: cosmic::iced::core::widget::Id,
}

impl ScratchpadApp {
    fn set_active(&mut self, index: usize) {
        self.active_index = index;
        self.config.active_file = self.notes.get(index).map(|n| n.filename.clone());
    }

    fn update_note_meta(&mut self, index: usize) {
        if let Some(note) = self.notes.get_mut(index) {
            let text = note.content.text();
            let fallback = fl!("note-title-fallback", index = (index + 1));
            let title = storage::derive_title(&text, &fallback);
            note.title_lower = title.to_lowercase();
            note.title = title;
            note.word_chars = ui::count_words_and_chars(&text);
            note.text_lower = text.to_lowercase();
            note.plain_text = Arc::from(text);
        }
    }

    fn update_search_filter(&mut self) {
        let query_lower = self.search_query.trim().to_lowercase();
        self.search_results = self
            .notes
            .iter()
            .enumerate()
            .filter(|(_, note)| {
                query_lower.is_empty()
                    || note.title_lower.contains(&query_lower)
                    || note.text_lower.contains(&query_lower)
            })
            .map(|(index, note)| SearchResult {
                index,
                preview: ui::get_preview_snippet_lower(&note.plain_text, &query_lower),
            })
            .collect();
    }

    fn alloc_note_id(&mut self) -> NoteId {
        let id = self.next_note_id;
        self.next_note_id = self.next_note_id.wrapping_add(1);
        id
    }

    fn note_index(&self, id: NoteId) -> Option<usize> {
        self.notes.iter().position(|note| note.id == id)
    }

    fn collect_clean_note_checks(&self) -> Vec<(NoteId, PathBuf, Option<SystemTime>)> {
        self.notes
            .iter()
            .filter(|note| note.save.is_saved() && !note.save.is_in_flight())
            .map(|note| (note.id, note.path.clone(), note.last_mtime))
            .collect()
    }

    fn apply_external_updates(&mut self, updates: Vec<ExternalNoteUpdate>) {
        let mut changed = false;
        for update in updates {
            if let Some(idx) = self.note_index(update.id) {
                let fallback = fl!("note-title-fallback", index = (idx + 1));
                let note = &mut self.notes[idx];
                // Re-check at apply time so edits typed while the task ran are not clobbered.
                if note.save.is_saved() && !note.save.is_in_flight() {
                    if note.plain_text.as_ref() != update.content {
                        note.apply_text(&update.content, &fallback, Some(update.mtime));
                        changed = true;
                    } else {
                        note.last_mtime = Some(update.mtime);
                    }
                }
            }
        }
        if changed {
            self.update_search_filter();
        }
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
        let text = Arc::clone(&note.plain_text);
        let path = note.path.clone();
        let expected_mtime = note.last_mtime;
        app::Task::future(async move {
            let res = tokio::task::spawn_blocking(move || {
                storage::save_note_checked(&path, &text, expected_mtime)
            })
            .await;
            let outcome = match res {
                Ok(Ok(storage::SaveResult::Saved(mtime))) => SaveOutcome::Saved(mtime),
                Ok(Ok(storage::SaveResult::Conflict {
                    conflict_path,
                    disk_content,
                    disk_mtime,
                })) => SaveOutcome::Conflict {
                    conflict_path,
                    disk_content,
                    disk_mtime,
                },
                Ok(Err(err)) => {
                    tracing::error!(?err, id, "Failed to save note in background");
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
        if let Some(index) = self.note_index(id) {
            match outcome {
                SaveOutcome::Saved(mtime) => {
                    let note = &mut self.notes[index];
                    let follow_up = note.save.finish(revision, true);
                    if let Some(t) = mtime {
                        note.last_mtime = Some(t);
                    }
                    if follow_up {
                        return self.save_note_async(index);
                    }
                }
                SaveOutcome::Failed => {
                    let note = &mut self.notes[index];
                    let follow_up = note.save.finish(revision, false);
                    if follow_up {
                        return self.save_note_async(index);
                    }
                }
                SaveOutcome::Conflict {
                    conflict_path,
                    disk_content,
                    disk_mtime,
                } => {
                    let fallback = fl!("note-title-fallback", index = (index + 1));
                    let note = &mut self.notes[index];
                    if note.save.resolve_conflict(revision) {
                        note.apply_text(&disk_content, &fallback, Some(disk_mtime));
                        tracing::warn!(?conflict_path, "External edit conflict preserved");
                        let filename = conflict_path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| conflict_path.display().to_string());
                        self.notice_banner = Some(NoticeBanner::Conflict { filename });
                        self.update_search_filter();
                    }
                }
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
                let backup = pending.backup;
                let fallback = fl!("note-title-fallback", index = (self.notes.len() + 1));
                let mut restored = match outcome {
                    SaveOutcome::Conflict {
                        disk_content,
                        disk_mtime,
                        ..
                    } => {
                        let mut note = NoteItem::from_loaded(
                            backup.id,
                            backup.path,
                            &disk_content,
                            &fallback,
                            Some(disk_mtime),
                        );
                        note.save = backup.save;
                        note.save.resolve_conflict(revision);
                        note
                    }
                    SaveOutcome::Saved(mtime) => {
                        let mut note = NoteItem::from_loaded(
                            backup.id,
                            backup.path,
                            &backup.plain_text,
                            &fallback,
                            mtime.or(backup.last_mtime),
                        );
                        note.filename = backup.filename;
                        note.title = backup.title;
                        note.title_lower = note.title.to_lowercase();
                        note.save = backup.save;
                        note.save.finish(revision, true);
                        note
                    }
                    SaveOutcome::Failed => {
                        let mut note = NoteItem::from_loaded(
                            backup.id,
                            backup.path,
                            &backup.plain_text,
                            &fallback,
                            backup.last_mtime,
                        );
                        note.filename = backup.filename;
                        note.title = backup.title;
                        note.title_lower = note.title.to_lowercase();
                        note.save = backup.save;
                        note.save.finish(revision, false);
                        note
                    }
                };
                if restored.save.has_failed() {
                    restored.last_edit_time = None;
                }
                self.notes.push(restored);
                if self
                    .undo_cache
                    .as_ref()
                    .is_some_and(|(_, p, _)| *p == pending.path)
                {
                    self.undo_cache = None;
                }
                self.update_search_filter();
                self.notice_banner = Some(NoticeBanner::Error(fl!("banner-delete-failed")));
            }
        }
        app::Task::none()
    }

    fn select_note_internal(&mut self, target: usize) -> app::Task<Message> {
        if target < self.notes.len() && target != self.active_index {
            let prev_idx = self.active_index;
            self.set_active(target);
            self.is_searching = false;
            return app::Task::batch([self.save_note_async(prev_idx), self.focus_editor()]);
        }
        if self.is_searching {
            self.is_searching = false;
        }
        self.focus_editor()
    }

    fn handle_escape(&mut self) -> app::Task<Message> {
        ALTGR_ACTIVE.store(false, Ordering::SeqCst);
        if self.is_searching {
            self.is_searching = false;
            self.search_query.clear();
            self.update_search_filter();
            return self.focus_editor();
        }

        if let Some(p) = self.popup.take() {
            reset_key_state();
            let save_task = self.save_note_async(self.active_index);
            self.persist_config();
            let destroy_task =
                cosmic::surface::surface_task(cosmic::surface::action::destroy_popup(p));
            return app::Task::batch([save_task, destroy_task]);
        }
        app::Task::none()
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
                    if text.is_empty() {
                        app::Task::none()
                    } else {
                        cosmic::task::message(cosmic::Action::App(Message::EditorAction(
                            Action::Edit(Edit::Paste(Arc::new(text))),
                        )))
                    }
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
        if ui::shortcut_for(&input.key, input.modifiers) == Some(ui::Shortcut::Escape) {
            return app::Task::none();
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

        if let Some(shortcut) = ui::shortcut_for(&input.key, input.modifiers) {
            if shortcut == ui::Shortcut::Escape {
                return app::Task::none();
            }
            return self.update(shortcut_to_message(shortcut));
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

fn load_initial_state(retention_days: u32) -> Result<InitPayload, String> {
    let data_dir = storage::data_dir().map_err(|e| e.to_string())?;
    let trash_dir = data_dir.join("trash");
    storage::tighten_dir_permissions(&data_dir);
    storage::tighten_dir_permissions(&trash_dir);

    if let Err(err) = storage::purge_old_trash(&trash_dir, retention_days, SystemTime::now()) {
        tracing::warn!(?err, ?trash_dir, "Failed to purge expired trash");
    }

    let note_paths = storage::list_notes_in_dir(&data_dir).map_err(|e| e.to_string())?;
    let mut notes = Vec::with_capacity(note_paths.len());
    for path in note_paths {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) => {
                tracing::warn!(?err, ?path, "Failed to read note during init");
                continue;
            }
        };
        let mtime = storage::note_mtime(&path);
        notes.push(LoadedNoteData { path, text, mtime });
    }

    let altgr_map = Arc::new(ui::AltGrMap::load_system());
    Ok(InitPayload {
        data_dir,
        notes,
        altgr_map,
    })
}

fn check_external_note_updates(
    checks: Vec<(NoteId, PathBuf, Option<SystemTime>)>,
) -> Vec<ExternalNoteUpdate> {
    let mut updates = Vec::new();
    for (id, path, last_mtime) in checks {
        match storage::load_note_if_modified(&path, last_mtime) {
            Ok(Some((content, mtime))) => {
                updates.push(ExternalNoteUpdate { id, content, mtime });
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(?err, ?path, id, "Failed to check note for external edits");
            }
        }
    }
    updates
}

fn is_relevant_note_event(data_dir: &Path, ev: &notify::Event) -> bool {
    use notify::EventKind;
    use notify::event::{AccessKind, AccessMode};

    if matches!(
        ev.kind,
        EventKind::Access(
            AccessKind::Open(_)
                | AccessKind::Read
                | AccessKind::Close(AccessMode::Read | AccessMode::Execute)
        )
    ) {
        return false;
    }
    if ev.paths.is_empty() {
        return true;
    }
    let trash_dir = data_dir.join("trash");
    ev.paths.iter().any(|path| {
        if path == &trash_dir || path.strip_prefix(&trash_dir).is_ok() {
            return false;
        }
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                !name.starts_with('.') && !name.contains(".tmp.") && name.ends_with(".md")
            })
    })
}

fn watch_notes_dir(dir: PathBuf) -> Subscription<Message> {
    Subscription::run_with(("notes-watcher", dir), |(_, dir)| {
        let dir = dir.clone();
        cosmic::iced::stream::channel(
            8,
            move |mut out: cosmic::iced::futures::channel::mpsc::Sender<Message>| async move {
                use cosmic::iced::futures::SinkExt;
                use notify::{RecursiveMode, Watcher};

                loop {
                    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                    let watch_dir = dir.clone();
                    let watcher_res = notify::recommended_watcher(move |res| {
                        let _ = tx.send(res);
                    })
                    .and_then(|mut watcher| {
                        watcher.watch(&watch_dir, RecursiveMode::NonRecursive)?;
                        Ok(watcher)
                    });

                    let _watcher = match watcher_res {
                        Ok(watcher) => watcher,
                        Err(err) => {
                            tracing::warn!(?err, ?dir, "Failed to start notes directory watcher");
                            tokio::time::sleep(Duration::from_secs(5)).await;
                            continue;
                        }
                    };

                    loop {
                        let Some(first) = rx.recv().await else {
                            tracing::warn!(
                                ?dir,
                                "Notes directory watcher channel closed; restarting"
                            );
                            break;
                        };
                        let mut relevant = match first {
                            Ok(ev) => is_relevant_note_event(&dir, &ev),
                            Err(err) => {
                                tracing::warn!(?err, ?dir, "Notes directory watcher error");
                                false
                            }
                        };

                        // Coalesce burst events (e.g. atomic write + rename) over 150ms.
                        let deadline = Instant::now() + Duration::from_millis(150);
                        loop {
                            match tokio::time::timeout_at(deadline, rx.recv()).await {
                                Ok(Some(Ok(ev))) => {
                                    if is_relevant_note_event(&dir, &ev) {
                                        relevant = true;
                                    }
                                }
                                Ok(Some(Err(err))) => {
                                    tracing::warn!(?err, ?dir, "Notes directory watcher error");
                                }
                                Ok(None) | Err(_) => break,
                            }
                        }

                        if relevant && out.send(Message::DataDirChanged).await.is_err() {
                            return;
                        }
                    }

                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            },
        )
    })
}

impl cosmic::Application for ScratchpadApp {
    type Executor = cosmic::SingleThreadExecutor;
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
        let (config_handler, config) = load_config();
        let retention_days = config.trash_retention_days;

        let app = Self {
            core,
            config,
            config_handler,
            popup: None,
            data_dir: None,
            notes: Vec::new(),
            notes_loaded: false,
            next_note_id: 0,
            pending_deletes: Vec::new(),
            active_index: 0,
            copied_recently: false,
            copy_generation: 0,
            show_settings: false,
            undo_generation: 0,
            undo_cache: None,
            notice_banner: None,
            is_searching: false,
            search_query: String::new(),
            search_results: Vec::new(),
            altgr_map: Arc::new(ui::AltGrMap::default()),
            editor_id: cosmic::iced::core::widget::Id::unique(),
        };

        let init_task = app::Task::future(async move {
            let result = tokio::task::spawn_blocking(move || load_initial_state(retention_days))
                .await
                .map_err(|err| format!("init task panicked: {err}"))
                .and_then(|res| res);
            cosmic::Action::App(Message::Initialized(result))
        });

        (app, init_task)
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Self::Message> {
        Some(Message::CloseRequested(id))
    }

    fn on_escape(&mut self) -> app::Task<Self::Message> {
        self.handle_escape()
    }

    fn on_app_exit(&mut self) -> Option<Self::Message> {
        for note in &mut self.notes {
            if !note.save.is_saved() {
                match storage::save_note_checked(&note.path, &note.plain_text, note.last_mtime) {
                    Ok(storage::SaveResult::Saved(mtime)) => {
                        if let Some(t) = mtime {
                            note.last_mtime = Some(t);
                        }
                    }
                    Ok(storage::SaveResult::Conflict { conflict_path, .. }) => {
                        tracing::warn!(
                            ?conflict_path,
                            path = ?note.path,
                            "External edit conflict preserved during exit flush"
                        );
                    }
                    Err(err) => {
                        tracing::error!(?err, path = ?note.path, "Failed to flush note on exit");
                    }
                }
            }
        }
        self.persist_config();
        None
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }

    fn update(&mut self, message: Self::Message) -> app::Task<Self::Message> {
        match message {
            Message::Initialized(Ok(payload)) => {
                self.data_dir = Some(payload.data_dir);
                self.altgr_map = payload.altgr_map;
                self.notes = payload
                    .notes
                    .into_iter()
                    .enumerate()
                    .map(|(i, loaded)| {
                        let id = self.alloc_note_id();
                        let fallback = fl!("note-title-fallback", index = (i + 1));
                        NoteItem::from_loaded(
                            id,
                            loaded.path,
                            &loaded.text,
                            &fallback,
                            loaded.mtime,
                        )
                    })
                    .collect();

                let initial_index = self
                    .config
                    .active_file
                    .as_deref()
                    .and_then(|file| self.notes.iter().position(|n| n.filename == file))
                    .unwrap_or(0);
                self.set_active(initial_index);
                self.notes_loaded = true;
                self.update_search_filter();

                if self.popup.is_some() {
                    if self.is_searching {
                        focus_search_input()
                    } else {
                        self.focus_editor()
                    }
                } else {
                    app::Task::none()
                }
            }

            Message::Initialized(Err(err)) => {
                tracing::error!(%err, "Failed to initialize scratchpad storage");
                self.notes_loaded = true;
                self.notice_banner = Some(NoticeBanner::Error(fl!("banner-storage-error")));
                app::Task::none()
            }

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

                let checks = self.collect_clean_note_checks();
                let sync_task = app::Task::future(async move {
                    let res = tokio::task::spawn_blocking(move || {
                        let altgr_map = Arc::new(ui::AltGrMap::load_system());
                        let updates = check_external_note_updates(checks);
                        (altgr_map, updates)
                    })
                    .await;
                    match res {
                        Ok((altgr_map, updates)) => {
                            cosmic::Action::App(Message::PopupSyncDone { altgr_map, updates })
                        }
                        Err(err) => {
                            tracing::error!(?err, "Popup sync task failed");
                            cosmic::Action::None
                        }
                    }
                });

                // Focus is applied from `PopupFocusGained` once the popup surface
                // exists; a focus operation issued here would find no widget tree.
                let popup_task = cosmic::surface::surface_task(cosmic::surface::action::app_popup(
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
                ));

                app::Task::batch([popup_task, sync_task])
            }

            Message::PopupSyncDone { altgr_map, updates } => {
                self.altgr_map = altgr_map;
                self.apply_external_updates(updates);
                app::Task::none()
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
                if !self.notes_loaded {
                    return app::Task::none();
                }
                let Some(dir) = self.data_dir.clone() else {
                    self.notice_banner = Some(NoticeBanner::Error(fl!("banner-storage-error")));
                    return app::Task::none();
                };
                let save_task = self.save_note_async(self.active_index);

                match storage::create_new_note_in_dir(&dir, title_opt.as_deref()) {
                    Ok(new_path) => {
                        let text = std::fs::read_to_string(&new_path).unwrap_or_default();
                        let fallback = fl!("note-title-fallback", index = (self.notes.len() + 1));
                        let id = self.alloc_note_id();
                        self.notes
                            .push(NoteItem::from_text(id, new_path, &text, &fallback));

                        self.set_active(self.notes.len() - 1);
                        self.is_searching = false;
                        self.search_query.clear();
                        self.update_search_filter();
                    }
                    Err(err) => {
                        tracing::error!(?err, "Failed to create new note");
                        self.notice_banner = Some(NoticeBanner::Error(fl!("banner-storage-error")));
                    }
                }

                app::Task::batch([save_task, self.focus_editor()])
            }

            Message::DeleteActiveNote => {
                let Some(target) = self.notes.get(self.active_index) else {
                    return app::Task::none();
                };

                let deleted_idx = self.active_index;
                if target.save.is_in_flight() {
                    let target_note = self.notes.remove(deleted_idx);
                    let deleted_text = Arc::clone(&target_note.plain_text);
                    let deleted_path = target_note.path.clone();
                    // Deleting now would let the running write recreate the file.
                    self.pending_deletes.push(PendingDelete {
                        id: target_note.id,
                        path: target_note.path,
                        backup: DeletedBackup {
                            id: target_note.id,
                            path: deleted_path.clone(),
                            filename: target_note.filename,
                            title: target_note.title,
                            plain_text: Arc::clone(&deleted_text),
                            last_mtime: target_note.last_mtime,
                            save: target_note.save,
                        },
                    });
                    self.undo_generation = self.undo_generation.wrapping_add(1);
                    let generation = self.undo_generation;
                    self.undo_cache = Some((generation, deleted_path, deleted_text));
                } else {
                    if let Err(err) = storage::delete_note(&target.path) {
                        tracing::error!(?err, path = ?target.path, "Failed to delete note");
                        self.notice_banner = Some(NoticeBanner::Error(fl!("banner-delete-failed")));
                        return self.focus_editor();
                    }
                    let target_note = self.notes.remove(deleted_idx);
                    let deleted_text = Arc::clone(&target_note.plain_text);
                    self.undo_generation = self.undo_generation.wrapping_add(1);
                    let generation = self.undo_generation;
                    self.undo_cache = Some((generation, target_note.path, deleted_text));
                }

                let generation = self.undo_generation;
                if self.notes.is_empty()
                    && let Some(dir) = self.data_dir.as_deref()
                {
                    match storage::create_new_note_in_dir(dir, None) {
                        Ok(new_path) => {
                            let fallback = fl!("note-title-fallback", index = 1);
                            let id = self.alloc_note_id();
                            self.notes
                                .push(NoteItem::from_text(id, new_path, "", &fallback));
                        }
                        Err(err) => {
                            tracing::error!(?err, "Failed to create fallback note after delete");
                            self.notice_banner =
                                Some(NoticeBanner::Error(fl!("banner-storage-error")));
                        }
                    }
                }

                let next_idx = self.active_index.min(self.notes.len().saturating_sub(1));
                self.set_active(next_idx);
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
                        let mut note = NoteItem::from_loaded(
                            pending.id,
                            path.clone(),
                            &content_text,
                            &fallback,
                            pending.backup.last_mtime,
                        );
                        note.save = pending.backup.save;
                        note
                    } else {
                        let id = self.alloc_note_id();
                        NoteItem::from_loaded(id, path.clone(), &content_text, &fallback, None)
                    };
                    // The file is gone or about to be; write it back in the background.
                    restored.save.mark_edited();

                    let next_idx = if let Some(pos) = self.notes.iter().position(|n| n.path == path)
                    {
                        self.notes[pos] = restored;
                        pos
                    } else if self.notes.len() == 1
                        && self.notes[0].plain_text.trim().is_empty()
                        && self.notes[0].save.is_saved()
                    {
                        if self.notes[0].path != path
                            && let Err(err) = std::fs::remove_file(&self.notes[0].path)
                            && err.kind() != std::io::ErrorKind::NotFound
                        {
                            tracing::warn!(
                                ?err,
                                path = ?self.notes[0].path,
                                "Failed to remove placeholder note during undo"
                            );
                        }
                        self.notes[0] = restored;
                        0
                    } else {
                        self.notes.push(restored);
                        self.notes.len() - 1
                    };

                    self.set_active(next_idx);
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

            Message::DismissNotice => {
                self.notice_banner = None;
                app::Task::none()
            }

            Message::ToggleSearch => {
                if !self.notes_loaded || self.notes.is_empty() {
                    return app::Task::none();
                }
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
                    if let Some(first) = self.search_results.first() {
                        return self.select_note_internal(first.index);
                    }
                    self.is_searching = false;
                    return app::Task::none();
                }

                // If exact title match exists, select it
                let query_lower = query.to_lowercase();
                if let Some(pos) = self.notes.iter().position(|n| n.title_lower == query_lower) {
                    return self.select_note_internal(pos);
                }

                // If there are partial matches, select the first match
                if let Some(first) = self.search_results.first() {
                    return self.select_note_internal(first.index);
                }

                // Otherwise, create new note with query as title
                self.update(Message::NewNote(Some(query)))
            }

            Message::NoteSaved(id, revision, outcome) => {
                self.handle_note_saved(id, revision, outcome)
            }

            Message::DataDirChanged => {
                let checks = self.collect_clean_note_checks();
                if checks.is_empty() {
                    return app::Task::none();
                }
                app::Task::future(async move {
                    let updates =
                        tokio::task::spawn_blocking(move || check_external_note_updates(checks))
                            .await
                            .unwrap_or_else(|err| {
                                tracing::error!(?err, "External notes check task failed");
                                Vec::new()
                            });
                    cosmic::Action::App(Message::ExternalNotesChecked(updates))
                })
            }

            Message::ExternalNotesChecked(updates) => {
                self.apply_external_updates(updates);
                app::Task::none()
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
                let Some(note) = self.notes.get(self.active_index) else {
                    return app::Task::none();
                };
                let text_to_copy = note.plain_text.to_string();
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

            Message::PasteClipboard => {
                if self.notes.get(self.active_index).is_none() {
                    return app::Task::none();
                }
                let paste_task = cosmic::iced::clipboard::read().and_then(|text| {
                    if text.is_empty() {
                        app::Task::none()
                    } else {
                        cosmic::task::message(cosmic::Action::App(Message::EditorAction(
                            Action::Edit(Edit::Paste(Arc::new(text))),
                        )))
                    }
                });
                app::Task::batch([paste_task, self.focus_editor()])
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

            Message::ConfigUpdated(mut cfg) => {
                cfg.sanitize();
                let prev_file = self.config.active_file.clone();
                self.config = cfg;
                if self.config.active_file != prev_file
                    && let Some(ref target_file) = self.config.active_file
                    && let Some(pos) = self.notes.iter().position(|n| n.filename == *target_file)
                    && pos != self.active_index
                {
                    self.set_active(pos);
                }
                app::Task::none()
            }

            Message::EscapePressed => self.handle_escape(),

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

        let config_sub = self
            .core()
            .watch_config::<ScratchpadConfig>(Self::APP_ID)
            .map(|update| Message::ConfigUpdated(update.config));

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

            let mut subs = vec![keyboard_sub, focus_sub, config_sub];
            if let Some(dir) = &self.data_dir {
                subs.push(watch_notes_dir(dir.clone()));
            }
            Subscription::batch(subs)
        } else {
            Subscription::batch([focus_sub, config_sub])
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

            let maybe_notice_banner: Option<Element<_>> =
                self.notice_banner.as_ref().map(|banner| {
                    let (msg, is_error) = match banner {
                        NoticeBanner::Conflict { filename } => {
                            (fl!("banner-conflict", file = filename.as_str()), false)
                        }
                        NoticeBanner::Error(err) => (err.clone(), true),
                    };
                    let label = if is_error {
                        text::caption(msg)
                            .width(Length::Fill)
                            .class(theme::Text::Custom(|theme| {
                                cosmic::iced::widget::text::Style {
                                    color: Some(theme.cosmic().destructive_text_color().into()),
                                    ..Default::default()
                                }
                            }))
                    } else {
                        text::caption(msg).width(Length::Fill)
                    };
                    container(
                        row![
                            label,
                            button::text(fl!("action-dismiss"))
                                .on_press(Message::DismissNotice)
                                .class(theme::Button::Text)
                                .padding([2, 8]),
                        ]
                        .spacing(6)
                        .align_y(Alignment::Center)
                        .width(Length::Fill),
                    )
                    .padding([4, 8])
                    .class(theme::Container::Card)
                    .into()
                });

            if !self.notes_loaded || self.notes.get(self.active_index).is_none() {
                let mut elements = Vec::with_capacity(2);
                let has_banner = maybe_notice_banner.is_some();
                if let Some(banner) = maybe_notice_banner {
                    elements.push(banner);
                }
                if !self.notes_loaded || !has_banner {
                    let body_msg = if !self.notes_loaded {
                        fl!("status-loading")
                    } else {
                        fl!("banner-storage-error")
                    };
                    elements.push(
                        container(text::body(body_msg))
                            .width(Length::Fill)
                            .height(Length::Fill)
                            .align_x(Alignment::Center)
                            .align_y(Alignment::Center)
                            .into(),
                    );
                }
                let popover_col = column::with_children(elements)
                    .spacing(spacing.space_xs)
                    .padding([8, 12])
                    .width(Length::Fixed(POPUP_WIDTH))
                    .height(Length::Fixed(POPUP_HEIGHT));
                return self.core.applet.popup_container(popover_col).into();
            }

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
                let query_lower = trimmed_query.to_lowercase();
                let has_exact_match = self.notes.iter().any(|n| n.title_lower == query_lower);

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

                for result in &self.search_results {
                    let Some(note) = self.notes.get(result.index) else {
                        continue;
                    };
                    let is_active = result.index == self.active_index;
                    let title_text = text(&note.title).size(13);
                    let count_text = text::caption(format!(
                        "{}/{} · {}w",
                        result.index + 1,
                        self.notes.len(),
                        note.word_chars.0
                    ));
                    let preview_text = text::caption(&result.preview);

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
                    .on_press(Message::SelectNote(result.index))
                    .class(if is_active {
                        theme::Button::Suggested
                    } else {
                        theme::Button::Standard
                    })
                    .width(Length::Fill)
                    .padding([6, 10]);

                    results_items.push(card.into());
                }

                if results_items.is_empty() {
                    results_items.push(
                        container(text::caption(fl!("search-no-results")))
                            .padding(16)
                            .align_x(Alignment::Center)
                            .width(Length::Fill)
                            .into(),
                    );
                }

                let results_col = column::with_children(results_items)
                    .spacing(4)
                    .width(Length::Fill);

                let results_scroll = scrollable(results_col)
                    .height(Length::Fill)
                    .width(Length::Fill);

                let mut search_elements = Vec::with_capacity(3);
                search_elements.push(search_header.into());
                if let Some(banner) = maybe_notice_banner {
                    search_elements.push(banner);
                }
                search_elements.push(results_scroll.into());

                let popover_col = column::with_children(search_elements)
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
                text::caption(format!("{} (Alt+← / Ctrl+[)", fl!("action-prev"))),
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
                text::caption(format!("{} (Alt+→ / Ctrl+])", fl!("action-next"))),
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

            let paste_btn = button::custom(
                icon::from_name("edit-paste-symbolic")
                    .size(16)
                    .symbolic(true),
            )
            .on_press(Message::PasteClipboard)
            .class(theme::Button::Text)
            .padding([4, 6]);

            let paste_with_tooltip = tooltip(
                paste_btn,
                text::caption(format!("{} (Ctrl+V)", fl!("action-paste"))),
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
                paste_with_tooltip,
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
            let editor = text_editor::text_editor(&active_note.content)
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
            let (words, chars) = active_note.word_chars;

            let counter_label = text::caption(fl!("status-count", words = words, chars = chars));

            let save = &active_note.save;
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
            let mut content_elements = Vec::with_capacity(6);
            content_elements.push(header.into());
            if let Some(settings) = maybe_settings_drawer {
                content_elements.push(settings);
            }
            if let Some(notice) = maybe_notice_banner {
                content_elements.push(notice);
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

    fn test_app(data_dir: Option<PathBuf>, notes: Vec<NoteItem>) -> ScratchpadApp {
        let mut app = ScratchpadApp {
            core: cosmic::app::Core::default(),
            config: ScratchpadConfig::default(),
            config_handler: None,
            popup: None,
            data_dir,
            next_note_id: notes.len() as NoteId,
            notes,
            notes_loaded: true,
            pending_deletes: Vec::new(),
            active_index: 0,
            copied_recently: false,
            copy_generation: 0,
            show_settings: false,
            undo_generation: 0,
            undo_cache: None,
            notice_banner: None,
            is_searching: false,
            search_query: String::new(),
            search_results: Vec::new(),
            altgr_map: Arc::new(ui::AltGrMap::default()),
            editor_id: Id::unique(),
        };
        app.set_active(0);
        app.update_search_filter();
        app
    }

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
    fn editor_binding_handles_shortcuts_and_word_motion() {
        let map = ui::AltGrMap::default();
        let ctrl = Modifiers::CTRL;

        assert!(matches!(
            editor_binding(&map, keypress(Key::Named(Named::ArrowLeft), ctrl, None)),
            Some(Binding::Move(Motion::WordLeft))
        ));
        assert!(matches!(
            editor_binding(&map, keypress(Key::Named(Named::ArrowRight), ctrl, None)),
            Some(Binding::Move(Motion::WordRight))
        ));
        assert!(matches!(
            editor_binding(
                &map,
                keypress(Key::Named(Named::ArrowLeft), Modifiers::ALT, None)
            ),
            Some(Binding::Custom(Message::PrevNote))
        ));
        assert!(matches!(
            editor_binding(
                &map,
                keypress(Key::Named(Named::ArrowRight), Modifiers::ALT, None)
            ),
            Some(Binding::Custom(Message::NextNote))
        ));
        assert!(matches!(
            editor_binding(&map, keypress(char_key("f"), ctrl, None)),
            Some(Binding::Custom(Message::ToggleSearch))
        ));
        assert!(matches!(
            editor_binding(&map, keypress(char_key("n"), ctrl, None)),
            Some(Binding::Custom(Message::NewNote(None)))
        ));
        assert!(matches!(
            editor_binding(&map, keypress(char_key("["), ctrl, None)),
            Some(Binding::Custom(Message::PrevNote))
        ));
        assert!(matches!(
            editor_binding(&map, keypress(char_key("]"), ctrl, None)),
            Some(Binding::Custom(Message::NextNote))
        ));
        assert!(matches!(
            editor_binding(&map, keypress(char_key("C"), ctrl | Modifiers::SHIFT, None)),
            Some(Binding::Custom(Message::CopyAll))
        ));
        assert!(matches!(
            editor_binding(
                &map,
                keypress(Key::Named(Named::Escape), Modifiers::empty(), None)
            ),
            Some(Binding::Custom(Message::EscapePressed))
        ));
        // Ctrl+Alt is AltGr on some setups and must not trigger app shortcuts.
        assert!(!matches!(
            editor_binding(&map, keypress(char_key("f"), ctrl | Modifiers::ALT, None)),
            Some(Binding::Custom(_))
        ));
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
    fn search_filter_handles_500_large_notes_and_precomputes_previews() {
        let body_filler = "lorem ipsum dolor sit amet consectetur adipiscing elit ".repeat(185);
        let mut notes = Vec::with_capacity(500);
        for i in 0..500 {
            let text = if i == 42 {
                format!("# Note {i}\n{body_filler}\nUnique match: needle-target-42\n")
            } else if i == 399 {
                format!("# Note {i}\n{body_filler}\nAnother hit: NEEDLE-TARGET-399\n")
            } else {
                format!("# Note {i}\n{body_filler}\n")
            };
            assert!(text.len() >= 10_000);
            notes.push(NoteItem::from_loaded(
                i as NoteId,
                PathBuf::from(format!("pad_{}.md", i + 1)),
                &text,
                &format!("Note {}", i + 1),
                None,
            ));
        }

        let mut app = test_app(None, notes);
        assert_eq!(app.search_results.len(), 500);

        app.search_query = "  needle-target  ".to_string();
        app.update_search_filter();
        assert_eq!(app.search_results.len(), 2);
        assert_eq!(app.search_results[0].index, 42);
        assert_eq!(
            app.search_results[0].preview,
            "Unique match: needle-target-42"
        );
        assert_eq!(app.search_results[1].index, 399);
        assert_eq!(
            app.search_results[1].preview,
            "Another hit: NEEDLE-TARGET-399"
        );

        app.search_query = "needle-target-42".to_string();
        app.update_search_filter();
        assert_eq!(app.search_results.len(), 1);
        assert_eq!(app.search_results[0].index, 42);
    }

    #[test]
    fn handle_note_saved_conflict_reloads_disk_content_and_sets_notice() {
        let mut note = NoteItem::from_loaded(
            0,
            PathBuf::from("/tmp/pad_1.md"),
            "# Local Title\nlocal edit",
            "Note 1",
            None,
        );
        note.save.mark_edited();
        let rev = note.save.begin().unwrap();
        let mut app = test_app(None, vec![note]);

        let disk_mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_123);
        let _task = app.handle_note_saved(
            0,
            rev,
            SaveOutcome::Conflict {
                conflict_path: PathBuf::from("/tmp/pad_1.conflict-123.md"),
                disk_content: "# Remote Title\nupdated body".to_string(),
                disk_mtime,
            },
        );

        let updated = &app.notes[0];
        assert_eq!(updated.plain_text.as_ref(), "# Remote Title\nupdated body");
        assert_eq!(updated.title, "Remote Title");
        assert_eq!(updated.title_lower, "remote title");
        assert_eq!(updated.text_lower, "# remote title\nupdated body");
        assert_eq!(updated.last_mtime, Some(disk_mtime));
        assert!(updated.save.is_saved());
        assert!(!updated.save.has_failed());
        assert!(!updated.save.is_in_flight());
        assert_eq!(
            app.notice_banner,
            Some(NoticeBanner::Conflict {
                filename: "pad_1.conflict-123.md".to_string(),
            })
        );
    }

    #[test]
    fn delete_active_note_failure_preserves_note_and_sets_error_banner() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = tempfile::tempdir().unwrap();
        let note_path = temp_dir.path().join("pad_1.md");
        std::fs::write(&note_path, "# Keep\nbody").unwrap();
        let trash = temp_dir.path().join("trash");
        std::fs::create_dir(&trash).unwrap();
        std::fs::set_permissions(&trash, std::fs::Permissions::from_mode(0o500)).unwrap();

        // Root ignores 0o500 directory permissions.
        let probe = trash.join(".probe");
        if std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
            .is_ok()
        {
            let _ = std::fs::remove_file(&probe);
            std::fs::set_permissions(&trash, std::fs::Permissions::from_mode(0o700)).unwrap();
            return;
        }

        let note = NoteItem::from_text(0, note_path.clone(), "# Keep\nbody", "Note 1");
        let mut app = test_app(Some(temp_dir.path().to_path_buf()), vec![note]);
        let _ = app.update(Message::DeleteActiveNote);

        assert_eq!(app.notes.len(), 1);
        assert_eq!(app.notes[0].path, note_path);
        assert!(app.undo_cache.is_none());
        assert!(matches!(app.notice_banner, Some(NoticeBanner::Error(_))));

        // In-flight delete failure also restores the note cleanly.
        app.notice_banner = None;
        app.notes[0].save.mark_edited();
        let rev = app.notes[0].save.begin().unwrap();
        let _ = app.update(Message::DeleteActiveNote);
        assert_eq!(app.pending_deletes.len(), 1);
        assert!(app.undo_cache.is_some());

        let mtime = storage::note_mtime(&note_path);
        let _ = app.handle_note_saved(0, rev, SaveOutcome::Saved(mtime));

        std::fs::set_permissions(&trash, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(app.pending_deletes.is_empty());
        assert!(app.notes.iter().any(|n| n.path == note_path));
        assert!(app.undo_cache.is_none());
        assert!(matches!(app.notice_banner, Some(NoticeBanner::Error(_))));
    }

    #[test]
    fn uncaptured_and_search_escape_ignore_shortcut_escape() {
        let note = NoteItem::from_loaded(0, PathBuf::from("/tmp/pad_1.md"), "hi", "Note 1", None);
        let mut app = test_app(None, vec![note]);
        let popup_id = window::Id::unique();
        app.popup = Some(popup_id);
        app.is_searching = true;

        let escape_input = KeyInput {
            key: Key::Named(Named::Escape),
            modified_key: Key::Named(Named::Escape),
            physical_key: Physical::Unidentified(
                cosmic::iced::core::keyboard::key::NativeCode::Unidentified,
            ),
            modifiers: Modifiers::empty(),
            text: None,
        };

        let _ = app.handle_uncaptured_key(popup_id, escape_input.clone());
        assert!(app.is_searching);
        assert_eq!(app.popup, Some(popup_id));

        let _ = app.search_key(false, escape_input.clone());
        assert!(app.is_searching);
        assert_eq!(app.popup, Some(popup_id));

        app.is_searching = false;
        let _ = app.handle_uncaptured_key(popup_id, escape_input);
        assert_eq!(app.popup, Some(popup_id));
    }

    #[test]
    fn config_updated_sanitizes_and_switches_active_note() {
        let notes = vec![
            NoteItem::from_loaded(0, PathBuf::from("/tmp/pad_1.md"), "one", "Note 1", None),
            NoteItem::from_loaded(1, PathBuf::from("/tmp/pad_2.md"), "two", "Note 2", None),
        ];
        let mut app = test_app(None, notes);
        assert_eq!(app.active_index, 0);

        let _ = app.update(Message::ConfigUpdated(ScratchpadConfig {
            wrap_lines: false,
            font_size: 99.0,
            active_file: Some("pad_2.md".to_string()),
            trash_retention_days: 9999,
        }));

        assert_eq!(app.config.font_size, 24.0);
        assert_eq!(app.config.trash_retention_days, 3650);
        assert_eq!(app.active_index, 1);
        assert_eq!(app.config.active_file.as_deref(), Some("pad_2.md"));
    }

    #[test]
    fn on_app_exit_flushes_dirty_notes_synchronously() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("pad_1.md");
        std::fs::write(&path, "initial").unwrap();
        let mtime = storage::note_mtime(&path);

        let mut note = NoteItem::from_loaded(0, path.clone(), "initial", "Note 1", mtime);
        note.content = content_at_end("flushed on exit");
        note.plain_text = Arc::from("flushed on exit");
        note.save.mark_edited();

        let mut app = test_app(Some(temp_dir.path().to_path_buf()), vec![note]);
        assert!(app.on_app_exit().is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "flushed on exit");
    }

    #[test]
    fn is_relevant_note_event_filters_trash_tmp_and_read_access() {
        use notify::event::{AccessKind, AccessMode, CreateKind, DataChange, ModifyKind};
        use notify::{Event, EventKind};

        let dir = Path::new("/tmp/scratchpad-notes");

        let note_modify = Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
            .add_path(dir.join("pad_1.md"));
        assert!(is_relevant_note_event(dir, &note_modify));

        let close_write = Event::new(EventKind::Access(AccessKind::Close(AccessMode::Write)))
            .add_path(dir.join("pad_1.md"));
        assert!(is_relevant_note_event(dir, &close_write));

        let read_access =
            Event::new(EventKind::Access(AccessKind::Read)).add_path(dir.join("pad_1.md"));
        assert!(!is_relevant_note_event(dir, &read_access));

        let tmp_create = Event::new(EventKind::Create(CreateKind::File))
            .add_path(dir.join("pad_1.md.tmp.123.0"));
        assert!(!is_relevant_note_event(dir, &tmp_create));

        let trash_create = Event::new(EventKind::Create(CreateKind::File))
            .add_path(dir.join("trash/100_0_pad_1.md"));
        assert!(!is_relevant_note_event(dir, &trash_create));
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
