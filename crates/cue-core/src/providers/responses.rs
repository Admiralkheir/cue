//! The OpenAI Responses API, which OpenAI's own models (and gateways serving them) use.

use std::ops::ControlFlow;

use serde_json::{Value, json};

use super::http::{self, error_message};
use super::{Completion, ProviderError, StopReason, Usage, max_tokens};
use crate::conversation::{Conversation, Role};
use crate::recommend::Tier;

pub struct Responses {
    /// The full `.../responses` URL.
    endpoint: String,
    api_key: String,
    agent: ureq::Agent,
    /// Header that carries the conversation id, for gateways that route on it.
    session_header: Option<&'static str>,
}

impl Responses {
    pub fn new(endpoint: String, api_key: String) -> Self {
        Self {
            endpoint,
            api_key,
            agent: http::agent(),
            session_header: None,
        }
    }

    pub fn with_session_header(mut self, name: &'static str) -> Self {
        self.session_header = Some(name);
        self
    }

    pub fn stream(
        &self,
        model: &str,
        tier: Tier,
        conversation: &Conversation,
        on_text: &mut dyn FnMut(&str) -> ControlFlow<()>,
    ) -> Result<Completion, ProviderError> {
        let mut body = json!({
            "model": model,
            "instructions": conversation.system,
            "input": input(conversation),
            "max_output_tokens": max_tokens(tier),
            "stream": true,
            // Screenshots of someone's screen have no reason to be kept server-side.
            "store": false,
        });
        // GPT-5 and later reason by default; quick in-game questions don't need much.
        if tier == Tier::Fast && reasons_by_default(model) {
            body["reasoning"] = json!({ "effort": "low" });
        }

        let mut request = self
            .agent
            .post(&self.endpoint)
            .header("authorization", &format!("Bearer {}", self.api_key));
        if let Some(name) = self.session_header {
            request = request.header(name, &conversation.id);
        }
        let mut body = http::send_json(request, &body)?;
        read_stream(&mut body, on_text)
    }
}

fn reasons_by_default(model: &str) -> bool {
    ["gpt-5", "gpt-6"]
        .iter()
        .any(|family| model.starts_with(family))
        && !model.contains("chat")
}

fn input(conversation: &Conversation) -> Vec<Value> {
    let mut image = conversation.image.as_ref();
    conversation
        .turns
        .iter()
        .map(|turn| match turn.role {
            Role::User => {
                let mut content = Vec::new();
                // The screenshot goes before the first question's text.
                if let Some(frame) = image.take() {
                    content
                        .push(json!({ "type": "input_image", "image_url": http::data_url(frame) }));
                }
                content.push(json!({ "type": "input_text", "text": turn.text }));
                json!({ "role": "user", "content": content })
            }
            Role::Assistant => json!({ "role": "assistant", "content": turn.text }),
        })
        .collect()
}

fn read_stream(
    body: &mut ureq::Body,
    on_text: &mut dyn FnMut(&str) -> ControlFlow<()>,
) -> Result<Completion, ProviderError> {
    let mut usage = Usage::default();
    let mut refusal = String::new();

    for event in http::json_events(body) {
        let Some(event) = event? else { break };
        let response = &event["response"];
        if let Some(input) = response["usage"]["input_tokens"].as_u64() {
            usage.input_tokens = input;
            usage.output_tokens = response["usage"]["output_tokens"].as_u64().unwrap_or(0);
        }
        match event["type"].as_str() {
            Some("response.output_text.delta") => {
                let text = event["delta"].as_str().unwrap_or_default();
                if on_text(text).is_break() {
                    return Ok(Completion {
                        stop: StopReason::Cancelled,
                        usage,
                    });
                }
            }
            Some("response.refusal.delta") => {
                refusal.push_str(event["delta"].as_str().unwrap_or_default())
            }
            Some("response.completed") => {
                let stop = if refusal.is_empty() {
                    StopReason::EndTurn
                } else {
                    StopReason::Refusal(Some(refusal))
                };
                return Ok(Completion { stop, usage });
            }
            Some("response.incomplete") => {
                let stop = match response["incomplete_details"]["reason"].as_str() {
                    Some("max_output_tokens") => StopReason::MaxTokens,
                    Some("content_filter") => StopReason::Refusal(None),
                    reason => StopReason::Other(reason.unwrap_or("incomplete").to_owned()),
                };
                return Ok(Completion { stop, usage });
            }
            Some("response.failed") => {
                let message =
                    error_message(response).unwrap_or_else(|| "the response failed".into());
                return Err(ProviderError::Api(message));
            }
            Some("error") => {
                let message = error_message(&event).unwrap_or_else(|| "unknown error".into());
                return Err(ProviderError::Api(message));
            }
            _ => {}
        }
    }

    // Every stream should end with completed, incomplete or failed.
    Err(ProviderError::Protocol(
        "the stream ended without a final status".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::EncodedFrame;
    use crate::conversation::Turn;
    use crate::providers::mock::{MockServer, Reply};

    fn conversation() -> Conversation {
        Conversation {
            id: "conversation-1".into(),
            system: "be brief".into(),
            image: Some(EncodedFrame {
                width: 1,
                height: 1,
                jpeg: vec![0xFF, 0xD8],
            }),
            turns: vec![
                Turn {
                    role: Role::User,
                    text: "boss tips?".into(),
                },
                Turn {
                    role: Role::Assistant,
                    text: "dodge left".into(),
                },
                Turn {
                    role: Role::User,
                    text: "and phase two?".into(),
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

    fn ask(
        client: &Responses,
        model: &str,
        tier: Tier,
    ) -> (Result<Completion, ProviderError>, String) {
        let mut text = String::new();
        let result = client.stream(model, tier, &conversation(), &mut |piece| {
            text.push_str(piece);
            ControlFlow::Continue(())
        });
        (result, text)
    }

    #[test]
    fn streams_text_and_reads_usage_from_the_final_event() {
        let server = MockServer::start(vec![sse(&[
            r#"{"type":"response.created","response":{"status":"in_progress"}}"#,
            r#"{"type":"response.output_text.delta","delta":"Stay "}"#,
            r#"{"type":"response.output_text.delta","delta":"mobile."}"#,
            r#"{"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":2100,"output_tokens":30}}}"#,
        ])]);
        let client = Responses::new(format!("{}/v1/responses", server.url), "sk-test".into())
            .with_session_header("x-opencode-session");

        let (result, text) = ask(&client, "gpt-6-luna", Tier::Fast);
        assert_eq!(text, "Stay mobile.");
        assert_eq!(
            result.unwrap(),
            Completion {
                stop: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 2100,
                    output_tokens: 30
                }
            }
        );

        let request = &server.requests()[0];
        assert_eq!(request.header("authorization"), Some("Bearer sk-test"));
        assert_eq!(request.header("x-opencode-session"), Some("conversation-1"));
        let body: Value = serde_json::from_str(&request.body).unwrap();
        assert_eq!(body["instructions"], "be brief");
        assert_eq!(body["store"], false);
        assert_eq!(body["reasoning"]["effort"], "low");
        let input = body["input"].as_array().unwrap();
        assert_eq!(
            input[0]["content"][0],
            json!({ "type": "input_image", "image_url": "data:image/jpeg;base64,/9g=" })
        );
        assert_eq!(
            input[0]["content"][1],
            json!({ "type": "input_text", "text": "boss tips?" })
        );
        assert_eq!(
            input[1],
            json!({ "role": "assistant", "content": "dodge left" })
        );
        assert_eq!(
            input[2]["content"].as_array().unwrap().len(),
            1,
            "image only on the first turn"
        );
    }

    #[test]
    fn leaves_reasoning_alone_where_it_does_not_apply() {
        let completed = r#"{"type":"response.completed","response":{"usage":{"input_tokens":1,"output_tokens":1}}}"#;
        let server = MockServer::start(vec![sse(&[completed]), sse(&[completed])]);
        let client = Responses::new(format!("{}/v1/responses", server.url), "sk-test".into());

        ask(&client, "gpt-6-luna", Tier::Deep).0.unwrap();
        ask(&client, "grok-4.7", Tier::Fast).0.unwrap();
        for request in server.requests() {
            let body: Value = serde_json::from_str(&request.body).unwrap();
            assert!(body.get("reasoning").is_none());
        }
    }

    #[test]
    fn reports_limits_refusals_and_failures() {
        let server = MockServer::start(vec![
            sse(&[
                r#"{"type":"response.incomplete","response":{"incomplete_details":{"reason":"max_output_tokens"}}}"#,
            ]),
            sse(&[
                r#"{"type":"response.refusal.delta","delta":"I can't help with that."}"#,
                r#"{"type":"response.completed","response":{}}"#,
            ]),
            sse(&[
                r#"{"type":"response.failed","response":{"error":{"code":"server_error","message":"boom"}}}"#,
            ]),
            sse(&[r#"{"type":"error","code":"rate_limit_exceeded","message":"slow down"}"#]),
            sse(&[r#"{"type":"response.output_text.delta","delta":"cut off"}"#]),
            Reply::json(401, r#"{"error":{"message":"Incorrect API key provided"}}"#),
        ]);
        let client = Responses::new(format!("{}/v1/responses", server.url), "sk-test".into());
        let ask = || ask(&client, "gpt-6-luna", Tier::Deep).0;

        assert_eq!(ask().unwrap().stop, StopReason::MaxTokens);
        assert_eq!(
            ask().unwrap().stop,
            StopReason::Refusal(Some("I can't help with that.".into()))
        );
        assert!(matches!(ask(), Err(ProviderError::Api(message)) if message == "boom"));
        assert!(matches!(ask(), Err(ProviderError::Api(message)) if message == "slow down"));
        assert!(matches!(ask(), Err(ProviderError::Protocol(_))));
        assert!(matches!(
            ask(),
            Err(ProviderError::Http { status: 401, .. })
        ));
    }
}
