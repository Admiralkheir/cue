//! Claude through the Anthropic Messages API, over raw HTTP (there's no official Rust SDK).

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::sync::Mutex;

use serde_json::{Value, json};

use super::http::{self, error_message};
use super::{Completion, Provider, ProviderError, StopReason, Usage, max_tokens};
use crate::capture::EncodedFrame;
use crate::conversation::{Conversation, Role};
use crate::provider::ModelInfo;
use crate::recommend::Tier;

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

/// What we need to know about a model before calling it.
#[derive(Debug, Clone, Copy, Default)]
struct Capabilities {
    low_effort: bool,
}

impl Capabilities {
    fn from_model(model: &Value) -> Self {
        let caps = &model["capabilities"];
        Self {
            low_effort: caps["effort"]["supported"].as_bool() == Some(true)
                && caps["effort"]["low"]["supported"].as_bool() == Some(true),
        }
    }

    /// Fallback for gateways that don't serve the model lookup, like OpenCode.
    fn known(model: &str) -> Self {
        // Models documented to accept `effort: low`.
        const LOW_EFFORT: &[&str] = &[
            "claude-haiku-5-5",
            "claude-sonnet-5-5",
            "claude-sonnet-5",
            "claude-sonnet-4-6",
            "claude-opus-5-5",
            "claude-opus-5",
            "claude-opus-4-8",
            "claude-opus-4-7",
            "claude-opus-4-6",
            "claude-fable-5-1",
            "claude-fable-5",
        ];
        Self {
            low_effort: LOW_EFFORT.contains(&model),
        }
    }
}

pub struct Anthropic {
    base_url: String,
    api_key: String,
    agent: ureq::Agent,
    capabilities: Mutex<HashMap<String, Capabilities>>,
    /// Header that carries the conversation id, for gateways that route on it.
    session_header: Option<&'static str>,
}

impl Anthropic {
    pub fn new(api_key: String, base_url: Option<&str>) -> Self {
        Self {
            base_url: base_url
                .unwrap_or(DEFAULT_BASE_URL)
                .trim_end_matches('/')
                .to_owned(),
            api_key,
            agent: http::agent(),
            capabilities: Mutex::default(),
            session_header: None,
        }
    }

    pub fn with_session_header(mut self, name: &'static str) -> Self {
        self.session_header = Some(name);
        self
    }

    fn get(&self, path: &str) -> Result<Value, ProviderError> {
        http::get_json(
            self.agent
                .get(format!("{}{path}", self.base_url))
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", API_VERSION),
        )
    }

    /// Looked up once per model; if the lookup fails we go by the model id.
    fn capabilities(&self, model: &str) -> Capabilities {
        if let Some(caps) = self.capabilities.lock().unwrap().get(model) {
            return *caps;
        }
        let caps = self
            .get(&format!("/v1/models/{model}"))
            .map(|model| Capabilities::from_model(&model))
            .unwrap_or_else(|_| Capabilities::known(model));
        self.capabilities
            .lock()
            .unwrap()
            .insert(model.to_owned(), caps);
        caps
    }

    fn request_body(&self, model: &str, tier: Tier, conversation: &Conversation) -> Value {
        let mut body = json!({
            "model": model,
            "max_tokens": max_tokens(tier),
            "stream": true,
            "system": conversation.system,
            "messages": messages(conversation),
        });
        // Quick in-game questions don't need much thinking, and less thinking is faster.
        if tier == Tier::Fast && self.capabilities(model).low_effort {
            body["output_config"] = json!({ "effort": "low" });
        }
        body
    }
}

fn messages(conversation: &Conversation) -> Vec<Value> {
    let mut image = conversation.image.as_ref();
    conversation
        .turns
        .iter()
        .map(|turn| match turn.role {
            Role::User => {
                let mut content = Vec::new();
                // The screenshot goes before the first question's text.
                if let Some(frame) = image.take() {
                    content.push(image_block(frame));
                }
                content.push(json!({ "type": "text", "text": turn.text }));
                json!({ "role": "user", "content": content })
            }
            Role::Assistant => json!({ "role": "assistant", "content": turn.text }),
        })
        .collect()
}

fn image_block(frame: &EncodedFrame) -> Value {
    json!({
        "type": "image",
        "source": {
            "type": "base64",
            "media_type": EncodedFrame::MEDIA_TYPE,
            "data": http::base64(frame),
        },
    })
}

impl Provider for Anthropic {
    fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let page = self.get("/v1/models?limit=1000")?;
        let models = page["data"]
            .as_array()
            .ok_or_else(|| ProviderError::Protocol("model list without `data`".into()))?;

        let mut cache = self.capabilities.lock().unwrap();
        Ok(models
            .iter()
            .filter_map(|model| {
                let id = model["id"].as_str()?.to_owned();
                cache.insert(id.clone(), Capabilities::from_model(model));
                Some(ModelInfo {
                    display_name: model["display_name"].as_str().map(str::to_owned),
                    vision: model["capabilities"]["image_input"]["supported"].as_bool(),
                    id,
                })
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
        let body = self.request_body(model, tier, conversation);
        let mut request = self
            .agent
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION);
        if let Some(name) = self.session_header {
            request = request.header(name, &conversation.id);
        }
        let mut body = http::send_json(request, &body)?;
        read_stream(&mut body, on_text)
    }
}

fn read_stream(
    body: &mut ureq::Body,
    on_text: &mut dyn FnMut(&str) -> ControlFlow<()>,
) -> Result<Completion, ProviderError> {
    let mut usage = Usage::default();
    let mut stop = None;

    for data in http::json_events(body) {
        let Some(data) = data? else { break };
        match data["type"].as_str() {
            Some("message_start") => {
                usage.input_tokens = data["message"]["usage"]["input_tokens"]
                    .as_u64()
                    .unwrap_or(0);
            }
            Some("content_block_delta") if data["delta"]["type"] == "text_delta" => {
                let text = data["delta"]["text"].as_str().unwrap_or_default();
                if on_text(text).is_break() {
                    return Ok(Completion {
                        stop: StopReason::Cancelled,
                        usage,
                    });
                }
            }
            Some("message_delta") => {
                if let Some(output) = data["usage"]["output_tokens"].as_u64() {
                    usage.output_tokens = output;
                }
                stop = data["delta"]["stop_reason"]
                    .as_str()
                    .map(|reason| match reason {
                        "end_turn" | "stop_sequence" => StopReason::EndTurn,
                        "max_tokens" => StopReason::MaxTokens,
                        "refusal" => StopReason::Refusal(
                            data["delta"]["stop_details"]["explanation"]
                                .as_str()
                                .map(str::to_owned),
                        ),
                        other => StopReason::Other(other.to_owned()),
                    });
            }
            Some("error") => {
                let message = error_message(&data).unwrap_or_else(|| "unknown error".into());
                return Err(ProviderError::Api(message));
            }
            Some("message_stop") => break,
            // Thinking blocks, pings and block boundaries carry nothing we show.
            _ => {}
        }
    }

    stop.map(|stop| Completion { stop, usage })
        .ok_or_else(|| ProviderError::Protocol("the stream ended without a stop reason".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::Turn;
    use crate::providers::mock::{MockServer, Reply};

    const MODEL_WITH_EFFORT: &str = r#"{"id":"claude-haiku-5-5","display_name":"Claude Haiku 5.5",
        "capabilities":{"image_input":{"supported":true},
        "effort":{"supported":true,"low":{"supported":true}}}}"#;

    fn conversation() -> Conversation {
        Conversation {
            id: "conversation-1".into(),
            system: "be brief".into(),
            image: Some(EncodedFrame {
                width: 2,
                height: 1,
                jpeg: vec![0xFF, 0xD8, 0xFF],
            }),
            turns: vec![
                Turn {
                    role: Role::User,
                    text: "where now?".into(),
                },
                Turn {
                    role: Role::Assistant,
                    text: "go left".into(),
                },
                Turn {
                    role: Role::User,
                    text: "and then?".into(),
                },
            ],
        }
    }

    fn sse(events: &[&str]) -> Reply {
        let body: String = events
            .iter()
            .map(|data| format!("event: x\ndata: {data}\n\n"))
            .collect();
        Reply::new(200, "text/event-stream", body)
    }

    fn collect(client: &Anthropic, tier: Tier) -> (Result<Completion, ProviderError>, String) {
        let mut text = String::new();
        let result = client.stream("claude-haiku-5-5", tier, &conversation(), &mut |piece| {
            text.push_str(piece);
            ControlFlow::Continue(())
        });
        (result, text)
    }

    #[test]
    fn lists_models_with_vision_support() {
        let server = MockServer::start(vec![Reply::json(
            200,
            &format!(
                r#"{{"data":[{MODEL_WITH_EFFORT},{{"id":"text-only","capabilities":{{"image_input":{{"supported":false}}}}}}]}}"#
            ),
        )]);
        let client = Anthropic::new("sk-test".into(), Some(&server.url));

        let models = client.list_models().unwrap();
        assert_eq!(models[0].id, "claude-haiku-5-5");
        assert_eq!(models[0].display_name.as_deref(), Some("Claude Haiku 5.5"));
        assert_eq!(models[0].vision, Some(true));
        assert_eq!(models[1].vision, Some(false));

        let request = &server.requests()[0];
        assert_eq!(request.path, "/v1/models?limit=1000");
        assert_eq!(request.header("x-api-key"), Some("sk-test"));
        assert_eq!(request.header("anthropic-version"), Some(API_VERSION));
        assert_eq!(
            request.header("user-agent"),
            Some(crate::providers::USER_AGENT)
        );
    }

    #[test]
    fn streams_text_and_sends_the_screenshot_once() {
        let server = MockServer::start(vec![
            Reply::json(200, MODEL_WITH_EFFORT),
            sse(&[
                r#"{"type":"message_start","message":{"usage":{"input_tokens":2900}}}"#,
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}"#,
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Take the "}}"#,
                r#"{"type":"ping"}"#,
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"north gate."}}"#,
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":42}}"#,
                r#"{"type":"message_stop"}"#,
            ]),
        ]);
        let client = Anthropic::new("sk-test".into(), Some(&server.url));

        let (result, text) = collect(&client, Tier::Fast);
        assert_eq!(text, "Take the north gate.");
        assert_eq!(
            result.unwrap(),
            Completion {
                stop: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 2900,
                    output_tokens: 42
                }
            }
        );

        let requests = server.requests();
        assert_eq!(requests[0].path, "/v1/models/claude-haiku-5-5");
        let body: Value = serde_json::from_str(&requests[1].body).unwrap();
        assert_eq!(body["output_config"]["effort"], "low");
        assert_eq!(body["stream"], true);
        assert_eq!(requests[1].header("x-opencode-session"), None);
        assert_eq!(body["system"], "be brief");
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(
            messages[0]["content"][0]["source"]["media_type"],
            "image/jpeg"
        );
        assert_eq!(
            messages[0]["content"][0]["source"]["data"], "/9j/",
            "base64 of the JPEG bytes"
        );
        assert_eq!(messages[0]["content"][1]["text"], "where now?");
        assert_eq!(
            messages[1],
            json!({ "role": "assistant", "content": "go left" })
        );
        assert_eq!(
            messages[2]["content"].as_array().unwrap().len(),
            1,
            "image only on the first turn"
        );
    }

    #[test]
    fn deep_tier_leaves_effort_to_the_model() {
        let server = MockServer::start(vec![sse(&[
            r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":16000}}"#,
        ])]);
        let client = Anthropic::new("sk-test".into(), Some(&server.url));

        let (result, _) = collect(&client, Tier::Deep);
        assert_eq!(result.unwrap().stop, StopReason::MaxTokens);
        let body: Value = serde_json::from_str(&server.requests()[0].body).unwrap();
        assert!(body.get("output_config").is_none());
    }

    #[test]
    fn reports_refusals_http_errors_and_stream_errors() {
        let server = MockServer::start(vec![
            Reply::json(404, "{}"),
            sse(&[
                r#"{"type":"message_delta","delta":{"stop_reason":"refusal","stop_details":{"explanation":"policy"}}}"#,
            ]),
            Reply::json(
                401,
                r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
            ),
            sse(&[
                r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
            ]),
        ]);
        let client = Anthropic::new("sk-test".into(), Some(&server.url));

        assert_eq!(
            collect(&client, Tier::Fast).0.unwrap().stop,
            StopReason::Refusal(Some("policy".into()))
        );
        assert!(matches!(
            collect(&client, Tier::Fast).0,
            Err(ProviderError::Http { status: 401, message }) if message == "invalid x-api-key"
        ));
        assert!(
            matches!(collect(&client, Tier::Fast).0, Err(ProviderError::Api(m)) if m == "Overloaded")
        );
    }

    #[test]
    fn stops_when_the_caller_breaks() {
        let server = MockServer::start(vec![sse(&[
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"one"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"two"}}"#,
        ])]);
        let client = Anthropic::new("sk-test".into(), Some(&server.url));

        let mut seen = Vec::new();
        let completion = client
            .stream("m", Tier::Deep, &conversation(), &mut |piece| {
                seen.push(piece.to_owned());
                ControlFlow::Break(())
            })
            .unwrap();
        assert_eq!(completion.stop, StopReason::Cancelled);
        assert_eq!(seen, ["one"]);
    }
}
