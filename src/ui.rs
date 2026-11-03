// SPDX-License-Identifier: GPL-3.0-only

use cosmic::iced::core::keyboard::{
    Key,
    key::{Code, Named, Physical},
};
use std::collections::HashMap;
use std::path::Path;
use xkbcommon::xkb;

/// Returns true if the key event corresponds to AltGr (`ISO_Level3_Shift` / Right Alt).
pub fn is_altgr_key(key: &Key, physical: Physical) -> bool {
    matches!(key, Key::Named(Named::AltGraph)) || matches!(physical, Physical::Code(Code::AltRight))
}

/// Precomputed lookup table mapping `(Physical KeyCode, shift)` to `(normal_char, altgr_char)`
/// for the user's active XKB keyboard layout.
///
/// Workaround for `cosmic-panel` resetting `ISO_Level3_Shift` (`Mod5`) in `wl_keyboard::modifiers`,
/// which otherwise causes `AltGr + key` combinations to emit unmodified Level 1/2 characters.
#[derive(Debug, Clone, Default)]
pub struct AltGrMap {
    entries: HashMap<(Code, bool), (Option<char>, char)>,
}

impl AltGrMap {
    /// Loads the active XKB configuration from COSMIC (`com.system76.CosmicComp`),
    /// environment variables, or system keyboard config files.
    pub fn load_system() -> Self {
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
            Some(km) => Self::from_keymap(&km),
            None => Self::default(),
        }
    }

    fn from_keymap(keymap: &xkb::Keymap) -> Self {
        let mut entries = HashMap::new();
        let num_groups = keymap.num_layouts().max(1);

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

        for &(code, scancode) in MAPPED_KEYS {
            let xkb_keycode = xkb::Keycode::new(scancode + 8);
            for shift in [false, true] {
                let active_shift = if shift { shift_bit } else { 0 };

                for group in 0..num_groups {
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
                        entries.insert((code, shift), (normal_char, ch));
                        break;
                    }
                }
            }
        }

        Self { entries }
    }

    /// Resolves `(normal_char, altgr_char)` for a physical key and Shift state.
    pub fn resolve(&self, physical: Physical, shift: bool) -> Option<(Option<char>, char)> {
        match physical {
            Physical::Code(code) => self.entries.get(&(code, shift)).copied(),
            Physical::Unidentified(_) => None,
        }
    }
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

const MAPPED_KEYS: &[(Code, u32)] = &[
    (Code::Digit1, 2),
    (Code::Digit2, 3),
    (Code::Digit3, 4),
    (Code::Digit4, 5),
    (Code::Digit5, 6),
    (Code::Digit6, 7),
    (Code::Digit7, 8),
    (Code::Digit8, 9),
    (Code::Digit9, 10),
    (Code::Digit0, 11),
    (Code::Minus, 12),
    (Code::Equal, 13),
    (Code::KeyQ, 16),
    (Code::KeyW, 17),
    (Code::KeyE, 18),
    (Code::KeyR, 19),
    (Code::KeyT, 20),
    (Code::KeyY, 21),
    (Code::KeyU, 22),
    (Code::KeyI, 23),
    (Code::KeyO, 24),
    (Code::KeyP, 25),
    (Code::BracketLeft, 26),
    (Code::BracketRight, 27),
    (Code::KeyA, 30),
    (Code::KeyS, 31),
    (Code::KeyD, 32),
    (Code::KeyF, 33),
    (Code::KeyG, 34),
    (Code::KeyH, 35),
    (Code::KeyJ, 36),
    (Code::KeyK, 37),
    (Code::KeyL, 38),
    (Code::Semicolon, 39),
    (Code::Quote, 40),
    (Code::Backquote, 41),
    (Code::Backslash, 43),
    (Code::KeyZ, 44),
    (Code::KeyX, 45),
    (Code::KeyC, 46),
    (Code::KeyV, 47),
    (Code::KeyB, 48),
    (Code::KeyN, 49),
    (Code::KeyM, 50),
    (Code::Comma, 51),
    (Code::Period, 52),
    (Code::Slash, 53),
    (Code::Space, 57),
    (Code::IntlBackslash, 86),
    (Code::IntlRo, 89),
    (Code::IntlYen, 124),
];

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
}
