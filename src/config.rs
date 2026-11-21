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

fn default_trash_retention_days() -> u32 {
    30
}

#[derive(Debug, Clone, Serialize, Deserialize, CosmicConfigEntry, PartialEq)]
#[version = 1]
pub struct ScratchpadConfig {
    // Wave 2 removes active_tab alongside src/lib.rs updates
    #[serde(default)]
    pub active_tab: usize,
    #[serde(default = "default_wrap_lines")]
    pub wrap_lines: bool,
    #[serde(default = "default_font_size")]
    pub font_size: f32,
    #[serde(default)]
    pub active_file: Option<String>,
    #[serde(default = "default_trash_retention_days")]
    pub trash_retention_days: u32,
}

impl Default for ScratchpadConfig {
    fn default() -> Self {
        Self {
            active_tab: 0,
            wrap_lines: default_wrap_lines(),
            font_size: default_font_size(),
            active_file: None,
            trash_retention_days: default_trash_retention_days(),
        }
    }
}

impl ScratchpadConfig {
    /// Sanitizes configuration values within valid bounds.
    pub fn sanitize(&mut self) {
        if self.font_size.is_nan() || self.font_size < 10.0 || self.font_size > 24.0 {
            self.font_size = if self.font_size.is_nan() {
                14.0
            } else {
                self.font_size.clamp(10.0, 24.0)
            };
        }
        self.trash_retention_days = self.trash_retention_days.min(3650);
    }
}

pub fn load_config() -> (Option<Config>, ScratchpadConfig) {
    match Config::new(CONFIG_ID, ScratchpadConfig::VERSION) {
        Ok(handler) => {
            let mut config = match ScratchpadConfig::get_entry(&handler) {
                Ok(cfg) => cfg,
                Err((errs, partial)) => {
                    for e in errs {
                        if e.is_err() {
                            tracing::warn!(%e, "config");
                        }
                    }
                    partial
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
        assert_eq!(deserialized.active_file, None);
        assert_eq!(deserialized.trash_retention_days, 30);
    }

    #[test]
    fn test_config_legacy_json_with_tab_names_deserializes() {
        let legacy = r#"{
            "active_tab": 2,
            "wrap_lines": false,
            "font_size": 16.0,
            "tab_names": ["Notes", "Snippets", "Scratch"],
            "active_file": "pad_3.md"
        }"#;
        let cfg: ScratchpadConfig =
            serde_json::from_str(legacy).expect("Legacy config JSON should deserialize");

        assert_eq!(cfg.active_tab, 2);
        assert!(!cfg.wrap_lines);
        assert_eq!(cfg.font_size, 16.0);
        assert_eq!(cfg.active_file.as_deref(), Some("pad_3.md"));
        assert_eq!(cfg.trash_retention_days, 30);
    }

    #[test]
    fn test_config_trash_retention_days_sanitize() {
        let mut cfg_zero = ScratchpadConfig {
            trash_retention_days: 0,
            ..Default::default()
        };
        cfg_zero.sanitize();
        assert_eq!(cfg_zero.trash_retention_days, 0);

        let mut cfg_normal = ScratchpadConfig {
            trash_retention_days: 90,
            ..Default::default()
        };
        cfg_normal.sanitize();
        assert_eq!(cfg_normal.trash_retention_days, 90);

        let mut cfg_huge = ScratchpadConfig {
            trash_retention_days: 10_000,
            ..Default::default()
        };
        cfg_huge.sanitize();
        assert_eq!(cfg_huge.trash_retention_days, 3650);
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
