//! UI-independent core of Cue: configuration, game profiles, provider accounts and their
//! clients, model recommendations, API key storage and screen capture.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub mod capture;
pub mod config;
pub mod conversation;
pub mod games;
pub mod provider;
pub mod providers;
pub mod recommend;
pub mod secrets;

pub use capture::{Capture, CaptureError, EncodedFrame, capture_focused_window};
pub use config::{Config, ConfigError, Hotkeys, OverlayPosition, OverlaySettings};
pub use conversation::{Conversation, Role, Turn};
pub use games::{GameProfile, find_profile};
pub use provider::{ModelInfo, ModelRef, ProviderAccount, ProviderKind};
pub use recommend::{AccountModels, Tier, recommend};

/// An id unique within this run and, through the timestamp, practically across runs.
pub fn unique_id(prefix: &str) -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_nanos());
    format!(
        "{prefix}-{nanos:x}-{:x}",
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}
