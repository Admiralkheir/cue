//! The Gemini API (`generateContent`), used by Google's models and gateways serving them.

use std::ops::ControlFlow;

use serde_json::{Value, json};

use super::http::{self, error_message};
use super::{Completion, ProviderError, StopReason, Usage, max_tokens};
use crate::capture::EncodedFrame;
use crate::conversation::{Conversation, Role};
use crate::recommend::Tier;

pub struct Gemini {
    /// The `.../v1beta/models` URL that model names are appended to.
    models_url: String,
    api_key: String,
    agent: ureq::Agent,
    /// Header that carries the conversation id, for gateways that route on it.
    session_header: Option<&'static str>,
}

impl Gemini {
    pub fn new(models_url: String, api_key: String) -> Self {
        Self {
            models_url,
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
        let body = json!({
            "systemInstruction": { "parts": [{ "text": conversation.system }] },
            "contents": contents(conversation),
            "generationConfig": { "maxOutputTokens": max_tokens(tier) },
        });
        let mut request = self
            .agent
            .post(format!(
                "{}/{model}:streamGenerateContent?alt=sse",
                self.models_url
            ))
            .header("x-goog-api-key", &self.api_key);
        if let Some(name) = self.session_header {
            request = request.header(name, &conversation.id);
        }
        let mut body = http::send_json(request, &body)?;
        read_stream(&mut body, on_text)
    }
}

fn contents(conversation: &Conversation) -> Vec<Value> {
    let mut image = conversation.image.as_ref();
    conversation
        .turns
        .iter()
        .map(|turn| match turn.role {
            Role::User => {
                let mut parts = Vec::new();
                // The screenshot goes before the first question's text.
                if let Some(frame) = image.take() {
                    parts.push(json!({
                        "inlineData": { "mimeType": EncodedFrame::MEDIA_TYPE, "data": http::base64(frame) },
                    }));
                }
                parts.push(json!({ "text": turn.text }));
                json!({ "role": "user", "parts": parts })
            }
            Role::Assistant => json!({ "role": "model", "parts": [{ "text": turn.text }] }),
        })
        .collect()
}

fn read_stream(
    body: &mut ureq::Body,
    on_text: &mut dyn FnMut(&str) -> ControlFlow<()>,
) -> Result<Completion, ProviderError> {
    let mut usage = Usage::default();
    let mut stop = None;

    for chunk in http::json_events(body) {
        let Some(chunk) = chunk? else { break };
        if let Some(message) = chunk.get("error").and_then(|_| error_message(&chunk)) {
            return Err(ProviderError::Api(message));
        }
        // The whole prompt was blocked, so no candidates follow.
        if let Some(reason) = chunk["promptFeedback"]["blockReason"].as_str() {
            return Ok(Completion {
                stop: StopReason::Refusal(Some(format!("blocked: {reason}"))),
                usage,
            });
        }
        let metadata = &chunk["usageMetadata"];
        if let Some(prompt) = metadata["promptTokenCount"].as_u64() {
            usage.input_tokens = prompt;
            // Thinking is billed as output, too.
            usage.output_tokens = metadata["candidatesTokenCount"].as_u64().unwrap_or(0)
                + metadata["thoughtsTokenCount"].as_u64().unwrap_or(0);
        }

        let Some(candidate) = chunk["candidates"].get(0) else {
            continue;
        };
        let parts = candidate["content"]["parts"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        // Thought summaries are marked `thought: true`; only the answer is shown.
        for part in parts.iter().filter(|part| part["thought"] != true) {
            if let Some(text) = part["text"].as_str().filter(|text| !text.is_empty())
                && on_text(text).is_break()
            {
                return Ok(Completion {
                    stop: StopReason::Cancelled,
                    usage,
                });
            }
        }
        if let Some(reason) = candidate["finishReason"].as_str() {
            stop = Some(match reason {
                "STOP" => StopReason::EndTurn,
                "MAX_TOKENS" => StopReason::MaxTokens,
                "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII"
                | "IMAGE_SAFETY" => StopReason::Refusal(Some(format!("stopped: {reason}"))),
                other => StopReason::Other(other.to_owned()),
            });
        }
    }

    stop.map(|stop| Completion { stop, usage })
        .ok_or_else(|| ProviderError::Protocol("the stream ended without a finish reason".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
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
                    text: "which path?".into(),
                },
                Turn {
                    role: Role::Assistant,
                    text: "the left one".into(),
                },
                Turn {
                    role: Role::User,
                    text: "why?".into(),
                },
            ],
        }
    }

    fn sse(chunks: &[&str]) -> Reply {
        let body: String = chunks
            .iter()
            .map(|data| format!("data: {data}\n\n"))
            .collect();
        Reply::new(200, "text/event-stream", body)
    }

    fn ask(client: &Gemini) -> (Result<Completion, ProviderError>, String) {
        let mut text = String::new();
        let result = client.stream(
            "gemini-3.5-flash",
            Tier::Fast,
            &conversation(),
            &mut |piece| {
                text.push_str(piece);
                ControlFlow::Continue(())
            },
        );
        (result, text)
    }

    #[test]
    fn streams_text_without_thoughts_and_sends_inline_image() {
        let server = MockServer::start(vec![sse(&[
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"weighing it","thought":true}]}}]}"#,
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"It has "}]}}]}"#,
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"the key."}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":1800,"candidatesTokenCount":12,"thoughtsTokenCount":40}}"#,
        ])]);
        let client = Gemini::new(format!("{}/v1beta/models", server.url), "g-test".into());

        let (result, text) = ask(&client);
        assert_eq!(text, "It has the key.");
        assert_eq!(
            result.unwrap(),
            Completion {
                stop: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 1800,
                    output_tokens: 52
                }
            }
        );

        let request = &server.requests()[0];
        assert_eq!(
            request.path,
            "/v1beta/models/gemini-3.5-flash:streamGenerateContent?alt=sse"
        );
        assert_eq!(request.header("x-goog-api-key"), Some("g-test"));
        let body: Value = serde_json::from_str(&request.body).unwrap();
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "be brief");
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(
            contents[0]["parts"][0]["inlineData"],
            json!({ "mimeType": "image/jpeg", "data": "/9g=" })
        );
        assert_eq!(contents[0]["parts"][1]["text"], "which path?");
        assert_eq!(
            contents[1],
            json!({ "role": "model", "parts": [{ "text": "the left one" }] })
        );
        assert_eq!(
            contents[2]["parts"].as_array().unwrap().len(),
            1,
            "image only on the first turn"
        );
    }

    #[test]
    fn reports_blocks_limits_and_errors() {
        let server = MockServer::start(vec![
            sse(&[r#"{"promptFeedback":{"blockReason":"SAFETY"}}"#]),
            sse(&[
                r#"{"candidates":[{"content":{"parts":[{"text":"cut"}]},"finishReason":"MAX_TOKENS"}]}"#,
            ]),
            sse(&[r#"{"candidates":[{"finishReason":"SAFETY"}]}"#]),
            Reply::json(
                400,
                r#"[{"error":{"code":400,"message":"API key not valid.","status":"INVALID_ARGUMENT"}}]"#,
            ),
        ]);
        let client = Gemini::new(format!("{}/v1beta/models", server.url), "g-test".into());

        assert_eq!(
            ask(&client).0.unwrap().stop,
            StopReason::Refusal(Some("blocked: SAFETY".into()))
        );
        assert_eq!(ask(&client).0.unwrap().stop, StopReason::MaxTokens);
        assert_eq!(
            ask(&client).0.unwrap().stop,
            StopReason::Refusal(Some("stopped: SAFETY".into()))
        );
        assert!(matches!(
            ask(&client).0,
            Err(ProviderError::Http { status: 400, message }) if message == "API key not valid."
        ));
    }
}
