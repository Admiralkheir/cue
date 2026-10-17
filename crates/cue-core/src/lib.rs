//! UI-independent core of Cue: configuration, provider accounts, model
//! recommendations and screen capture. Provider clients will live here too.

pub mod capture;
pub mod config;
pub mod provider;
pub mod recommend;

pub use capture::{Capture, CaptureError, EncodedFrame, capture_focused_window};
pub use config::{Config, ConfigError, Hotkeys};
pub use provider::{ModelInfo, ModelRef, ProviderAccount, ProviderKind};
pub use recommend::{AccountModels, Tier, recommend};
