// SPDX-License-Identifier: GPL-3.0-only

use cosmic::cosmic_config::{
    self, Config, CosmicConfigEntry, cosmic_config_derive::CosmicConfigEntry,
};
use serde::{Deserialize, Serialize};

pub const CONFIG_ID: &str = "io.github.hasmolam.cosmic-ext-applet-scratchpad";

#[derive(Debug, Clone, Serialize, Deserialize, CosmicConfigEntry, PartialEq, Eq)]
#[version = 1]
pub struct ScratchpadConfig {
    pub active_tab: usize,
    pub wrap_lines: bool,
}

impl Default for ScratchpadConfig {
    fn default() -> Self {
        Self {
            active_tab: 0,
            wrap_lines: true,
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
