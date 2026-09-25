// SPDX-License-Identifier: GPL-3.0-only

pub mod config;
mod localize;
pub mod storage;
pub mod ui;

use config::{ScratchpadConfig, load_config};
use cosmic::cosmic_config::CosmicConfigEntry;
use cosmic::iced::widget::{column, row};
use cosmic::iced::{Alignment, Length, Limits, Subscription, window};
use cosmic::widget::text_editor::{Action, Content};
use cosmic::widget::{button, container, icon, space, text, text_editor};
use cosmic::{Element, app, theme};
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

#[derive(Debug, Clone)]
pub enum Message {
    TogglePopup,
    CloseRequested(window::Id),
    SelectTab(usize),
    EditorAction(Action),
    DebounceTimeout(usize),
    CopyAll,
    ResetCopyStatus,
    ClearNote,
    UndoClear,
    DismissUndoBanner,
    ToggleSettings,
    ToggleWrapLines,
    AdjustFontSize(f32),
    Surface(cosmic::surface::Action<Message>),
}

pub struct ScratchpadApp {
    core: cosmic::app::Core,
    config: ScratchpadConfig,
    config_handler: Option<cosmic::cosmic_config::Config>,
    popup: Option<window::Id>,
    active_tab: usize,
    contents: [Content; storage::TOTAL_PADS],
    last_edit_time: [Option<Instant>; storage::TOTAL_PADS],
    last_mtimes: [Option<SystemTime>; storage::TOTAL_PADS],
    saved_status: [bool; storage::TOTAL_PADS],
    copied_recently: bool,
    show_settings: bool,
    undo_cache: Option<(usize, String)>,
}

impl ScratchpadApp {
    fn current_text(&self, index: usize) -> String {
        self.contents[index].text()
    }

    fn save_current_pad_if_dirty(&mut self, index: usize) {
        if !self.saved_status[index] {
            let text = self.current_text(index);
            if let Err(err) = storage::save_pad_atomic(index, &text) {
                tracing::error!(?err, index, "Failed to save pad atomically");
            } else {
                self.saved_status[index] = true;
                self.last_edit_time[index] = None;
                self.last_mtimes[index] = storage::get_pad_mtime(index);
            }
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
        let (config_handler, config) = load_config();
        let active_tab = config.active_tab.min(storage::TOTAL_PADS - 1);

        let mut contents: [Content; storage::TOTAL_PADS] = Default::default();
        let mut last_mtimes: [Option<SystemTime>; storage::TOTAL_PADS] =
            [None; storage::TOTAL_PADS];
        for (i, content) in contents.iter_mut().enumerate() {
            let initial_text = storage::load_pad(i).unwrap_or_default();
            *content = Content::with_text(&initial_text);
            last_mtimes[i] = storage::get_pad_mtime(i);
        }

        let app = Self {
            core,
            config,
            config_handler,
            popup: None,
            active_tab,
            contents,
            last_edit_time: [None; storage::TOTAL_PADS],
            last_mtimes,
            saved_status: [true; storage::TOTAL_PADS],
            copied_recently: false,
            show_settings: false,
            undo_cache: None,
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
                    self.save_current_pad_if_dirty(self.active_tab);
                    return cosmic::surface::surface_task(cosmic::surface::action::destroy_popup(
                        p,
                    ));
                }

                // If opening, check for external disk edits on the active tab
                if self.saved_status[self.active_tab]
                    && let Ok(Some((reloaded, new_mtime))) = storage::load_pad_if_modified(
                        self.active_tab,
                        self.last_mtimes[self.active_tab],
                    )
                {
                    self.contents[self.active_tab] = Content::with_text(&reloaded);
                    self.last_mtimes[self.active_tab] = Some(new_mtime);
                }

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
                    self.save_current_pad_if_dirty(self.active_tab);
                    self.popup = None;
                }
                app::Task::none()
            }

            Message::Surface(a) => cosmic::task::message(cosmic::Action::Surface(a)),

            Message::SelectTab(index) => {
                if index < storage::TOTAL_PADS && index != self.active_tab {
                    self.save_current_pad_if_dirty(self.active_tab);
                    self.active_tab = index;
                    self.config.active_tab = index;

                    // Check for external disk edits on the newly selected tab
                    if self.saved_status[index]
                        && let Ok(Some((reloaded, new_mtime))) =
                            storage::load_pad_if_modified(index, self.last_mtimes[index])
                    {
                        self.contents[index] = Content::with_text(&reloaded);
                        self.last_mtimes[index] = Some(new_mtime);
                    }

                    if let Some(handler) = &self.config_handler
                        && let Err(err) = self.config.write_entry(handler)
                    {
                        tracing::warn!(?err, "Could not persist active_tab config");
                    }
                }
                app::Task::none()
            }

            Message::EditorAction(action) => {
                let tab = self.active_tab;
                let is_edit = action.is_edit();
                self.contents[tab].perform(action);

                if is_edit {
                    self.saved_status[tab] = false;
                    self.last_edit_time[tab] = Some(Instant::now());

                    app::Task::future(async move {
                        tokio::time::sleep(Duration::from_millis(DEBOUNCE_MILLIS)).await;
                        cosmic::Action::App(Message::DebounceTimeout(tab))
                    })
                } else {
                    app::Task::none()
                }
            }

            Message::DebounceTimeout(tab) => {
                if let Some(last_time) = self.last_edit_time[tab]
                    && last_time.elapsed() >= Duration::from_millis(DEBOUNCE_MILLIS - 50)
                {
                    self.save_current_pad_if_dirty(tab);
                }
                app::Task::none()
            }

            Message::CopyAll => {
                let text_to_copy = self.current_text(self.active_tab);
                self.copied_recently = true;

                let copy_task = cosmic::iced::clipboard::write(text_to_copy);
                let reset_task = app::Task::future(async move {
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                    cosmic::Action::App(Message::ResetCopyStatus)
                });

                app::Task::batch([copy_task, reset_task])
            }

            Message::ResetCopyStatus => {
                self.copied_recently = false;
                app::Task::none()
            }

            Message::ClearNote => {
                let tab = self.active_tab;
                let previous_text = self.current_text(tab);
                if !previous_text.is_empty() {
                    self.undo_cache = Some((tab, previous_text));
                    self.contents[tab] = Content::with_text("");
                    self.save_current_pad_if_dirty(tab);

                    app::Task::future(async move {
                        tokio::time::sleep(Duration::from_secs(UNDO_BANNER_SECS)).await;
                        cosmic::Action::App(Message::DismissUndoBanner)
                    })
                } else {
                    app::Task::none()
                }
            }

            Message::UndoClear => {
                if let Some((tab, text)) = self.undo_cache.take() {
                    self.contents[tab] = Content::with_text(&text);
                    self.save_current_pad_if_dirty(tab);
                }
                app::Task::none()
            }

            Message::DismissUndoBanner => {
                self.undo_cache = None;
                app::Task::none()
            }

            Message::ToggleSettings => {
                self.show_settings = !self.show_settings;
                app::Task::none()
            }

            Message::ToggleWrapLines => {
                self.config.wrap_lines = !self.config.wrap_lines;
                if let Some(handler) = &self.config_handler
                    && let Err(err) = self.config.write_entry(handler)
                {
                    tracing::warn!(?err, "Could not persist wrap_lines config");
                }
                app::Task::none()
            }

            Message::AdjustFontSize(delta) => {
                let new_size = (self.config.font_size + delta).clamp(10.0, 24.0);
                if (new_size - self.config.font_size).abs() > f32::EPSILON {
                    self.config.font_size = new_size;
                    if let Some(handler) = &self.config_handler
                        && let Err(err) = self.config.write_entry(handler)
                    {
                        tracing::warn!(?err, "Could not persist font_size config");
                    }
                }
                app::Task::none()
            }
        }
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        Subscription::none()
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

            // 1. Header: Pill Tabs (Left) and Quick Action Buttons (Right)
            let tab_names = [
                if self.config.tab_names[0] == "Notes" {
                    fl!("tab-notes")
                } else {
                    self.config.tab_names[0].clone()
                },
                if self.config.tab_names[1] == "Snippets" {
                    fl!("tab-snippets")
                } else {
                    self.config.tab_names[1].clone()
                },
                if self.config.tab_names[2] == "Scratch" {
                    fl!("tab-scratch")
                } else {
                    self.config.tab_names[2].clone()
                },
            ];

            let mut tab_buttons = Vec::with_capacity(storage::TOTAL_PADS);
            for (i, name) in tab_names.into_iter().enumerate() {
                let is_selected = i == self.active_tab;
                let btn = button::text(name)
                    .on_press(Message::SelectTab(i))
                    .class(if is_selected {
                        theme::Button::Suggested
                    } else {
                        theme::Button::Text
                    })
                    .padding([4, 10]);
                tab_buttons.push(btn.into());
            }

            let tabs_row = row::with_children(tab_buttons).spacing(4);

            let copy_icon = if self.copied_recently {
                icon::from_name("emblem-ok-symbolic")
                    .size(16)
                    .symbolic(true)
            } else {
                icon::from_name("edit-copy-symbolic")
                    .size(16)
                    .symbolic(true)
            };

            let copy_btn = button::custom(copy_icon)
                .on_press(Message::CopyAll)
                .class(if self.copied_recently {
                    theme::Button::Suggested
                } else {
                    theme::Button::Text
                })
                .padding([4, 6]);

            let clear_btn = button::custom(
                icon::from_name("edit-clear-symbolic")
                    .size(16)
                    .symbolic(true),
            )
            .on_press(Message::ClearNote)
            .class(theme::Button::Text)
            .padding([4, 6]);

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

            let actions_row = row![copy_btn, clear_btn, settings_btn].spacing(4);

            let header = row![
                tabs_row,
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
            let maybe_undo_banner: Option<Element<_>> = if let Some((tab, _)) = &self.undo_cache {
                if *tab == self.active_tab {
                    Some(
                        container(
                            row![
                                text::caption(fl!("banner-cleared")),
                                space::horizontal().width(Length::Fill),
                                button::text(fl!("action-undo"))
                                    .on_press(Message::UndoClear)
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
                }
            } else {
                None
            };

            // 4. Text Editor with Font Size & Line Wrapping
            let editor = text_editor::text_editor(&self.contents[self.active_tab])
                .placeholder(fl!("placeholder"))
                .size(self.config.font_size)
                .wrapping(if self.config.wrap_lines {
                    cosmic::iced::core::text::Wrapping::Word
                } else {
                    cosmic::iced::core::text::Wrapping::None
                })
                .height(Length::Fill)
                .padding(10)
                .on_action(Message::EditorAction);

            let editor_container = container(editor)
                .width(Length::Fill)
                .height(Length::Fill)
                .class(theme::Container::Card);

            // 5. Footer: Word & Character Counter (Left), Save Status (Right)
            let current_text = self.current_text(self.active_tab);
            let (words, chars) = ui::count_words_and_chars(&current_text);

            let counter_label = text::caption(fl!("status-count", words = words, chars = chars));

            let status_label = if self.saved_status[self.active_tab] {
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
