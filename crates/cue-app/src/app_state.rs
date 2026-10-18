//! App-wide state shared by every window.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use cue_core::{Config, ModelInfo};
use gpui_kit::Global;

/// How many recently captured windows to offer as new game profiles.
const RECENT_WINDOWS: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentWindow {
    pub app: String,
    pub title: String,
}

pub struct AppState {
    pub config: Config,
    /// `None` when the platform has no config directory; changes then live in memory only.
    config_path: Option<PathBuf>,
    /// A problem loading the config, shown in the settings window.
    pub load_error: Option<String>,
    /// The models each account listed, by account id, for every model picker.
    pub models: HashMap<String, Vec<ModelInfo>>,
    /// Windows captured during this run, newest first.
    pub recent_windows: Vec<RecentWindow>,
}

impl Global for AppState {}

impl AppState {
    pub fn load() -> Self {
        let (config, config_path, load_error) = match Config::default_path() {
            Err(err) => (Config::default(), None, Some(err.to_string())),
            Ok(path) => match Config::load_from(&path) {
                Ok(config) => (config, Some(path), None),
                // Keep the path so fixing the config in settings overwrites the broken file.
                Err(err) => (Config::default(), Some(path), Some(err.to_string())),
            },
        };
        Self {
            config,
            config_path,
            load_error,
            models: HashMap::new(),
            recent_windows: Vec::new(),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = self
            .config_path
            .as_ref()
            .context("there's no config directory")?;
        self.config.save_to(path).map_err(|err| {
            tracing::error!("couldn't save the config: {err}");
            err.into()
        })
    }

    pub fn remember_window(&mut self, app: &str, title: &str) {
        let window = RecentWindow {
            app: app.to_owned(),
            title: title.to_owned(),
        };
        self.recent_windows.retain(|recent| recent != &window);
        self.recent_windows.insert(0, window);
        self.recent_windows.truncate(RECENT_WINDOWS);
    }
}
