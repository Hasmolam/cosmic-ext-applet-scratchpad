// SPDX-License-Identifier: GPL-3.0-only

pub mod config;
mod localize;
pub mod storage;
pub mod ui;

use config::{ScratchpadConfig, load_config};
use cosmic::cosmic_config::CosmicConfigEntry;
use cosmic::iced::core::keyboard::{Key, key::Named};
use cosmic::iced::widget::{column, row};
use cosmic::iced::{Alignment, Length, Limits, Subscription, window};
use cosmic::widget::text_editor::{Action, Content};
use cosmic::widget::{
    button, container, icon, scrollable, space, text, text_editor, text_input, tooltip,
};
use cosmic::{Element, app, theme};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use tokio::time::Instant;

pub const APP_ID: &str = "io.github.hasmolam.cosmic-ext-applet-scratchpad";
const POPUP_WIDTH: f32 = 380.0;
const POPUP_HEIGHT: f32 = 440.0;
const DEBOUNCE_MILLIS: u64 = 400;
const UNDO_BANNER_SECS: u64 = 5;

pub fn run() -> cosmic::iced::Result {
    localize::localize();
    cosmic::applet::run::<ScratchpadApp>(())
}

pub struct NoteItem {
    pub path: PathBuf,
    pub filename: String,
    pub title: String,
    pub content: Content,
    pub word_chars: (usize, usize),
    pub last_edit_time: Option<Instant>,
    pub last_mtime: Option<SystemTime>,
    pub saved: bool,
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
    DebounceTimeout(usize),
    NoteSaved(usize, Option<SystemTime>),
    CopyAll,
    ResetCopyStatus(u64),
    ToggleSettings,
    ToggleWrapLines,
    AdjustFontSize(f32),
    EscapePressed,
    Surface(cosmic::surface::Action<Message>),
}

pub struct ScratchpadApp {
    core: cosmic::app::Core,
    config: ScratchpadConfig,
    config_handler: Option<cosmic::cosmic_config::Config>,
    popup: Option<window::Id>,
    notes: Vec<NoteItem>,
    active_index: usize,
    copied_recently: bool,
    copy_generation: u64,
    show_settings: bool,
    undo_generation: u64,
    undo_cache: Option<(u64, PathBuf, String)>,
    is_searching: bool,
    search_query: String,
    filtered_indices: Vec<usize>,
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

    fn save_note_async(&mut self, index: usize) -> app::Task<Message> {
        if let Some(note) = self.notes.get_mut(index)
            && !note.saved
        {
            note.saved = true;
            note.last_edit_time = None;
            let text = note.content.text();
            let path = note.path.clone();
            return app::Task::future(async move {
                let res = tokio::task::spawn_blocking(move || {
                    storage::save_note_atomic(&path, &text).map(|()| storage::note_mtime(&path))
                })
                .await;
                match res {
                    Ok(Ok(mtime)) => cosmic::Action::App(Message::NoteSaved(index, mtime)),
                    Ok(Err(err)) => {
                        tracing::error!(
                            ?err,
                            index,
                            "Failed to save note atomically in background"
                        );
                        cosmic::Action::App(Message::NoteSaved(index, None))
                    }
                    Err(err) => {
                        tracing::error!(?err, index, "Failed to join save task");
                        cosmic::Action::App(Message::NoteSaved(index, None))
                    }
                }
            });
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
            return self.save_note_async(prev_idx);
        }
        app::Task::none()
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
            let filename = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let fallback = fl!("note-title-fallback", index = (i + 1));
            let title = storage::derive_title(&text, &fallback);
            let word_chars = ui::count_words_and_chars(&text);
            let last_mtime = storage::note_mtime(&path);
            let content = Content::with_text(&text);
            notes.push(NoteItem {
                path,
                filename,
                title,
                content,
                word_chars,
                last_edit_time: None,
                last_mtime,
                saved: true,
            });
        }

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
            active_index,
            copied_recently: false,
            copy_generation: 0,
            show_settings: false,
            undo_generation: 0,
            undo_cache: None,
            is_searching: false,
            search_query: String::new(),
            filtered_indices,
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
                if let Some(p) = self.popup.take() {
                    let save_task = self.save_note_async(self.active_index);
                    self.persist_config();
                    let destroy_task =
                        cosmic::surface::surface_task(cosmic::surface::action::destroy_popup(p));
                    return app::Task::batch([save_task, destroy_task]);
                }

                // If opening, check for external disk edits on all notes
                for (i, note) in self.notes.iter_mut().enumerate() {
                    if note.saved
                        && let Ok(Some((reloaded, new_mtime))) =
                            storage::load_note_if_modified(&note.path, note.last_mtime)
                    {
                        let fallback = fl!("note-title-fallback", index = (i + 1));
                        note.title = storage::derive_title(&reloaded, &fallback);
                        note.word_chars = ui::count_words_and_chars(&reloaded);
                        note.content = Content::with_text(&reloaded);
                        note.last_mtime = Some(new_mtime);
                    }
                }
                self.update_search_filter();

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
                let prev_idx = self.active_index;
                let save_task = self.save_note_async(prev_idx);

                let title_ref = title_opt.as_deref();
                match storage::create_new_note(title_ref) {
                    Ok(new_path) => {
                        let filename = new_path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        let text = std::fs::read_to_string(&new_path).unwrap_or_default();
                        let word_chars = ui::count_words_and_chars(&text);
                        let last_mtime = storage::note_mtime(&new_path);
                        let note_idx = self.notes.len() + 1;
                        let fallback = fl!("note-title-fallback", index = note_idx);
                        let title = storage::derive_title(&text, &fallback);

                        self.notes.push(NoteItem {
                            path: new_path,
                            filename,
                            title,
                            content: Content::with_text(&text),
                            word_chars,
                            last_edit_time: None,
                            last_mtime,
                            saved: true,
                        });

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

                save_task
            }

            Message::DeleteActiveNote => {
                if self.notes.is_empty() {
                    return app::Task::none();
                }

                let deleted_idx = self.active_index;
                let target_note = self.notes.remove(deleted_idx);
                let _ = storage::delete_note(&target_note.path);

                self.undo_generation = self.undo_generation.wrapping_add(1);
                let generation = self.undo_generation;
                self.undo_cache = Some((
                    generation,
                    target_note.path.clone(),
                    target_note.content.text(),
                ));

                if self.notes.is_empty()
                    && let Ok(new_path) = storage::create_new_note(None)
                {
                    let filename = new_path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    let fallback = fl!("note-title-fallback", index = 1);
                    self.notes.push(NoteItem {
                        path: new_path.clone(),
                        filename,
                        title: fallback,
                        content: Content::with_text(""),
                        word_chars: (0, 0),
                        last_edit_time: None,
                        last_mtime: storage::note_mtime(&new_path),
                        saved: true,
                    });
                }

                self.active_index = self.active_index.min(self.notes.len().saturating_sub(1));
                self.config.active_tab = self.active_index;
                self.config.active_file = self
                    .notes
                    .get(self.active_index)
                    .map(|n| n.filename.clone());
                self.update_search_filter();

                app::Task::future(async move {
                    tokio::time::sleep(Duration::from_secs(UNDO_BANNER_SECS)).await;
                    cosmic::Action::App(Message::DismissUndoBanner(generation))
                })
            }

            Message::UndoDelete => {
                if let Some((_, path, content_text)) = self.undo_cache.take() {
                    let _ = storage::atomic_write(&path, &content_text);
                    let filename = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    let word_chars = ui::count_words_and_chars(&content_text);
                    let last_mtime = storage::note_mtime(&path);
                    let note_idx = self.notes.len() + 1;
                    let fallback = fl!("note-title-fallback", index = note_idx);
                    let title = storage::derive_title(&content_text, &fallback);

                    let restored = NoteItem {
                        path: path.clone(),
                        filename,
                        title,
                        content: Content::with_text(&content_text),
                        word_chars,
                        last_edit_time: None,
                        last_mtime,
                        saved: true,
                    };

                    if let Some(pos) = self.notes.iter().position(|n| n.path == path) {
                        self.notes[pos] = restored;
                        self.active_index = pos;
                    } else if self.notes.len() == 1
                        && self.notes[0].content.text().trim().is_empty()
                        && self.notes[0].saved
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
                    return text_input::focus(cosmic::iced::core::widget::Id::new(
                        "nv_search_input",
                    ));
                }
                app::Task::none()
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

            Message::NoteSaved(idx, maybe_mtime) => {
                if let Some(note) = self.notes.get_mut(idx)
                    && let Some(mtime) = maybe_mtime
                {
                    note.last_mtime = Some(mtime);
                }
                app::Task::none()
            }

            Message::EditorAction(action) => {
                let idx = self.active_index;
                let is_edit = action.is_edit();
                if let Some(note) = self.notes.get_mut(idx) {
                    note.content.perform(action);
                    if is_edit {
                        note.saved = false;
                        note.last_edit_time = Some(Instant::now());
                    }
                }

                if is_edit {
                    self.update_note_meta(idx);
                    app::Task::future(async move {
                        tokio::time::sleep(Duration::from_millis(DEBOUNCE_MILLIS)).await;
                        cosmic::Action::App(Message::DebounceTimeout(idx))
                    })
                } else {
                    app::Task::none()
                }
            }

            Message::DebounceTimeout(idx) => {
                if let Some(note) = self.notes.get(idx)
                    && let Some(last_time) = note.last_edit_time
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
                if self.is_searching {
                    self.is_searching = false;
                    self.search_query.clear();
                    return app::Task::none();
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
        }
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        if self.popup.is_some() {
            cosmic::iced::keyboard::listen().filter_map(|event| {
                if let cosmic::iced::core::keyboard::Event::KeyPressed { key, modifiers, .. } =
                    event
                {
                    if key
                        == cosmic::iced::core::keyboard::Key::Named(
                            cosmic::iced::core::keyboard::key::Named::Escape,
                        )
                    {
                        Some(Message::EscapePressed)
                    } else if modifiers.command()
                        && (key.as_ref() == Key::Character("f")
                            || key.as_ref() == Key::Character("F")
                            || key.as_ref() == Key::Character("k")
                            || key.as_ref() == Key::Character("K"))
                    {
                        Some(Message::ToggleSearch)
                    } else if modifiers.command()
                        && (key.as_ref() == Key::Character("n")
                            || key.as_ref() == Key::Character("N"))
                    {
                        Some(Message::NewNote(None))
                    } else if modifiers.command()
                        && (key.as_ref() == Key::Character("[")
                            || key == Key::Named(Named::ArrowLeft))
                    {
                        Some(Message::PrevNote)
                    } else if modifiers.command()
                        && (key.as_ref() == Key::Character("]")
                            || key == Key::Named(Named::ArrowRight))
                    {
                        Some(Message::NextNote)
                    } else if modifiers.command()
                        && modifiers.shift()
                        && (key.as_ref() == cosmic::iced::core::keyboard::Key::Character("c")
                            || key.as_ref() == cosmic::iced::core::keyboard::Key::Character("C"))
                    {
                        Some(Message::CopyAll)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
        } else {
            Subscription::none()
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
                        .id(cosmic::iced::core::widget::Id::new("nv_search_input"))
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

            let title_btn = button::text(note_counter_title)
                .on_press(Message::ToggleSearch)
                .class(theme::Button::Text)
                .padding([4, 8]);

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
                title_with_tooltip,
                next_with_tooltip,
                new_with_tooltip,
            ]
            .spacing(2)
            .align_y(Alignment::Center);

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

            let header = row![
                nav_group,
                space::horizontal().width(Length::Fill),
                actions_row
            ]
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
            let editor = text_editor::text_editor(&self.notes[self.active_index].content)
                .placeholder(fl!("placeholder"))
                .size(self.config.font_size)
                .wrapping(if self.config.wrap_lines {
                    cosmic::iced::core::text::Wrapping::Word
                } else {
                    cosmic::iced::core::text::Wrapping::None
                })
                .key_binding(|keypress| {
                    if keypress.key == Key::Named(Named::Escape) {
                        Some(text_editor::Binding::Custom(Message::EscapePressed))
                    } else if keypress.modifiers.command()
                        && (keypress.key.as_ref() == Key::Character("f")
                            || keypress.key.as_ref() == Key::Character("F")
                            || keypress.key.as_ref() == Key::Character("k")
                            || keypress.key.as_ref() == Key::Character("K"))
                    {
                        Some(text_editor::Binding::Custom(Message::ToggleSearch))
                    } else if keypress.modifiers.command()
                        && (keypress.key.as_ref() == Key::Character("n")
                            || keypress.key.as_ref() == Key::Character("N"))
                    {
                        Some(text_editor::Binding::Custom(Message::NewNote(None)))
                    } else if keypress.modifiers.command()
                        && (keypress.key.as_ref() == Key::Character("[")
                            || keypress.key == Key::Named(Named::ArrowLeft))
                    {
                        Some(text_editor::Binding::Custom(Message::PrevNote))
                    } else if keypress.modifiers.command()
                        && (keypress.key.as_ref() == Key::Character("]")
                            || keypress.key == Key::Named(Named::ArrowRight))
                    {
                        Some(text_editor::Binding::Custom(Message::NextNote))
                    } else if keypress.modifiers.command()
                        && keypress.modifiers.shift()
                        && (keypress.key.as_ref() == Key::Character("c")
                            || keypress.key.as_ref() == Key::Character("C"))
                    {
                        Some(text_editor::Binding::Custom(Message::CopyAll))
                    } else {
                        text_editor::Binding::from_key_press(keypress)
                    }
                })
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

            let status_label = if self.notes[self.active_index].saved {
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
