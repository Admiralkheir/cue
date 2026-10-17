use serde::{Deserialize, Serialize};

/// An AI provider Cue can talk to. The user brings their own key for each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Anthropic,
    OpenAi,
    OpenCodeZen,
    OpenCodeGo,
    Gemini,
    Ollama,
}

impl ProviderKind {
    pub const ALL: [ProviderKind; 6] = [
        ProviderKind::Anthropic,
        ProviderKind::OpenAi,
        ProviderKind::OpenCodeZen,
        ProviderKind::OpenCodeGo,
        ProviderKind::Gemini,
        ProviderKind::Ollama,
    ];

    pub fn display_name(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "Anthropic (Claude)",
            ProviderKind::OpenAi => "OpenAI",
            ProviderKind::OpenCodeZen => "OpenCode Zen",
            ProviderKind::OpenCodeGo => "OpenCode Go",
            ProviderKind::Gemini => "Google Gemini",
            ProviderKind::Ollama => "Ollama (local)",
        }
    }

    /// Stable lowercase name, the same one the config file uses.
    pub fn slug(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::OpenAi => "open_ai",
            ProviderKind::OpenCodeZen => "open_code_zen",
            ProviderKind::OpenCodeGo => "open_code_go",
            ProviderKind::Gemini => "gemini",
            ProviderKind::Ollama => "ollama",
        }
    }

    /// Local providers run without a key.
    pub fn needs_api_key(self) -> bool {
        !matches!(self, ProviderKind::Ollama)
    }
}

/// One provider key the user added. A user may add several, even for the same provider.
///
/// The key itself is never stored here: it lives in the OS keychain under `id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderAccount {
    pub id: String,
    pub kind: ProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Overrides the provider's default endpoint, e.g. a remote Ollama host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

impl ProviderAccount {
    pub fn display_name(&self) -> &str {
        self.label.as_deref().unwrap_or(self.kind.display_name())
    }
}

/// A model as listed by a provider for a given key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: Option<String>,
    /// Whether the model accepts images, when the provider says.
    pub vision: Option<bool>,
}

impl ModelInfo {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            display_name: None,
            vision: None,
        }
    }
}

/// The user's choice of model: which account to bill and which model to call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRef {
    pub account_id: String,
    pub model: String,
}
