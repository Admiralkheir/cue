//! Clients for the AI providers. Calls block, so run them off the UI thread.

mod anthropic;
#[cfg(test)]
mod mock;
mod opencode;
mod sse;

use std::ops::ControlFlow;

pub use anthropic::Anthropic;
pub use opencode::{OpenCode, Plan};

use crate::conversation::Conversation;
use crate::provider::{ModelInfo, ProviderAccount, ProviderKind};
use crate::recommend::Tier;

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("no API key is stored for this account")]
    MissingKey,
    #[error("{0} isn't supported yet")]
    Unsupported(&'static str),
    #[error("the provider returned HTTP {status}: {message}")]
    Http { status: u16, message: String },
    #[error("the provider reported an error: {0}")]
    Api(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("unexpected response: {0}")]
    Protocol(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    /// The model declined; the explanation comes from the provider when it gives one.
    Refusal(Option<String>),
    /// The caller stopped the stream.
    Cancelled,
    Other(String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    pub stop: StopReason,
    pub usage: Usage,
}

pub trait Provider: Send + Sync {
    /// Models this key can use.
    fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError>;

    /// Streams the answer to the conversation's last user turn. `on_text` gets each
    /// piece of text as it arrives and can stop the stream by returning `Break`.
    fn stream(
        &self,
        model: &str,
        tier: Tier,
        conversation: &Conversation,
        on_text: &mut dyn FnMut(&str) -> ControlFlow<()>,
    ) -> Result<Completion, ProviderError>;
}

/// Whether Cue has a client for this provider yet.
pub fn is_supported(kind: ProviderKind) -> bool {
    matches!(
        kind,
        ProviderKind::Anthropic | ProviderKind::OpenCodeZen | ProviderKind::OpenCodeGo
    )
}

/// Builds the client for an account. `api_key` comes from the keychain.
pub fn connect(
    account: &ProviderAccount,
    api_key: Option<String>,
) -> Result<Box<dyn Provider>, ProviderError> {
    let needs_key = account.kind.needs_api_key();
    let api_key = match api_key {
        Some(key) => key,
        None if needs_key => return Err(ProviderError::MissingKey),
        None => String::new(),
    };
    let base_url = account.base_url.as_deref();
    match account.kind {
        ProviderKind::Anthropic => Ok(Box::new(Anthropic::new(api_key, base_url))),
        ProviderKind::OpenCodeZen => Ok(Box::new(OpenCode::new(Plan::Zen, api_key, base_url))),
        ProviderKind::OpenCodeGo => Ok(Box::new(OpenCode::new(Plan::Go, api_key, base_url))),
        kind => Err(ProviderError::Unsupported(kind.display_name())),
    }
}

/// Output budget per tier. Thinking counts against it, so the fast tier isn't tiny.
fn max_tokens(tier: Tier) -> u32 {
    match tier {
        Tier::Fast => 4_096,
        Tier::Deep => 16_000,
    }
}

/// Sent with every request so providers can tell Cue's traffic apart.
pub const USER_AGENT: &str = concat!("cue/", env!("CARGO_PKG_VERSION"));

fn http_agent() -> ureq::Agent {
    use std::time::Duration;
    ureq::Agent::config_builder()
        .user_agent(USER_AGENT)
        // Read error bodies ourselves instead of getting a bare status code.
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(120)))
        .build()
        .into()
}
