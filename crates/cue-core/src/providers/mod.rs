//! Clients for the AI providers. Calls block, so run them off the UI thread.

mod answer;
mod anthropic;
mod chat_completions;
mod gemini;
mod google;
pub(crate) mod http;
#[cfg(test)]
pub(crate) mod mock;
mod openai;
mod opencode;
mod responses;
mod sse;

use std::ops::ControlFlow;

pub use anthropic::Anthropic;
pub use chat_completions::ChatCompletions;
pub use gemini::Gemini;
pub use google::GoogleAi;
pub use http::USER_AGENT;
pub use openai::OpenAi;
pub use opencode::{OpenCode, Plan};
pub use responses::Responses;

use crate::conversation::Conversation;
use crate::provider::{Effort, ModelInfo, ProviderAccount, ProviderKind};

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
    /// Web searches made for the answer, by the provider or through Cue's search tool.
    pub web_searches: u32,
}

impl Usage {
    fn add(&mut self, other: Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.web_searches += other.web_searches;
    }
}

/// A search engine the model can call as a tool, like a local SearXNG.
pub trait WebSearch: Send + Sync {
    /// Results for `query` as text for the model to read.
    fn search(&self, query: &str) -> Result<String, String>;
}

/// Whether and how the model may search the web.
#[derive(Clone, Copy, Default)]
pub enum Search<'a> {
    #[default]
    Off,
    /// The provider's own search tool, billed by the provider per search. Models in formats
    /// without one answer without searching.
    Provider,
    /// A search engine Cue calls on the model's behalf.
    Tool(&'a dyn WebSearch),
}

/// How to answer, beyond which model and what to answer.
#[derive(Clone, Copy, Default)]
pub struct Options<'a> {
    pub effort: Effort,
    pub search: Search<'a>,
}

/// What a stream reports as it goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event<'a> {
    /// A piece of the answer.
    Text(&'a str),
    /// The model started a web search, for this query when the provider says.
    Searching(Option<&'a str>),
}

/// Gets each [`Event`] and can stop the stream by returning `Break`.
pub type OnEvent<'a> = dyn FnMut(Event<'_>) -> ControlFlow<()> + 'a;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    pub stop: StopReason,
    pub usage: Usage,
}

pub trait Provider: Send + Sync {
    /// Models this key can use.
    fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError>;

    /// Streams the answer to the conversation's last user turn. `on_event` gets each
    /// piece of text as it arrives, and each web search the model starts.
    fn stream(
        &self,
        model: &str,
        options: Options<'_>,
        conversation: &Conversation,
        on_event: &mut OnEvent<'_>,
    ) -> Result<Completion, ProviderError>;
}

/// Whether Cue has a client for this provider yet.
pub fn is_supported(kind: ProviderKind) -> bool {
    // Ollama is the one provider still to come.
    !matches!(kind, ProviderKind::Ollama)
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
        ProviderKind::OpenAi => Ok(Box::new(OpenAi::new(api_key, base_url))),
        ProviderKind::Gemini => Ok(Box::new(GoogleAi::new(api_key, base_url))),
        ProviderKind::OpenCodeZen => Ok(Box::new(OpenCode::new(Plan::Zen, api_key, base_url))),
        ProviderKind::OpenCodeGo => Ok(Box::new(OpenCode::new(Plan::Go, api_key, base_url))),
        kind => Err(ProviderError::Unsupported(kind.display_name())),
    }
}

/// Output budget per request. Thinking counts against it, so even low effort isn't tiny.
fn max_tokens(effort: Effort) -> u32 {
    match effort {
        Effort::Low => 4_096,
        Effort::Medium => 8_192,
        Effort::High | Effort::Default => 16_000,
    }
}
