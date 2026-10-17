//! Platform- and UI-independent core of Cue: configuration, provider accounts and
//! model recommendations. Screen capture and provider clients will live here too.

pub mod config;
pub mod provider;
pub mod recommend;

pub use config::{Config, ConfigError, Hotkeys};
pub use provider::{ModelInfo, ModelRef, ProviderAccount, ProviderKind};
pub use recommend::{AccountModels, Tier, recommend};
