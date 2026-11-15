//! OpenCode Zen and OpenCode Go: one key for many vendors' models. OpenCode serves each
//! model in its vendor's API format, so requests are routed per model.
//!
//! The Anthropic Messages, OpenAI Responses, Gemini and Chat Completions formats are all
//! wired up; only Jev, which isn't a chat model, is left out.

use super::http;
use super::{
    Anthropic, ChatCompletions, Completion, Gemini, OnEvent, Options, Provider, ProviderError,
    Responses, Search,
};
use crate::conversation::Conversation;
use crate::provider::ModelInfo;

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

    /// Which format OpenCode serves `model` in, from the endpoint tables in its docs.
    fn format(self, model: &str) -> Format {
        const RESPONSES: &[&str] = &["gpt-", "grok-", "muse-"];
        if model.starts_with("claude-") || self.anthropic_format_models().contains(&model) {
            Format::Anthropic
        } else if RESPONSES.iter().any(|prefix| model.starts_with(prefix)) {
            Format::Responses
        } else if model.starts_with("gemini-") {
            Format::Gemini
        } else if model.starts_with("jev-") {
            // Jev answers structured questionnaires, not chat.
            Format::Unsupported
        } else {
            // OpenCode's catch-all endpoint, used by most open models.
            Format::ChatCompletions
        }
    }
}

/// The API format OpenCode serves a model in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Anthropic,
    Responses,
    Gemini,
    ChatCompletions,
    Unsupported,
}

pub struct OpenCode {
    plan: Plan,
    base_url: String,
    api_key: String,
    agent: ureq::Agent,
    anthropic: Anthropic,
    responses: Responses,
    gemini: Gemini,
    chat: ChatCompletions,
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
            responses: Responses::new(format!("{base_url}/openai/v1/responses"), api_key.clone())
                .with_session_header("x-opencode-session"),
            gemini: Gemini::new(format!("{base_url}/google/v1beta/models"), api_key.clone())
                .with_session_header("x-opencode-session"),
            chat: ChatCompletions::new(
                format!("{base_url}/openai/v1/chat/completions"),
                api_key.clone(),
            )
            .with_session_header("x-opencode-session"),
            base_url,
            api_key,
            agent: http::agent(),
        }
    }
}

impl Provider for OpenCode {
    fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let list = http::get_json(
            self.agent
                .get(format!("{}/v1/models", self.base_url))
                .header("authorization", &format!("Bearer {}", self.api_key)),
        )?;
        let models = list["data"]
            .as_array()
            .ok_or_else(|| ProviderError::Protocol("model list without `data`".into()))?;

        Ok(models
            .iter()
            .filter_map(|model| model["id"].as_str())
            // Models in other API formats can't be called yet, so don't offer them.
            .filter(|id| self.plan.format(id) != Format::Unsupported)
            .map(|id| ModelInfo {
                id: id.to_owned(),
                display_name: None,
                // OpenCode's list has no capability data, but these families all take
                // images, and "-vl" (vision-language) in a name means a model does too.
                vision: (["claude-", "gemini-", "gpt-5", "gpt-6"]
                    .iter()
                    .any(|family| id.starts_with(family))
                    || id.contains("-vl"))
                .then_some(true),
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
        match self.plan.format(model) {
            Format::Anthropic => {
                // OpenCode doesn't pass Anthropic's own web search tool on: Claude models
                // answer that they can't search. Ask without it rather than promise a search.
                let options = match options.search {
                    Search::Provider => Options {
                        search: Search::Off,
                        ..options
                    },
                    _ => options,
                };
                self.anthropic
                    .stream(model, options, conversation, on_event)
            }
            Format::Responses => self
                .responses
                .stream(model, options, conversation, on_event),
            Format::Gemini => self.gemini.stream(model, options, conversation, on_event),
            Format::ChatCompletions => self.chat.stream(model, options, conversation, on_event),
            Format::Unsupported => Err(ProviderError::Unsupported("this OpenCode model")),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ops::ControlFlow;

    use serde_json::Value;

    use super::*;
    use crate::capture::EncodedFrame;
    use crate::conversation::{Role, Turn};
    use crate::providers::StopReason;
    use crate::providers::mock::{MockServer, Reply};

    #[test]
    fn lists_only_models_it_can_call() {
        let server = MockServer::start(vec![Reply::json(
            200,
            r#"{"object":"list","data":[{"id":"claude-haiku-5-5"},{"id":"glm-5.3"},{"id":"minimax-m3"},
                {"id":"gpt-6-luna"},{"id":"gemini-4-argon"},{"id":"jev-1.13"},{"id":"nemotron-3.5-super-vl-free"}]}"#,
        )]);
        let client = OpenCode::new(Plan::Go, "oc-test".into(), Some(&server.url));

        let models = client.list_models().unwrap();
        let ids: Vec<_> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "claude-haiku-5-5",
                "glm-5.3",
                "minimax-m3",
                "gpt-6-luna",
                "gemini-4-argon",
                "nemotron-3.5-super-vl-free"
            ]
        );
        let vision: Vec<_> = models.iter().map(|model| model.vision).collect();
        let yes = Some(true);
        assert_eq!(vision, [yes, None, None, yes, yes, yes]);

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
                Options {
                    effort: crate::provider::Effort::Low,
                    // OpenCode drops this tool, so Cue doesn't send it.
                    search: Search::Provider,
                },
                &conversation,
                &mut |event| {
                    if let crate::providers::Event::Text(piece) = event {
                        text.push_str(piece);
                    }
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
        assert!(body.get("tools").is_none());
        assert_eq!(body["system"], "", "no promise of a search it can't make");
    }

    #[test]
    fn sends_open_models_through_chat_completions() {
        let server = MockServer::start(vec![Reply::new(
            200,
            "text/event-stream",
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
        )]);
        let client = OpenCode::new(Plan::Zen, "oc-test".into(), Some(&server.url));
        let conversation = Conversation {
            id: "conversation-1".into(),
            system: String::new(),
            image: None,
            turns: vec![Turn {
                role: Role::User,
                text: "hello".into(),
            }],
        };

        let mut text = String::new();
        client
            .stream(
                "nemotron-3.5-super-vl-free",
                Options {
                    effort: crate::provider::Effort::Low,
                    ..Options::default()
                },
                &conversation,
                &mut |event| {
                    if let crate::providers::Event::Text(piece) = event {
                        text.push_str(piece);
                    }
                    ControlFlow::Continue(())
                },
            )
            .unwrap();
        assert_eq!(text, "ok");
        let request = &server.requests()[0];
        assert_eq!(request.path, "/openai/v1/chat/completions");
        assert_eq!(request.header("authorization"), Some("Bearer oc-test"));
        assert_eq!(request.header("x-opencode-session"), Some("conversation-1"));
    }

    #[test]
    fn sends_gpt_and_gemini_models_to_their_own_formats() {
        let server = MockServer::start(vec![
            Reply::new(
                200,
                "text/event-stream",
                "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
            ),
            Reply::new(
                200,
                "text/event-stream",
                "data: {\"candidates\":[{\"finishReason\":\"STOP\"}]}\n\n",
            ),
        ]);
        let client = OpenCode::new(Plan::Go, "oc-test".into(), Some(&server.url));
        let conversation = Conversation {
            id: "conversation-1".into(),
            system: String::new(),
            image: None,
            turns: vec![Turn {
                role: Role::User,
                text: "hello".into(),
            }],
        };

        for model in ["gpt-6-luna", "gemini-4-argon"] {
            client
                .stream(model, Options::default(), &conversation, &mut |_| {
                    ControlFlow::Continue(())
                })
                .unwrap();
        }
        let requests = server.requests();
        assert_eq!(requests[0].path, "/openai/v1/responses");
        assert_eq!(
            requests[1].path,
            "/google/v1beta/models/gemini-4-argon:streamGenerateContent?alt=sse"
        );
        assert_eq!(requests[1].header("x-goog-api-key"), Some("oc-test"));
        assert!(
            requests
                .iter()
                .all(|request| request.header("x-opencode-session") == Some("conversation-1"))
        );
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
        let result = client.stream("jev-1.13", Options::default(), &conversation, &mut |_| {
            ControlFlow::Continue(())
        });
        assert!(matches!(result, Err(ProviderError::Unsupported(_))));
    }
}
