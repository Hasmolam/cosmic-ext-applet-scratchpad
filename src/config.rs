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

pub fn load_config() -> (Option<Config>, ScratchpadConfig) {
    match Config::new(CONFIG_ID, ScratchpadConfig::VERSION) {
        Ok(handler) => {
            let config = match ScratchpadConfig::get_entry(&handler) {
                Ok(cfg) => cfg,
                Err(err) => {
                    tracing::debug!(?err, "Could not read config entry, using defaults");
                    ScratchpadConfig::default()
                }
            };
            (Some(handler), config)
        }
        Err(err) => {
            tracing::debug!(?err, "Could not open cosmic-config handler");
            (None, ScratchpadConfig::default())
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
        let config = ScratchpadConfig {
            active_tab: 5,
            ..Default::default()
        };
        let clamped = config.active_tab.min(crate::storage::TOTAL_PADS - 1);
        assert_eq!(clamped, 2);
    }
}
