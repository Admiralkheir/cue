//! UI-independent core of Cue: configuration, provider accounts and their clients,
//! model recommendations, API key storage and screen capture.

pub mod capture;
pub mod config;
pub mod conversation;
pub mod provider;
pub mod providers;
pub mod recommend;
pub mod secrets;

pub use capture::{Capture, CaptureError, EncodedFrame, capture_focused_window};
pub use config::{Config, ConfigError, Hotkeys};
pub use conversation::{Conversation, Role, Turn};
pub use provider::{ModelInfo, ModelRef, ProviderAccount, ProviderKind};
pub use recommend::{AccountModels, Tier, recommend};
