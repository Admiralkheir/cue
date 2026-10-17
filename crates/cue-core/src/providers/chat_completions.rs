//! The OpenAI-compatible Chat Completions format, which most gateways and open-model
//! hosts speak.

use std::io::{BufReader, Read};
use std::ops::ControlFlow;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Value, json};

use super::sse::Events;
use super::{Completion, ProviderError, StopReason, Usage, http_agent, max_tokens};
use crate::capture::EncodedFrame;
use crate::conversation::{Conversation, Role};
use crate::recommend::Tier;

pub struct ChatCompletions {
    /// The full `.../chat/completions` URL.
    endpoint: String,
    api_key: String,
    agent: ureq::Agent,
    /// Header that carries the conversation id, for gateways that route on it.
    session_header: Option<&'static str>,
}

impl ChatCompletions {
    pub fn new(endpoint: String, api_key: String) -> Self {
        Self {
            endpoint,
            api_key,
            agent: http_agent(),
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
            "model": model,
            "max_tokens": max_tokens(tier),
            "stream": true,
            "stream_options": { "include_usage": true },
            "messages": messages(conversation),
        });
        let mut request = self
            .agent
            .post(&self.endpoint)
            .header("authorization", &format!("Bearer {}", self.api_key))
            .header("content-type", "application/json");
        if let Some(name) = self.session_header {
            request = request.header(name, &conversation.id);
        }
        let response = request.send(body.to_string()).map_err(network)?;
        let status = response.status().as_u16();
        let mut body = response.into_body();
        if status != 200 {
            return Err(http_error(status, &body.read_to_string().map_err(network)?));
        }
        read_stream(body.as_reader(), on_text)
    }
}

fn messages(conversation: &Conversation) -> Vec<Value> {
    let mut messages = vec![json!({ "role": "system", "content": conversation.system })];
    let mut image = conversation.image.as_ref();
    messages.extend(conversation.turns.iter().map(|turn| match turn.role {
        Role::User => {
            let mut content = Vec::new();
            // The screenshot goes before the first question's text.
            if let Some(frame) = image.take() {
                content.push(image_part(frame));
            }
            content.push(json!({ "type": "text", "text": turn.text }));
            json!({ "role": "user", "content": content })
        }
        Role::Assistant => json!({ "role": "assistant", "content": turn.text }),
    }));
    messages
}

fn image_part(frame: &EncodedFrame) -> Value {
    let url = format!(
        "data:{};base64,{}",
        EncodedFrame::MEDIA_TYPE,
        BASE64.encode(&frame.jpeg)
    );
    json!({ "type": "image_url", "image_url": { "url": url } })
}

fn read_stream(
    reader: impl Read,
    on_text: &mut dyn FnMut(&str) -> ControlFlow<()>,
) -> Result<Completion, ProviderError> {
    let mut usage = Usage::default();
    let mut stop = None;

    for event in Events::new(BufReader::new(reader)) {
        let event = event.map_err(|err| ProviderError::Network(err.to_string()))?;
        if event.data == "[DONE]" {
            break;
        }
        let chunk: Value = serde_json::from_str(&event.data)
            .map_err(|err| ProviderError::Protocol(err.to_string()))?;
        if chunk.get("error").is_some() {
            return Err(ProviderError::Api(error_message(&chunk)));
        }
        if let Some(prompt) = chunk["usage"]["prompt_tokens"].as_u64() {
            usage.input_tokens = prompt;
            usage.output_tokens = chunk["usage"]["completion_tokens"].as_u64().unwrap_or(0);
        }
        let Some(choice) = chunk["choices"].get(0) else {
            continue;
        };
        // Reasoning models may also stream `reasoning_content`; only the answer is shown.
        if let Some(text) = choice["delta"]["content"]
            .as_str()
            .filter(|text| !text.is_empty())
            && on_text(text).is_break()
        {
            return Ok(Completion {
                stop: StopReason::Cancelled,
                usage,
            });
        }
        if let Some(reason) = choice["finish_reason"].as_str() {
            stop = Some(match reason {
                "stop" => StopReason::EndTurn,
                "length" => StopReason::MaxTokens,
                "content_filter" => StopReason::Refusal(None),
                other => StopReason::Other(other.to_owned()),
            });
        }
    }

    // Some servers end the stream without a finish reason; treat that as a normal end.
    Ok(Completion {
        stop: stop.unwrap_or(StopReason::EndTurn),
        usage,
    })
}

fn error_message(body: &Value) -> String {
    let error = &body["error"];
    error["message"]
        .as_str()
        .or_else(|| error.as_str())
        .unwrap_or("unknown error")
        .to_owned()
}

fn http_error(status: u16, body: &str) -> ProviderError {
    let message = serde_json::from_str::<Value>(body)
        .map(|body| error_message(&body))
        .unwrap_or_else(|_| body.chars().take(200).collect());
    ProviderError::Http { status, message }
}

fn network(err: ureq::Error) -> ProviderError {
    ProviderError::Network(err.to_string())
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
                    text: "what is this?".into(),
                },
                Turn {
                    role: Role::Assistant,
                    text: "a door".into(),
                },
                Turn {
                    role: Role::User,
                    text: "open it?".into(),
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

    #[test]
    fn streams_text_with_the_screenshot_as_a_data_url() {
        let server = MockServer::start(vec![sse(&[
            r#"{"choices":[{"delta":{"role":"assistant","reasoning_content":"hmm"}}]}"#,
            r#"{"choices":[{"delta":{"content":"Yes, "}}]}"#,
            r#"{"choices":[{"delta":{"content":"use the key."},"finish_reason":"stop"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":1200,"completion_tokens":9}}"#,
            "[DONE]",
        ])]);
        let client = ChatCompletions::new(
            format!("{}/v1/chat/completions", server.url),
            "sk-test".into(),
        )
        .with_session_header("x-opencode-session");

        let mut text = String::new();
        let completion = client
            .stream("nemotron-vl", Tier::Fast, &conversation(), &mut |piece| {
                text.push_str(piece);
                ControlFlow::Continue(())
            })
            .unwrap();
        assert_eq!(text, "Yes, use the key.");
        assert_eq!(
            completion,
            Completion {
                stop: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 1200,
                    output_tokens: 9
                }
            }
        );

        let request = &server.requests()[0];
        assert_eq!(request.path, "/v1/chat/completions");
        assert_eq!(request.header("authorization"), Some("Bearer sk-test"));
        assert_eq!(request.header("x-opencode-session"), Some("conversation-1"));
        let body: Value = serde_json::from_str(&request.body).unwrap();
        assert_eq!(body["stream_options"]["include_usage"], true);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(
            messages[0],
            json!({ "role": "system", "content": "be brief" })
        );
        assert_eq!(
            messages[1]["content"][0]["image_url"]["url"],
            "data:image/jpeg;base64,/9g="
        );
        assert_eq!(messages[1]["content"][1]["text"], "what is this?");
        assert_eq!(
            messages[2],
            json!({ "role": "assistant", "content": "a door" })
        );
        assert_eq!(
            messages[3]["content"].as_array().unwrap().len(),
            1,
            "image only on the first turn"
        );
    }

    #[test]
    fn reports_errors_and_length_limits() {
        let server = MockServer::start(vec![
            Reply::json(
                429,
                r#"{"error":{"message":"rate limited","type":"rate_limit"}}"#,
            ),
            sse(&[r#"{"error":{"message":"model overloaded"}}"#]),
            sse(&[r#"{"choices":[{"delta":{"content":"cut"},"finish_reason":"length"}]}"#]),
        ]);
        let client = ChatCompletions::new(
            format!("{}/v1/chat/completions", server.url),
            "sk-test".into(),
        );
        let ask = || {
            client.stream("m", Tier::Deep, &conversation(), &mut |_| {
                ControlFlow::Continue(())
            })
        };

        assert!(
            matches!(ask(), Err(ProviderError::Http { status: 429, message }) if message == "rate limited")
        );
        assert!(matches!(ask(), Err(ProviderError::Api(message)) if message == "model overloaded"));
        assert_eq!(ask().unwrap().stop, StopReason::MaxTokens);
    }
}
