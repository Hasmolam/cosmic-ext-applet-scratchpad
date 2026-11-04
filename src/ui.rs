// SPDX-License-Identifier: GPL-3.0-only

use cosmic::cctk::wayland_client::{
    Connection, Dispatch, QueueHandle, WEnum,
    protocol::{wl_keyboard, wl_registry, wl_seat},
};
use cosmic::iced::core::keyboard::{
    Key,
    key::{Code, Named, NativeCode, Physical},
};
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use xkbcommon::xkb;

/// Returns true if the key event corresponds to AltGr (`ISO_Level3_Shift` / Right Alt).
///
/// Note: `libcosmic`'s Wayland backend calls `raw_keycode_to_physicalkey(ke.raw_code)` which
/// subtracts 8 from the evdev scancode (`100 - 8 = 92 => Code::Convert` for `AltRight`).
pub fn is_altgr_key(key: &Key, physical: Physical) -> bool {
    matches!(key, Key::Named(Named::AltGraph))
        || matches!(physical, Physical::Code(Code::Convert | Code::AltRight))
}

/// Precomputed lookup table for the user's active XKB keyboard layout.
///
/// Workaround for two upstream issues in COSMIC applets:
/// 1. `cosmic-panel` resets `ISO_Level3_Shift` (`Mod5`) in `wl_keyboard::modifiers`, causing
///    `AltGr + key` combinations to emit unmodified Level 1/2 characters.
/// 2. `libcosmic`'s `sctk_event.rs` passes evdev `raw_code` (without `+ 8`) to
///    `raw_keycode_to_physicalkey`, which calls `saturating_sub(8)`. As a result, evdev
///    scancodes `2..=8` (`Digit1..=Digit7`) all report `Physical::Unidentified(Xkb(0))` and
///    scancodes `>= 9` report the `Physical::Code` of `scancode - 8`.
#[derive(Debug, Clone, Default)]
pub struct AltGrMap {
    /// Maps `(evdev_scancode, shift)` -> `(normal_char, altgr_char)` for all keys.
    by_scancode: HashMap<(u32, bool), (Option<char>, char)>,
    /// Maps `(normal_char, shift)` -> `(Option<char>, char)` for `Digit1..=Digit7` (evdev `2..=8`),
    /// where `libcosmic` collapses `physical_key` to `Unidentified(Xkb(0))`.
    low_scancodes_by_char: HashMap<(char, bool), (Option<char>, char)>,
    /// Fallback mapping `(normal_char, shift)` -> `(Option<char>, char)` across all keys.
    by_char: HashMap<(char, bool), (Option<char>, char)>,
}

impl AltGrMap {
    /// Loads the active XKB keymap directly from the Wayland compositor (`wl_keyboard::keymap`),
    /// falling back to COSMIC (`com.system76.CosmicComp`), environment variables, or system files.
    pub fn load_system() -> Self {
        if let Some((keymap, active_group)) = fetch_wayland_keymap() {
            let map = Self::from_keymap(&keymap, active_group);
            if !map.by_scancode.is_empty() {
                return map;
            }
        }

        let rmlvo = detect_system_rmlvo();
        Self::from_rmlvo(
            &rmlvo.rules,
            &rmlvo.model,
            &rmlvo.layout,
            &rmlvo.variant,
            rmlvo.options.as_deref(),
        )
    }

    /// Builds an `AltGrMap` for the specified XKB RMLVO parameters.
    pub fn from_rmlvo(
        rules: &str,
        model: &str,
        layout: &str,
        variant: &str,
        options: Option<&str>,
    ) -> Self {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_names(
            &context,
            rules,
            model,
            layout,
            variant,
            options.map(str::to_string),
            xkb::COMPILE_NO_FLAGS,
        )
        .or_else(|| {
            xkb::Keymap::new_from_names(&context, "", "", "", "", None, xkb::COMPILE_NO_FLAGS)
        });

        match keymap {
            Some(km) => Self::from_keymap(&km, 0),
            None => Self::default(),
        }
    }

    fn from_keymap(keymap: &xkb::Keymap, preferred_group: u32) -> Self {
        let mut by_scancode = HashMap::new();
        let mut low_scancodes_by_char = HashMap::new();
        let mut by_char = HashMap::new();
        let num_groups = keymap.num_layouts().max(1);

        let mut group_order = Vec::with_capacity(num_groups as usize);
        if preferred_group < num_groups {
            group_order.push(preferred_group);
        }
        for g in 0..num_groups {
            if g != preferred_group {
                group_order.push(g);
            }
        }

        let shift_idx = keymap.mod_get_index(xkb::MOD_NAME_SHIFT);
        let shift_bit = if shift_idx != xkb::MOD_INVALID {
            1u32 << shift_idx
        } else {
            0
        };

        let mut level3_bits = 0u32;
        for mod_name in ["Mod5", "LevelThree", "ISO_Level3_Shift"] {
            let idx = keymap.mod_get_index(mod_name);
            if idx != xkb::MOD_INVALID {
                level3_bits |= 1u32 << idx;
            }
        }

        for &scancode in MAPPED_EVDEV_SCANCODES {
            let xkb_keycode = xkb::Keycode::new(scancode + 8);
            for shift in [false, true] {
                let active_shift = if shift { shift_bit } else { 0 };

                for &group in &group_order {
                    let mut normal_state = xkb::State::new(keymap);
                    normal_state.update_mask(active_shift, 0, 0, 0, 0, group);
                    let normal_char = extract_xkb_char(&normal_state, xkb_keycode);

                    let mut altgr_state = xkb::State::new(keymap);
                    if level3_bits != 0 {
                        altgr_state.update_mask(level3_bits | active_shift, 0, 0, 0, 0, group);
                    } else {
                        altgr_state.update_mask(active_shift, 0, 0, 0, 0, group);
                        altgr_state.update_key(xkb::Keycode::new(108), xkb::KeyDirection::Down);
                    }
                    let altgr_char = extract_xkb_char(&altgr_state, xkb_keycode);

                    if let Some(ch) = altgr_char
                        && Some(ch) != normal_char
                    {
                        by_scancode.insert((scancode, shift), (normal_char, ch));
                        if let Some(norm) = normal_char {
                            if (2..=8).contains(&scancode) {
                                low_scancodes_by_char.insert((norm, shift), (normal_char, ch));
                            }
                            by_char.entry((norm, shift)).or_insert((normal_char, ch));
                        }
                        break;
                    }
                }
            }
        }

        Self {
            by_scancode,
            low_scancodes_by_char,
            by_char,
        }
    }

    /// Resolves `(normal_char, altgr_char)` for a Wayland key press in `libcosmic`.
    pub fn resolve_key(
        &self,
        key: &Key,
        physical: Physical,
        shift: bool,
    ) -> Option<(Option<char>, char)> {
        let key_char = match key.as_ref() {
            Key::Character(s) => s.chars().next(),
            _ => None,
        };

        // 1. If `physical` carries a non-zero scancode (evdev scancode >= 9, shifted by -8 in libcosmic)
        if let Some(evdev_scancode) = wayland_physical_to_evdev_scancode(physical) {
            if let Some(&(normal_ch, altgr_ch)) = self.by_scancode.get(&(evdev_scancode, shift))
                && (chars_match_ci(key_char, normal_ch)
                    || key_char.is_none()
                    || key_char == Some(altgr_ch))
            {
                return Some((normal_ch, altgr_ch));
            }
            // Fallback in case a future libcosmic update removes the -8 scancode offset
            if let Some(unshifted) = unshifted_code_to_evdev_scancode(physical)
                && let Some(&(normal_ch, altgr_ch)) = self.by_scancode.get(&(unshifted, shift))
                && (chars_match_ci(key_char, normal_ch) || key_char == Some(altgr_ch))
            {
                return Some((normal_ch, altgr_ch));
            }
            return None;
        }

        // 2. For evdev scancodes 2..=8 (Digit1..=Digit7), libcosmic reports Unidentified(Xkb(0))
        if let Some(ch) = key_char {
            if let Some(&res) = self.low_scancodes_by_char.get(&(ch, shift)) {
                return Some(res);
            }
            let lower = ch.to_lowercase().next().unwrap_or(ch);
            if let Some(&res) = self.low_scancodes_by_char.get(&(lower, shift)) {
                return Some(res);
            }
            if let Some(&res) = self.by_char.get(&(ch, shift)) {
                return Some(res);
            }
        }

        None
    }

    /// Resolves `(normal_char, altgr_char)` directly by unshifted `Code` (for unit tests).
    pub fn resolve(&self, physical: Physical, shift: bool) -> Option<(Option<char>, char)> {
        let scancode = unshifted_code_to_evdev_scancode(physical)?;
        self.by_scancode.get(&(scancode, shift)).copied()
    }
}

fn chars_match_ci(a: Option<char>, b: Option<char>) -> bool {
    match (a, b) {
        (Some(ca), Some(cb)) => ca == cb || ca.to_lowercase().eq(cb.to_lowercase()),
        _ => false,
    }
}

#[derive(Default)]
struct WaylandKeymapState {
    keymap_string: Option<String>,
    active_group: u32,
}

impl Dispatch<wl_registry::WlRegistry, ()> for WaylandKeymapState {
    fn event(
        _state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
            && interface == "wl_seat"
        {
            let seat = registry.bind::<wl_seat::WlSeat, _, _>(name, version.min(5), qh, ());
            let _keyboard = seat.get_keyboard(qh, ());
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for WaylandKeymapState {
    fn event(
        _state: &mut Self,
        _seat: &wl_seat::WlSeat,
        _event: wl_seat::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for WaylandKeymapState {
    fn event(
        state: &mut Self,
        _keyboard: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Keymap {
                format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1),
                fd,
                size,
            } => {
                let mut file = std::fs::File::from(fd);
                let len = size.saturating_sub(1) as usize;
                let mut buf = vec![0u8; len];
                if file.read_exact(&mut buf).is_ok() {
                    if let Some(nul_pos) = buf.iter().position(|&b| b == 0) {
                        buf.truncate(nul_pos);
                    }
                    if let Ok(s) = String::from_utf8(buf) {
                        state.keymap_string = Some(s);
                    }
                }
            }
            wl_keyboard::Event::Modifiers { group, .. } => {
                state.active_group = group;
            }
            _ => {}
        }
    }
}

fn fetch_wayland_keymap() -> Option<(xkb::Keymap, u32)> {
    let conn = Connection::connect_to_env().ok()?;
    let display = conn.display();
    let mut event_queue = conn.new_event_queue::<WaylandKeymapState>();
    let qh = event_queue.handle();
    let _registry = display.get_registry(&qh, ());

    let mut state = WaylandKeymapState::default();
    event_queue.roundtrip(&mut state).ok()?;
    if state.keymap_string.is_none() {
        event_queue.roundtrip(&mut state).ok()?;
    }

    let keymap_str = state.keymap_string?;
    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let keymap = xkb::Keymap::new_from_string(
        &context,
        keymap_str,
        xkb::KEYMAP_FORMAT_TEXT_V1,
        xkb::COMPILE_NO_FLAGS,
    )?;
    Some((keymap, state.active_group))
}

fn extract_xkb_char(state: &xkb::State, keycode: xkb::Keycode) -> Option<char> {
    let utf8 = state.key_get_utf8(keycode);
    if let Some(c) = utf8.chars().find(|c| !c.is_control()) {
        return Some(c);
    }

    // Fallback for dead keys commonly bound to Level 3/4 (e.g. dead_tilde on AltGr+4 / AltGr+])
    match state.key_get_one_sym(keycode).raw() {
        0xfe50 => Some('`'), // XKB_KEY_dead_grave
        0xfe51 => Some('´'), // XKB_KEY_dead_acute
        0xfe52 => Some('^'), // XKB_KEY_dead_circumflex
        0xfe53 => Some('~'), // XKB_KEY_dead_tilde
        0xfe54 => Some('¯'), // XKB_KEY_dead_macron
        0xfe55 => Some('˘'), // XKB_KEY_dead_breve
        0xfe56 => Some('˙'), // XKB_KEY_dead_abovedot
        0xfe57 => Some('¨'), // XKB_KEY_dead_diaeresis
        0xfe58 => Some('˚'), // XKB_KEY_dead_abovering
        0xfe59 => Some('˝'), // XKB_KEY_dead_doubleacute
        0xfe5a => Some('ˇ'), // XKB_KEY_dead_caron
        0xfe5b => Some('¸'), // XKB_KEY_dead_cedilla
        0xfe5c => Some('˛'), // XKB_KEY_dead_ogonek
        _ => None,
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct RmlvoConfig {
    rules: String,
    model: String,
    layout: String,
    variant: String,
    options: Option<String>,
}

fn detect_system_rmlvo() -> RmlvoConfig {
    if let Some(config_dir) = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
    {
        let cosmic_xkb_path = config_dir
            .join("cosmic")
            .join("com.system76.CosmicComp")
            .join("v1")
            .join("xkb_config");
        if let Ok(content) = std::fs::read_to_string(&cosmic_xkb_path)
            && let Some(cfg) = parse_cosmic_xkb_ron(&content)
            && !cfg.layout.is_empty()
        {
            return cfg;
        }
    }

    if let Ok(layout) = std::env::var("XKB_DEFAULT_LAYOUT")
        && !layout.trim().is_empty()
    {
        return RmlvoConfig {
            rules: std::env::var("XKB_DEFAULT_RULES").unwrap_or_default(),
            model: std::env::var("XKB_DEFAULT_MODEL").unwrap_or_default(),
            layout,
            variant: std::env::var("XKB_DEFAULT_VARIANT").unwrap_or_default(),
            options: std::env::var("XKB_DEFAULT_OPTIONS")
                .ok()
                .filter(|s| !s.is_empty()),
        };
    }

    for path in ["/etc/vconsole.conf", "/etc/default/keyboard"] {
        if let Ok(content) = std::fs::read_to_string(Path::new(path))
            && let Some(cfg) = parse_key_value_keyboard_config(&content)
            && !cfg.layout.is_empty()
        {
            return cfg;
        }
    }

    RmlvoConfig::default()
}

fn parse_cosmic_xkb_ron(content: &str) -> Option<RmlvoConfig> {
    let mut cfg = RmlvoConfig::default();
    let mut found_any = false;

    for line in content.lines() {
        let trimmed = line.trim().trim_end_matches(',');
        if let Some((key, raw_val)) = trimmed.split_once(':') {
            let key = key.trim();
            let val = raw_val.trim();
            match key {
                "rules" => {
                    cfg.rules = unquote_ron_str(val);
                    found_any = true;
                }
                "model" => {
                    cfg.model = unquote_ron_str(val);
                    found_any = true;
                }
                "layout" => {
                    cfg.layout = unquote_ron_str(val);
                    found_any = true;
                }
                "variant" => {
                    cfg.variant = unquote_ron_str(val);
                    found_any = true;
                }
                "options" => {
                    if let Some(inner) = val.strip_prefix("Some(").and_then(|s| s.strip_suffix(')'))
                    {
                        let opt = unquote_ron_str(inner);
                        if !opt.is_empty() {
                            cfg.options = Some(opt);
                        }
                    }
                    found_any = true;
                }
                _ => {}
            }
        }
    }

    found_any.then_some(cfg)
}

fn parse_key_value_keyboard_config(content: &str) -> Option<RmlvoConfig> {
    let mut cfg = RmlvoConfig::default();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((key, raw_val)) = trimmed.split_once('=') {
            let val = raw_val.trim().trim_matches('"').trim_matches('\'');
            match key.trim() {
                "XKBLAYOUT" => cfg.layout = val.to_string(),
                "XKBMODEL" => cfg.model = val.to_string(),
                "XKBVARIANT" => cfg.variant = val.to_string(),
                "XKBOPTIONS" if !val.is_empty() => cfg.options = Some(val.to_string()),
                _ => {}
            }
        }
    }
    (!cfg.layout.is_empty()).then_some(cfg)
}

fn unquote_ron_str(s: &str) -> String {
    s.trim().trim_matches('"').to_string()
}

const MAPPED_EVDEV_SCANCODES: &[u32] = &[
    2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 30, 31,
    32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 57, 86, 89,
    124,
];

/// Converts `libcosmic` Wayland's `Physical` key (which went through `raw_code.saturating_sub(8)`)
/// back to the original Linux evdev scancode (`raw_code = shifted + 8`).
fn wayland_physical_to_evdev_scancode(physical: Physical) -> Option<u32> {
    let shifted = unshifted_code_to_evdev_scancode(physical)?;
    (shifted > 0).then_some(shifted + 8)
}

/// Exact inverse of `scancode_to_physicalkey` in `libcosmic/iced/winit/.../wayland/keymap.rs`.
fn unshifted_code_to_evdev_scancode(physical: Physical) -> Option<u32> {
    let code = match physical {
        Physical::Code(code) => code,
        Physical::Unidentified(NativeCode::Xkb(raw)) => return (raw > 0).then_some(raw),
        _ => return None,
    };
    let scancode = match code {
        Code::Escape => 1,
        Code::Digit1 => 2,
        Code::Digit2 => 3,
        Code::Digit3 => 4,
        Code::Digit4 => 5,
        Code::Digit5 => 6,
        Code::Digit6 => 7,
        Code::Digit7 => 8,
        Code::Digit8 => 9,
        Code::Digit9 => 10,
        Code::Digit0 => 11,
        Code::Minus => 12,
        Code::Equal => 13,
        Code::Backspace => 14,
        Code::Tab => 15,
        Code::KeyQ => 16,
        Code::KeyW => 17,
        Code::KeyE => 18,
        Code::KeyR => 19,
        Code::KeyT => 20,
        Code::KeyY => 21,
        Code::KeyU => 22,
        Code::KeyI => 23,
        Code::KeyO => 24,
        Code::KeyP => 25,
        Code::BracketLeft => 26,
        Code::BracketRight => 27,
        Code::Enter => 28,
        Code::ControlLeft => 29,
        Code::KeyA => 30,
        Code::KeyS => 31,
        Code::KeyD => 32,
        Code::KeyF => 33,
        Code::KeyG => 34,
        Code::KeyH => 35,
        Code::KeyJ => 36,
        Code::KeyK => 37,
        Code::KeyL => 38,
        Code::Semicolon => 39,
        Code::Quote => 40,
        Code::Backquote => 41,
        Code::ShiftLeft => 42,
        Code::Backslash => 43,
        Code::KeyZ => 44,
        Code::KeyX => 45,
        Code::KeyC => 46,
        Code::KeyV => 47,
        Code::KeyB => 48,
        Code::KeyN => 49,
        Code::KeyM => 50,
        Code::Comma => 51,
        Code::Period => 52,
        Code::Slash => 53,
        Code::ShiftRight => 54,
        Code::NumpadMultiply => 55,
        Code::AltLeft => 56,
        Code::Space => 57,
        Code::CapsLock => 58,
        Code::F1 => 59,
        Code::F2 => 60,
        Code::F3 => 61,
        Code::F4 => 62,
        Code::F5 => 63,
        Code::F6 => 64,
        Code::F7 => 65,
        Code::F8 => 66,
        Code::F9 => 67,
        Code::F10 => 68,
        Code::NumLock => 69,
        Code::ScrollLock => 70,
        Code::Numpad7 => 71,
        Code::Numpad8 => 72,
        Code::Numpad9 => 73,
        Code::NumpadSubtract => 74,
        Code::Numpad4 => 75,
        Code::Numpad5 => 76,
        Code::Numpad6 => 77,
        Code::NumpadAdd => 78,
        Code::Numpad1 => 79,
        Code::Numpad2 => 80,
        Code::Numpad3 => 81,
        Code::Numpad0 => 82,
        Code::NumpadDecimal => 83,
        Code::Lang5 => 85,
        Code::IntlBackslash => 86,
        Code::F11 => 87,
        Code::F12 => 88,
        Code::IntlRo => 89,
        Code::Lang3 => 90,
        Code::Lang4 => 91,
        Code::Convert => 92,
        Code::KanaMode => 93,
        Code::NonConvert => 94,
        Code::NumpadEnter => 96,
        Code::ControlRight => 97,
        Code::NumpadDivide => 98,
        Code::PrintScreen => 99,
        Code::AltRight => 100,
        Code::Home => 102,
        Code::ArrowUp => 103,
        Code::PageUp => 104,
        Code::ArrowLeft => 105,
        Code::ArrowRight => 106,
        Code::End => 107,
        Code::ArrowDown => 108,
        Code::PageDown => 109,
        Code::Insert => 110,
        Code::Delete => 111,
        Code::AudioVolumeMute => 113,
        Code::AudioVolumeDown => 114,
        Code::AudioVolumeUp => 115,
        Code::NumpadEqual => 117,
        Code::Pause => 119,
        Code::NumpadComma => 121,
        Code::Lang1 => 122,
        Code::Lang2 => 123,
        Code::IntlYen => 124,
        Code::SuperLeft => 125,
        Code::SuperRight => 126,
        Code::ContextMenu => 127,
        _ => return None,
    };
    Some(scancode)
}

/// Calculates word and character count for any UTF-8 text string.
pub fn count_words_and_chars(text: &str) -> (usize, usize) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return (0, 0);
    }
    let words = trimmed.split_whitespace().count();
    let chars = text.chars().count();
    (words, chars)
}

/// Extracts a clean preview snippet from note content.
/// If `query` is non-empty, finds the line matching query.
/// Otherwise returns the second line (after title) or first line.
pub fn get_preview_snippet(text: &str, query: &str) -> String {
    let query_lower = query.trim().to_lowercase();
    if !query_lower.is_empty() {
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.to_lowercase().contains(&query_lower) {
                let cleaned = trimmed.trim_start_matches('#').trim();
                let chars: Vec<char> = cleaned.chars().collect();
                return if chars.len() > 42 {
                    let s: String = chars.into_iter().take(42).collect();
                    format!("{}…", s.trim_end())
                } else {
                    cleaned.to_string()
                };
            }
        }
    }

    let mut non_empty_lines = text
        .lines()
        .map(|l| l.trim().trim_start_matches('#').trim())
        .filter(|l| !l.is_empty());

    let _title_line = non_empty_lines.next();
    if let Some(body_line) = non_empty_lines.next() {
        let chars: Vec<char> = body_line.chars().collect();
        if chars.len() > 42 {
            let s: String = chars.into_iter().take(42).collect();
            format!("{}…", s.trim_end())
        } else {
            body_line.to_string()
        }
    } else {
        "…".to_string()
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn test_word_and_char_counter_empty() {
        assert_eq!(count_words_and_chars(""), (0, 0));
        assert_eq!(count_words_and_chars("   \n\t  "), (0, 0));
    }

    #[test]
    fn test_word_and_char_counter_unicode_and_multiline() {
        assert_eq!(count_words_and_chars("hello world"), (2, 11));
        assert_eq!(
            count_words_and_chars("Merhaba dünya! Şekerli çay."),
            (4, 27)
        );
        assert_eq!(
            count_words_and_chars("curl -X POST https://api.com/v1\nAuthorization: Bearer xyz"),
            (7, 57)
        );
    }

    #[test]
    fn test_get_preview_snippet_matching_query() {
        let text = "# Meeting\nDiscuss architecture\nAction item: buy milk";
        assert_eq!(get_preview_snippet(text, "milk"), "Action item: buy milk");
    }

    #[test]
    fn test_get_preview_snippet_empty_or_fallback() {
        let text = "# Only Title";
        assert_eq!(get_preview_snippet(text, ""), "…");

        let multiline = "# Title\nFirst line of body text here";
        assert_eq!(
            get_preview_snippet(multiline, ""),
            "First line of body text here"
        );
    }

    #[test]
    fn test_altgr_map_spanish_layout() {
        let map = AltGrMap::from_rmlvo("", "", "es", "", Some("lv3:ralt_switch"));
        // AltGr+1 => '|', AltGr+2 => '@', AltGr+3 => '#', AltGr+4 => '~'
        assert_eq!(
            map.resolve(Physical::Code(Code::Digit1), false),
            Some((Some('1'), '|'))
        );
        assert_eq!(
            map.resolve(Physical::Code(Code::Digit2), false),
            Some((Some('2'), '@'))
        );
        assert_eq!(
            map.resolve(Physical::Code(Code::Digit3), false),
            Some((Some('3'), '#'))
        );
        assert_eq!(
            map.resolve(Physical::Code(Code::Digit4), false),
            Some((Some('4'), '~'))
        );
        assert_eq!(
            map.resolve(Physical::Code(Code::BracketLeft), false)
                .map(|(_, c)| c),
            Some('[')
        );
        assert_eq!(
            map.resolve(Physical::Code(Code::BracketRight), false)
                .map(|(_, c)| c),
            Some(']')
        );
    }

    #[test]
    fn test_altgr_map_turkish_and_german_layouts() {
        let tr_map = AltGrMap::from_rmlvo("", "", "tr", "", Some("lv3:ralt_switch"));
        assert_eq!(
            tr_map.resolve(Physical::Code(Code::KeyQ), false),
            Some((Some('q'), '@'))
        );
        assert_eq!(
            tr_map.resolve(Physical::Code(Code::Digit3), false),
            Some((Some('3'), '#'))
        );
        assert_eq!(
            tr_map.resolve(Physical::Code(Code::Digit7), false),
            Some((Some('7'), '{'))
        );
        assert_eq!(
            tr_map.resolve(Physical::Code(Code::Digit8), false),
            Some((Some('8'), '['))
        );
        assert_eq!(
            tr_map.resolve(Physical::Code(Code::Digit9), false),
            Some((Some('9'), ']'))
        );
        assert_eq!(
            tr_map.resolve(Physical::Code(Code::Digit0), false),
            Some((Some('0'), '}'))
        );
        assert_eq!(
            tr_map
                .resolve(Physical::Code(Code::BracketRight), false)
                .map(|(_, c)| c),
            Some('~')
        );

        let de_map = AltGrMap::from_rmlvo("", "", "de", "", Some("lv3:ralt_switch"));
        assert_eq!(
            de_map.resolve(Physical::Code(Code::KeyQ), false),
            Some((Some('q'), '@'))
        );
        assert_eq!(
            de_map
                .resolve(Physical::Code(Code::BracketRight), false)
                .map(|(_, c)| c),
            Some('~')
        );
    }

    #[test]
    fn test_parse_cosmic_xkb_ron() {
        let ron = r#"(
    rules: "",
    model: "",
    layout: "es",
    variant: "",
    options: Some("lv3:ralt_switch,compose:rctrl"),
    repeat_delay: 600,
    repeat_rate: 25,
)"#;
        let parsed = parse_cosmic_xkb_ron(ron).expect("should parse RON");
        assert_eq!(parsed.layout, "es");
        assert_eq!(
            parsed.options.as_deref(),
            Some("lv3:ralt_switch,compose:rctrl")
        );
    }

    #[test]
    fn test_resolve_key_with_libcosmic_wayland_shifted_scancodes() {
        use cosmic::iced::core::keyboard::key::NativeCode;

        let es_map = AltGrMap::from_rmlvo("", "", "es", "", Some("lv3:ralt_switch"));
        let unidentified_zero = Physical::Unidentified(NativeCode::Xkb(0));

        // In libcosmic Wayland, Digit2 (evdev 3) and Digit3 (evdev 4) saturate to Xkb(0)
        assert_eq!(
            es_map.resolve_key(&Key::Character("2".into()), unidentified_zero, false),
            Some((Some('2'), '@'))
        );
        assert_eq!(
            es_map.resolve_key(&Key::Character("3".into()), unidentified_zero, false),
            Some((Some('3'), '#'))
        );
        assert_eq!(
            es_map.resolve_key(&Key::Character("4".into()), unidentified_zero, false),
            Some((Some('4'), '~'))
        );

        let tr_map = AltGrMap::from_rmlvo("", "", "tr", "", Some("lv3:ralt_switch"));
        // Digit7 (evdev 8) -> 8 - 8 = 0 => Unidentified(Xkb(0))
        assert_eq!(
            tr_map.resolve_key(&Key::Character("7".into()), unidentified_zero, false),
            Some((Some('7'), '{'))
        );
        // Digit8 (evdev 9) -> 9 - 8 = 1 => Code::Escape
        assert_eq!(
            tr_map.resolve_key(
                &Key::Character("8".into()),
                Physical::Code(Code::Escape),
                false
            ),
            Some((Some('8'), '['))
        );
        // KeyQ (evdev 16) -> 16 - 8 = 8 => Code::Digit7
        assert_eq!(
            tr_map.resolve_key(
                &Key::Character("q".into()),
                Physical::Code(Code::Digit7),
                false
            ),
            Some((Some('q'), '@'))
        );

        // Exhaustively verify every mapped scancode across es, tr, de, fr, se layouts
        for layout in ["es", "tr", "de", "fr", "se"] {
            let map = AltGrMap::from_rmlvo("", "", layout, "", None);
            assert!(
                !map.by_scancode.is_empty(),
                "layout {layout} should not be empty"
            );
            for (&(evdev_scancode, shift), &(normal_ch, expected_altgr)) in &map.by_scancode {
                let shifted = evdev_scancode.saturating_sub(8);
                let wayland_physical = sim_libcosmic_scancode_to_physical(shifted);
                let key = match normal_ch {
                    Some(c) => Key::Character(c.to_string().into()),
                    None => Key::Unidentified,
                };
                if shifted > 0 || normal_ch.is_some() {
                    let resolved = map
                        .resolve_key(&key, wayland_physical, shift)
                        .map(|(_, ch)| ch);
                    assert_eq!(
                        resolved,
                        Some(expected_altgr),
                        "failed for layout={layout} evdev={evdev_scancode} shift={shift} normal={normal_ch:?}"
                    );
                }
            }
        }
    }

    fn sim_libcosmic_scancode_to_physical(scancode: u32) -> Physical {
        if scancode == 0 {
            return Physical::Unidentified(NativeCode::Xkb(0));
        }
        for code in [
            Code::Escape,
            Code::Digit1,
            Code::Digit2,
            Code::Digit3,
            Code::Digit4,
            Code::Digit5,
            Code::Digit6,
            Code::Digit7,
            Code::Digit8,
            Code::Digit9,
            Code::Digit0,
            Code::Minus,
            Code::Equal,
            Code::Backspace,
            Code::Tab,
            Code::KeyQ,
            Code::KeyW,
            Code::KeyE,
            Code::KeyR,
            Code::KeyT,
            Code::KeyY,
            Code::KeyU,
            Code::KeyI,
            Code::KeyO,
            Code::KeyP,
            Code::BracketLeft,
            Code::BracketRight,
            Code::Enter,
            Code::ControlLeft,
            Code::KeyA,
            Code::KeyS,
            Code::KeyD,
            Code::KeyF,
            Code::KeyG,
            Code::KeyH,
            Code::KeyJ,
            Code::KeyK,
            Code::KeyL,
            Code::Semicolon,
            Code::Quote,
            Code::Backquote,
            Code::ShiftLeft,
            Code::Backslash,
            Code::KeyZ,
            Code::KeyX,
            Code::KeyC,
            Code::KeyV,
            Code::KeyB,
            Code::KeyN,
            Code::KeyM,
            Code::Comma,
            Code::Period,
            Code::Slash,
            Code::ShiftRight,
            Code::NumpadMultiply,
            Code::AltLeft,
            Code::Space,
            Code::CapsLock,
            Code::F1,
            Code::F2,
            Code::F3,
            Code::F4,
            Code::F5,
            Code::F6,
            Code::F7,
            Code::F8,
            Code::F9,
            Code::F10,
            Code::NumLock,
            Code::ScrollLock,
            Code::Numpad7,
            Code::Numpad8,
            Code::Numpad9,
            Code::NumpadSubtract,
            Code::Numpad4,
            Code::Numpad5,
            Code::Numpad6,
            Code::NumpadAdd,
            Code::Numpad1,
            Code::Numpad2,
            Code::Numpad3,
            Code::Numpad0,
            Code::NumpadDecimal,
            Code::Lang5,
            Code::IntlBackslash,
            Code::F11,
            Code::F12,
            Code::IntlRo,
            Code::Lang3,
            Code::Lang4,
            Code::Convert,
            Code::KanaMode,
            Code::NonConvert,
            Code::NumpadEnter,
            Code::ControlRight,
            Code::NumpadDivide,
            Code::PrintScreen,
            Code::AltRight,
            Code::Home,
            Code::ArrowUp,
            Code::PageUp,
            Code::ArrowLeft,
            Code::ArrowRight,
            Code::End,
            Code::ArrowDown,
            Code::PageDown,
            Code::Insert,
            Code::Delete,
            Code::AudioVolumeMute,
            Code::AudioVolumeDown,
            Code::AudioVolumeUp,
            Code::NumpadEqual,
            Code::Pause,
            Code::NumpadComma,
            Code::Lang1,
            Code::Lang2,
            Code::IntlYen,
            Code::SuperLeft,
            Code::SuperRight,
            Code::ContextMenu,
        ] {
            let phys = Physical::Code(code);
            if unshifted_code_to_evdev_scancode(phys) == Some(scancode) {
                return phys;
            }
        }
        Physical::Unidentified(NativeCode::Xkb(scancode))
    }
}
