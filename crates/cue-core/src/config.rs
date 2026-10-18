use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::games::{GameProfile, find_profile};
use crate::provider::{ModelRef, ProviderAccount};
use crate::recommend::Tier;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not determine a config directory for this platform")]
    NoConfigDir,
    #[error("could not read or write {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("invalid config file {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("could not serialize config: {0}")]
    Serialize(#[from] toml::ser::Error),
}

/// Global shortcuts, written in the `CmdOrCtrl+Shift+Space` style.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hotkeys {
    /// Capture the game and ask with the fast model.
    pub ask: String,
    /// Capture the game and ask with the deep model.
    pub ask_deep: String,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            ask: "CmdOrCtrl+Shift+Space".into(),
            ask_deep: "CmdOrCtrl+Shift+Enter".into(),
        }
    }
}

/// Everything Cue persists, except API keys, which live in the OS keychain.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub accounts: Vec<ProviderAccount>,
    pub fast_model: Option<ModelRef>,
    pub deep_model: Option<ModelRef>,
    pub hotkeys: Hotkeys,
    pub games: Vec<GameProfile>,
}

impl Config {
    /// Default location, e.g. `~/Library/Application Support/io.github.Admiralkheir.Cue/config.toml`.
    pub fn default_path() -> Result<PathBuf, ConfigError> {
        ProjectDirs::from("io.github", "Admiralkheir", "Cue")
            .map(|dirs| dirs.config_dir().join("config.toml"))
            .ok_or(ConfigError::NoConfigDir)
    }

    /// Loads the config, or returns the default when the file doesn't exist yet.
    pub fn load_from(path: &Path) -> Result<Self, ConfigError> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => {
                return Err(ConfigError::Io {
                    path: path.into(),
                    source,
                });
            }
        };
        toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.into(),
            source,
        })
    }

    /// Writes through a temporary file so a crash never leaves a half-written config.
    pub fn save_to(&self, path: &Path) -> Result<(), ConfigError> {
        let io_err = |source| ConfigError::Io {
            path: path.into(),
            source,
        };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(io_err)?;
        }
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, toml::to_string_pretty(self)?).map_err(io_err)?;
        fs::rename(&tmp, path).map_err(io_err)
    }

    pub fn model_for(&self, tier: Tier) -> Option<&ModelRef> {
        match tier {
            Tier::Fast => self.fast_model.as_ref(),
            Tier::Deep => self.deep_model.as_ref(),
        }
    }

    pub fn account(&self, id: &str) -> Option<&ProviderAccount> {
        self.accounts.iter().find(|account| account.id == id)
    }

    /// The profile for a captured window, if the player made one.
    pub fn game_for(&self, app_name: &str, title: &str) -> Option<&GameProfile> {
        find_profile(&self.games, app_name, title)
    }

    pub fn game(&self, id: &str) -> Option<&GameProfile> {
        self.games.iter().find(|game| game.id == id)
    }

    pub fn game_mut(&mut self, id: &str) -> Option<&mut GameProfile> {
        self.games.iter_mut().find(|game| game.id == id)
    }

    /// The model to ask: the game's own pick if it has one, the global one otherwise.
    pub fn model_for_game<'a>(
        &'a self,
        tier: Tier,
        game: Option<&'a GameProfile>,
    ) -> Option<&'a ModelRef> {
        game.and_then(|game| game.model_for(tier))
            .or_else(|| self.model_for(tier))
    }

    /// Drops every reference to an account that's being removed.
    pub fn forget_account(&mut self, account_id: &str) {
        self.accounts.retain(|account| account.id != account_id);
        let slots = [&mut self.fast_model, &mut self.deep_model]
            .into_iter()
            .chain(
                self.games
                    .iter_mut()
                    .flat_map(|game| [&mut game.fast_model, &mut game.deep_model]),
            );
        for slot in slots {
            if slot
                .as_ref()
                .is_some_and(|model| model.account_id == account_id)
            {
                *slot = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::games::GameProfile;
    use crate::provider::ProviderKind;

    #[test]
    fn missing_file_gives_default() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load_from(&dir.path().join("config.toml")).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/config.toml");
        let config = Config {
            accounts: vec![
                ProviderAccount {
                    id: "a".into(),
                    kind: ProviderKind::Anthropic,
                    label: None,
                    base_url: None,
                },
                ProviderAccount {
                    id: "o".into(),
                    kind: ProviderKind::OpenAi,
                    label: Some("Work".into()),
                    base_url: None,
                },
            ],
            fast_model: Some(ModelRef {
                account_id: "a".into(),
                model: "claude-haiku-5-5".into(),
            }),
            deep_model: Some(ModelRef {
                account_id: "a".into(),
                model: "claude-sonnet-5-5".into(),
            }),
            hotkeys: Hotkeys::default(),
            games: vec![GameProfile {
                notes: "Playing a ranged build.\nNo spoilers.".into(),
                deep_model: Some(ModelRef {
                    account_id: "o".into(),
                    model: "gpt-6-sol".into(),
                }),
                ..GameProfile::for_window("Hades II", "Hades II")
            }],
        };

        config.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path).unwrap(), config);
        assert_eq!(
            config.model_for(Tier::Deep).unwrap().model,
            "claude-sonnet-5-5"
        );
        assert_eq!(config.account("o").unwrap().display_name(), "Work");

        let game = config.game_for("Hades II", "Hades II");
        assert_eq!(
            config.model_for_game(Tier::Deep, game).unwrap().model,
            "gpt-6-sol"
        );
        assert_eq!(
            config.model_for_game(Tier::Fast, game).unwrap().model,
            "claude-haiku-5-5"
        );
    }

    #[test]
    fn forgetting_an_account_clears_every_pick_that_used_it() {
        let pick = |account: &str| {
            Some(ModelRef {
                account_id: account.into(),
                model: "m".into(),
            })
        };
        let mut config = Config {
            fast_model: pick("gone"),
            deep_model: pick("kept"),
            games: vec![GameProfile {
                fast_model: pick("gone"),
                deep_model: pick("kept"),
                ..GameProfile::for_window("Game", "")
            }],
            ..Config::default()
        };

        config.forget_account("gone");
        assert_eq!(config.fast_model, None);
        assert_eq!(config.deep_model, pick("kept"));
        assert_eq!(config.games[0].fast_model, None);
        assert_eq!(config.games[0].deep_model, pick("kept"));
    }

    #[test]
    fn rejects_unknown_provider() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "[[accounts]]\nid = \"x\"\nkind = \"nope\"\n").unwrap();
        assert!(matches!(
            Config::load_from(&path),
            Err(ConfigError::Parse { .. })
        ));
    }
}
