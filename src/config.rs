// SPDX-License-Identifier: GPL-3.0-only

use cosmic::cosmic_config::{
    self, Config, CosmicConfigEntry, cosmic_config_derive::CosmicConfigEntry,
};
use serde::{Deserialize, Serialize};

pub const CONFIG_ID: &str = "io.github.hasmolam.cosmic-ext-applet-scratchpad";

#[derive(Debug, Clone, Serialize, Deserialize, CosmicConfigEntry, PartialEq)]
#[version = 1]
pub struct ScratchpadConfig {
    pub active_tab: usize,
    pub wrap_lines: bool,
    pub font_size: f32,
    pub tab_names: [String; 3],
}

impl Default for ScratchpadConfig {
    fn default() -> Self {
        Self {
            active_tab: 0,
            wrap_lines: true,
            font_size: 14.0,
            tab_names: [
                "Notes".to_string(),
                "Snippets".to_string(),
                "Scratch".to_string(),
            ],
        }
    }
}

impl ScratchpadConfig {
    /// Sanitizes configuration values ensuring active_tab and font_size are within valid bounds.
    pub fn sanitize(&mut self) {
        self.active_tab = self
            .active_tab
            .min(crate::storage::TOTAL_PADS.saturating_sub(1));
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
    }

    #[test]
    fn test_config_active_tab_bounds() {
        let mut config = ScratchpadConfig {
            active_tab: 5,
            ..Default::default()
        };
        config.sanitize();
        assert_eq!(config.active_tab, 2);
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
