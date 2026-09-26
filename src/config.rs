// SPDX-License-Identifier: GPL-3.0-only

use cosmic::cosmic_config::{
    self, Config, CosmicConfigEntry, cosmic_config_derive::CosmicConfigEntry,
};
use serde::{Deserialize, Serialize};

pub const CONFIG_ID: &str = "io.github.hasmolam.cosmic-ext-applet-scratchpad";

fn default_wrap_lines() -> bool {
    true
}

fn default_font_size() -> f32 {
    14.0
}

fn default_tab_names() -> [String; 3] {
    [
        "Notes".to_string(),
        "Snippets".to_string(),
        "Scratch".to_string(),
    ]
}

#[derive(Debug, Clone, Serialize, Deserialize, CosmicConfigEntry, PartialEq)]
#[version = 1]
pub struct ScratchpadConfig {
    #[serde(default)]
    pub active_tab: usize,
    #[serde(default = "default_wrap_lines")]
    pub wrap_lines: bool,
    #[serde(default = "default_font_size")]
    pub font_size: f32,
    #[serde(default = "default_tab_names")]
    pub tab_names: [String; 3],
    #[serde(default)]
    pub active_file: Option<String>,
}

impl Default for ScratchpadConfig {
    fn default() -> Self {
        Self {
            active_tab: 0,
            wrap_lines: default_wrap_lines(),
            font_size: default_font_size(),
            tab_names: default_tab_names(),
            active_file: None,
        }
    }
}

impl ScratchpadConfig {
    /// Sanitizes configuration values ensuring font_size is within valid bounds.
    pub fn sanitize(&mut self) {
        if self.font_size.is_nan() || self.font_size < 10.0 || self.font_size > 24.0 {
            self.font_size = if self.font_size.is_nan() {
                14.0
            } else {
                self.font_size.clamp(10.0, 24.0)
            };
        }
    }
}

pub fn load_config() -> (Option<Config>, ScratchpadConfig) {
    match Config::new(CONFIG_ID, ScratchpadConfig::VERSION) {
        Ok(handler) => {
            let mut config = match ScratchpadConfig::get_entry(&handler) {
                Ok(cfg) => cfg,
                Err(err) => {
                    tracing::debug!(?err, "Could not read config entry, using defaults");
                    ScratchpadConfig::default()
                }
            };
            config.sanitize();
            (Some(handler), config)
        }
        Err(err) => {
            tracing::debug!(?err, "Could not open cosmic-config handler");
            let mut config = ScratchpadConfig::default();
            config.sanitize();
            (None, config)
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn test_config_default_serialization() {
        let default_config = ScratchpadConfig::default();
        let serialized = serde_json::to_string(&default_config)
            .expect("Failed to serialize default ScratchpadConfig");
        let deserialized: ScratchpadConfig =
            serde_json::from_str(&serialized).expect("Failed to deserialize ScratchpadConfig");

        assert_eq!(default_config, deserialized);
        assert_eq!(deserialized.active_tab, 0);
        assert!(deserialized.wrap_lines);
        assert_eq!(deserialized.tab_names.len(), 3);
        assert_eq!(deserialized.active_file, None);
    }

    #[test]
    fn test_config_font_size_bounds_and_nan() {
        let mut config_nan = ScratchpadConfig {
            font_size: f32::NAN,
            ..Default::default()
        };
        config_nan.sanitize();
        assert_eq!(config_nan.font_size, 14.0);

        let mut config_zero = ScratchpadConfig {
            font_size: 0.0,
            ..Default::default()
        };
        config_zero.sanitize();
        assert_eq!(config_zero.font_size, 10.0);

        let mut config_huge = ScratchpadConfig {
            font_size: 99.0,
            ..Default::default()
        };
        config_huge.sanitize();
        assert_eq!(config_huge.font_size, 24.0);
    }
}
