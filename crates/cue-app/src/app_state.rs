//! App-wide state shared by every window.

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use cue_core::Config;
use gpui_kit::Global;

pub struct AppState {
    pub config: Config,
    /// `None` when the platform has no config directory; changes then live in memory only.
    config_path: Option<PathBuf>,
    /// A problem loading the config, shown in the settings window.
    pub load_error: Option<String>,
}

impl Global for AppState {}

impl AppState {
    pub fn load() -> Self {
        let path = match Config::default_path() {
            Ok(path) => path,
            Err(err) => {
                return Self {
                    config: Config::default(),
                    config_path: None,
                    load_error: Some(err.to_string()),
                };
            }
        };
        match Config::load_from(&path) {
            Ok(config) => Self {
                config,
                config_path: Some(path),
                load_error: None,
            },
            // Keep the path so fixing the config in settings overwrites the broken file.
            Err(err) => Self {
                config: Config::default(),
                config_path: Some(path),
                load_error: Some(err.to_string()),
            },
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = self
            .config_path
            .as_ref()
            .context("there's no config directory")?;
        Ok(self.config.save_to(path)?)
    }
}
