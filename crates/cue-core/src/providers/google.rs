//! Google's Gemini API, with an API key from Google AI Studio.

use super::{Completion, Gemini, OnEvent, Options, Provider, ProviderError, http};
use crate::conversation::Conversation;
use crate::provider::ModelInfo;

const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

pub struct GoogleAi {
    base_url: String,
    api_key: String,
    agent: ureq::Agent,
    gemini: Gemini,
}

impl GoogleAi {
    pub fn new(api_key: String, base_url: Option<&str>) -> Self {
        let base_url = base_url
            .unwrap_or(DEFAULT_BASE_URL)
            .trim_end_matches('/')
            .to_owned();
        Self {
            gemini: Gemini::new(format!("{base_url}/models"), api_key.clone()),
            base_url,
            api_key,
            agent: http::agent(),
        }
    }
}

impl Provider for GoogleAi {
    fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let list = http::get_json(
            self.agent
                .get(format!("{}/models?pageSize=1000", self.base_url))
                .header("x-goog-api-key", &self.api_key),
        )?;
        let models = list["models"]
            .as_array()
            .ok_or_else(|| ProviderError::Protocol("model list without `models`".into()))?;
        Ok(models
            .iter()
            .filter(|model| {
                model["supportedGenerationMethods"]
                    .as_array()
                    .is_some_and(|methods| methods.iter().any(|method| method == "generateContent"))
            })
            .filter_map(|model| {
                let id = model["name"].as_str()?.strip_prefix("models/")?;
                // Gemma and other open models are listed too; Gemini models all take images.
                id.starts_with("gemini-").then(|| ModelInfo {
                    id: id.to_owned(),
                    display_name: model["displayName"].as_str().map(str::to_owned),
                    vision: Some(true),
                })
            })
            .collect())
    }

    fn stream(
        &self,
        model: &str,
        options: Options<'_>,
        conversation: &Conversation,
        on_event: &mut OnEvent<'_>,
    ) -> Result<Completion, ProviderError> {
        self.gemini.stream(model, options, conversation, on_event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::mock::{MockServer, Reply};

    #[test]
    fn lists_gemini_models_that_generate_content() {
        let server = MockServer::start(vec![Reply::json(
            200,
            r#"{"models":[
                {"name":"models/gemini-3.5-flash","displayName":"Gemini 3.5 Flash","supportedGenerationMethods":["generateContent","countTokens"]},
                {"name":"models/gemini-embedding-2","supportedGenerationMethods":["embedContent"]},
                {"name":"models/gemma-4-27b-it","supportedGenerationMethods":["generateContent"]}]}"#,
        )]);
        let client = GoogleAi::new("g-test".into(), Some(&format!("{}/v1beta", server.url)));

        let models = client.list_models().unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gemini-3.5-flash");
        assert_eq!(models[0].display_name.as_deref(), Some("Gemini 3.5 Flash"));
        let request = &server.requests()[0];
        assert_eq!(request.path, "/v1beta/models?pageSize=1000");
        assert_eq!(request.header("x-goog-api-key"), Some("g-test"));
    }
}
