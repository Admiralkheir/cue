//! OpenAI's own API: models listed from `/v1/models`, answers through the Responses API.

use std::ops::ControlFlow;

use super::{Completion, Provider, ProviderError, Responses, http};
use crate::conversation::Conversation;
use crate::provider::ModelInfo;
use crate::recommend::Tier;

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

pub struct OpenAi {
    base_url: String,
    api_key: String,
    agent: ureq::Agent,
    responses: Responses,
}

impl OpenAi {
    pub fn new(api_key: String, base_url: Option<&str>) -> Self {
        let base_url = base_url
            .unwrap_or(DEFAULT_BASE_URL)
            .trim_end_matches('/')
            .to_owned();
        Self {
            responses: Responses::new(format!("{base_url}/responses"), api_key.clone()),
            base_url,
            api_key,
            agent: http::agent(),
        }
    }
}

/// `/v1/models` lists every model the key can use, including audio, image and embedding
/// models. Keep the ones that can answer a question about a screenshot.
fn is_chat_model(id: &str) -> bool {
    const FAMILIES: &[&str] = &["gpt-", "chatgpt-", "o1", "o3", "o4"];
    const NOT_CHAT: &[&str] = &[
        "audio",
        "realtime",
        "transcribe",
        "tts",
        "search",
        "image",
        "embedding",
        "moderation",
        "instruct",
    ];
    FAMILIES.iter().any(|family| id.starts_with(family))
        && !NOT_CHAT.iter().any(|kind| id.contains(kind))
}

impl Provider for OpenAi {
    fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let list = http::get_json(
            self.agent
                .get(format!("{}/models", self.base_url))
                .header("authorization", &format!("Bearer {}", self.api_key)),
        )?;
        let models = list["data"]
            .as_array()
            .ok_or_else(|| ProviderError::Protocol("model list without `data`".into()))?;
        let mut models: Vec<ModelInfo> = models
            .iter()
            .filter_map(|model| model["id"].as_str())
            .filter(|id| is_chat_model(id))
            .map(ModelInfo::new)
            .collect();
        models.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(models)
    }

    fn stream(
        &self,
        model: &str,
        tier: Tier,
        conversation: &Conversation,
        on_text: &mut dyn FnMut(&str) -> ControlFlow<()>,
    ) -> Result<Completion, ProviderError> {
        self.responses.stream(model, tier, conversation, on_text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::mock::{MockServer, Reply};

    #[test]
    fn lists_only_chat_models() {
        let server = MockServer::start(vec![Reply::json(
            200,
            r#"{"data":[{"id":"gpt-6-luna"},{"id":"text-embedding-3-large"},{"id":"gpt-realtime"},
                {"id":"gpt-image-2"},{"id":"o4-mini"},{"id":"whisper-1"},{"id":"gpt-5.4-mini"}]}"#,
        )]);
        let client = OpenAi::new("sk-test".into(), Some(&format!("{}/v1", server.url)));

        let ids: Vec<_> = client
            .list_models()
            .unwrap()
            .into_iter()
            .map(|model| model.id)
            .collect();
        assert_eq!(ids, ["gpt-5.4-mini", "gpt-6-luna", "o4-mini"]);
        let request = &server.requests()[0];
        assert_eq!(request.path, "/v1/models");
        assert_eq!(request.header("authorization"), Some("Bearer sk-test"));
    }
}
