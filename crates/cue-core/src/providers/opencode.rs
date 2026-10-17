//! OpenCode Zen and OpenCode Go: one key for many vendors' models. OpenCode serves each
//! model in its vendor's API format, so requests are routed per model.
//!
//! Only models served in the Anthropic Messages format are wired up so far.

use std::ops::ControlFlow;

use serde_json::Value;

use super::{Anthropic, Completion, Provider, ProviderError, http_agent};
use crate::conversation::Conversation;
use crate::provider::ModelInfo;
use crate::recommend::Tier;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// Pay as you go.
    Zen,
    /// The monthly subscription.
    Go,
}

impl Plan {
    fn base_url(self) -> &'static str {
        match self {
            Plan::Zen => "https://opencode.ai/inference",
            Plan::Go => "https://opencode.ai/inference/go",
        }
    }

    /// Non-Claude models OpenCode serves in the Anthropic Messages format, from
    /// https://opencode.ai/docs/zen/ and https://opencode.ai/docs/go/.
    fn anthropic_format_models(self) -> &'static [&'static str] {
        match self {
            Plan::Zen => &[
                "qwen3.8-flash",
                "qwen3.7-max",
                "qwen3.7-plus",
                "qwen3.6-plus",
                "qwen3.5-plus",
            ],
            Plan::Go => &["minimax-m3", "qwen3.8-max", "qwen3.8-flash", "qwen3.7-plus"],
        }
    }

    fn speaks_anthropic(self, model: &str) -> bool {
        model.starts_with("claude-") || self.anthropic_format_models().contains(&model)
    }
}

pub struct OpenCode {
    plan: Plan,
    base_url: String,
    api_key: String,
    agent: ureq::Agent,
    anthropic: Anthropic,
}

impl OpenCode {
    pub fn new(plan: Plan, api_key: String, base_url: Option<&str>) -> Self {
        let base_url = base_url
            .unwrap_or(plan.base_url())
            .trim_end_matches('/')
            .to_owned();
        Self {
            plan,
            // OpenCode asks clients to send a stable id per conversation.
            anthropic: Anthropic::new(api_key.clone(), Some(&format!("{base_url}/anthropic")))
                .with_session_header("x-opencode-session"),
            base_url,
            api_key,
            agent: http_agent(),
        }
    }
}

impl Provider for OpenCode {
    fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let response = self
            .agent
            .get(format!("{}/v1/models", self.base_url))
            .header("authorization", &format!("Bearer {}", self.api_key))
            .call()
            .map_err(|err| ProviderError::Network(err.to_string()))?;
        let status = response.status().as_u16();
        let body = response
            .into_body()
            .read_to_string()
            .map_err(|err| ProviderError::Network(err.to_string()))?;
        if status != 200 {
            return Err(ProviderError::Http {
                status,
                message: body.chars().take(200).collect(),
            });
        }
        let list: Value =
            serde_json::from_str(&body).map_err(|err| ProviderError::Protocol(err.to_string()))?;
        let models = list["data"]
            .as_array()
            .ok_or_else(|| ProviderError::Protocol("model list without `data`".into()))?;

        Ok(models
            .iter()
            .filter_map(|model| model["id"].as_str())
            // Models in other API formats can't be called yet, so don't offer them.
            .filter(|id| self.plan.speaks_anthropic(id))
            .map(|id| ModelInfo {
                id: id.to_owned(),
                display_name: None,
                // OpenCode's list has no capability data; every Claude model takes images.
                vision: id.starts_with("claude-").then_some(true),
            })
            .collect())
    }

    fn stream(
        &self,
        model: &str,
        tier: Tier,
        conversation: &Conversation,
        on_text: &mut dyn FnMut(&str) -> ControlFlow<()>,
    ) -> Result<Completion, ProviderError> {
        if !self.plan.speaks_anthropic(model) {
            return Err(ProviderError::Unsupported("this OpenCode model"));
        }
        self.anthropic.stream(model, tier, conversation, on_text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::EncodedFrame;
    use crate::conversation::{Role, Turn};
    use crate::providers::StopReason;
    use crate::providers::mock::{MockServer, Reply};

    #[test]
    fn lists_only_models_it_can_call() {
        let server = MockServer::start(vec![Reply::json(
            200,
            r#"{"object":"list","data":[{"id":"claude-haiku-5-5"},{"id":"glm-5.3"},{"id":"minimax-m3"},{"id":"gpt-6-luna"}]}"#,
        )]);
        let client = OpenCode::new(Plan::Go, "oc-test".into(), Some(&server.url));

        let models = client.list_models().unwrap();
        let ids: Vec<_> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(ids, ["claude-haiku-5-5", "minimax-m3"]);
        assert_eq!(models[0].vision, Some(true));
        assert_eq!(models[1].vision, None);

        let request = &server.requests()[0];
        assert_eq!(request.path, "/v1/models");
        assert_eq!(request.header("authorization"), Some("Bearer oc-test"));
    }

    #[test]
    fn sends_claude_models_through_the_anthropic_endpoint() {
        let server = MockServer::start(vec![
            // Capability lookup: OpenCode doesn't serve it, so Cue goes by the model id.
            Reply::json(404, "{}"),
            Reply::new(
                200,
                "text/event-stream",
                "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n\
                 data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
            ),
        ]);
        let client = OpenCode::new(Plan::Go, "oc-test".into(), Some(&server.url));
        let conversation = Conversation {
            id: "conversation-1".into(),
            system: String::new(),
            image: Some(EncodedFrame {
                width: 1,
                height: 1,
                jpeg: vec![0xFF],
            }),
            turns: vec![Turn {
                role: Role::User,
                text: "hello".into(),
            }],
        };

        let mut text = String::new();
        let completion = client
            .stream(
                "claude-haiku-5-5",
                Tier::Fast,
                &conversation,
                &mut |piece| {
                    text.push_str(piece);
                    ControlFlow::Continue(())
                },
            )
            .unwrap();
        assert_eq!(
            (text.as_str(), completion.stop),
            ("hi", StopReason::EndTurn)
        );

        let requests = server.requests();
        assert_eq!(requests[1].path, "/anthropic/v1/messages");
        assert_eq!(requests[1].header("x-api-key"), Some("oc-test"));
        assert_eq!(
            requests[1].header("x-opencode-session"),
            Some("conversation-1")
        );
        assert_eq!(
            requests[1].header("user-agent"),
            Some(crate::providers::USER_AGENT)
        );
        let body: Value = serde_json::from_str(&requests[1].body).unwrap();
        assert_eq!(body["output_config"]["effort"], "low");
    }

    #[test]
    fn refuses_models_in_other_formats() {
        let client = OpenCode::new(Plan::Zen, "oc-test".into(), Some("http://127.0.0.1:9"));
        let conversation = Conversation {
            id: "conversation-1".into(),
            system: String::new(),
            image: None,
            turns: Vec::new(),
        };
        let result = client.stream("gpt-6-luna", Tier::Deep, &conversation, &mut |_| {
            ControlFlow::Continue(())
        });
        assert!(matches!(result, Err(ProviderError::Unsupported(_))));
    }
}
