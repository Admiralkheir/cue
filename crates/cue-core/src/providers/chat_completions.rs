//! The OpenAI-compatible Chat Completions format, which most gateways and open-model
//! hosts speak.
//!
//! There's no common way to set reasoning effort or to search the web built into this
//! format, so those settings don't apply here, except for Cue's own search tool, which goes
//! through function calling.

use std::collections::BTreeMap;
use std::ops::ControlFlow;

use serde_json::{Value, json};

use super::answer::{self, Answer, TOOL_DESCRIPTION, TOOL_NAME};
use super::http::{self, error_message};
use super::{Completion, OnEvent, Options, ProviderError, Search, StopReason, Usage, max_tokens};
use crate::capture::EncodedFrame;
use crate::conversation::{Conversation, Role};

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
        options: Options<'_>,
        conversation: &Conversation,
        on_event: &mut OnEvent<'_>,
    ) -> Result<Completion, ProviderError> {
        let has_tool = matches!(options.search, Search::Tool(_));
        let mut answer = Answer::new(options.search, on_event);
        let mut messages = messages(conversation, has_tool);
        loop {
            answer.next_round()?;
            let mut body = json!({
                "model": model,
                "max_tokens": max_tokens(options.effort),
                "stream": true,
                "stream_options": { "include_usage": true },
                "messages": messages,
            });
            if has_tool {
                body["tools"] = json!([{
                    "type": "function",
                    "function": {
                        "name": TOOL_NAME,
                        "description": TOOL_DESCRIPTION,
                        "parameters": answer::tool_parameters(),
                    },
                }]);
            }
            let mut request = self
                .agent
                .post(&self.endpoint)
                .header("authorization", &format!("Bearer {}", self.api_key));
            if let Some(name) = self.session_header {
                request = request.header(name, &conversation.id);
            }
            let mut response = http::send_json(request, &body)?;
            let (text, calls) = match read_stream(&mut response, &mut answer)? {
                Round::Done(stop) => return Ok(answer.finish(stop)),
                Round::ToolCalls { text, calls } => (text, calls),
            };
            let mut results = Vec::new();
            for call in &calls {
                let Some(result) = answer.call_tool(&answer::parse_arguments(&call.arguments))
                else {
                    return Ok(answer.finish(StopReason::Other("tool_calls".into())));
                };
                let ControlFlow::Continue(result) = result else {
                    return Ok(answer.finish(StopReason::Cancelled));
                };
                results.push(json!({ "role": "tool", "tool_call_id": call.id, "content": result }));
            }
            messages.push(json!({
                "role": "assistant",
                "content": (!text.is_empty()).then_some(text),
                "tool_calls": calls.iter().map(|call| json!({
                    "id": call.id,
                    "type": "function",
                    "function": { "name": call.name, "arguments": call.arguments },
                })).collect::<Vec<_>>(),
            }));
            messages.extend(results);
        }
    }
}

fn messages(conversation: &Conversation, can_search: bool) -> Vec<Value> {
    let system = answer::system_prompt(&conversation.system, can_search);
    let mut messages = vec![json!({ "role": "system", "content": system })];
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
    json!({ "type": "image_url", "image_url": { "url": http::data_url(frame) } })
}

/// A tool call put together from its streamed pieces.
#[derive(Debug, Default)]
struct ToolCall {
    id: String,
    name: String,
    arguments: String,
}

enum Round {
    Done(StopReason),
    /// The model called Cue's search tool, after writing `text`.
    ToolCalls {
        text: String,
        calls: Vec<ToolCall>,
    },
}

fn read_stream(
    body: &mut ureq::Body,
    answer: &mut Answer<'_, '_, '_>,
) -> Result<Round, ProviderError> {
    let mut usage = Usage::default();
    let mut stop = None;
    let mut text = String::new();
    let mut calls: BTreeMap<u64, ToolCall> = BTreeMap::new();

    for chunk in http::json_events(body) {
        let Some(chunk) = chunk? else { break };
        if chunk.get("error").is_some() {
            let message = error_message(&chunk).unwrap_or_else(|| "unknown error".into());
            return Err(ProviderError::Api(message));
        }
        if let Some(prompt) = chunk["usage"]["prompt_tokens"].as_u64() {
            usage.input_tokens = prompt;
            usage.output_tokens = chunk["usage"]["completion_tokens"].as_u64().unwrap_or(0);
        }
        let Some(choice) = chunk["choices"].get(0) else {
            continue;
        };
        let delta = &choice["delta"];
        // Reasoning models may also stream `reasoning_content`; only the answer is shown.
        if let Some(piece) = delta["content"].as_str() {
            text.push_str(piece);
            if answer.text(piece).is_break() {
                answer.add_usage(usage);
                return Ok(Round::Done(StopReason::Cancelled));
            }
        }
        for piece in delta["tool_calls"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let call = calls
                .entry(piece["index"].as_u64().unwrap_or(0))
                .or_default();
            if let Some(id) = piece["id"].as_str() {
                call.id = id.to_owned();
            }
            if let Some(name) = piece["function"]["name"].as_str() {
                call.name.push_str(name);
            }
            if let Some(arguments) = piece["function"]["arguments"].as_str() {
                call.arguments.push_str(arguments);
            }
        }
        if let Some(reason) = choice["finish_reason"].as_str() {
            stop = Some(match reason {
                "stop" | "tool_calls" => StopReason::EndTurn,
                "length" => StopReason::MaxTokens,
                "content_filter" => StopReason::Refusal(None),
                other => StopReason::Other(other.to_owned()),
            });
        }
    }

    answer.add_usage(usage);
    // Some servers end the stream without a finish reason; treat that as a normal end.
    let stop = stop.unwrap_or(StopReason::EndTurn);
    if stop == StopReason::EndTurn && !calls.is_empty() {
        return Ok(Round::ToolCalls {
            text,
            calls: calls.into_values().collect(),
        });
    }
    Ok(Round::Done(stop))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::Turn;
    use crate::providers::Event;
    use crate::providers::mock::{FakeSearch, MockServer, Reply};

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
        let options = Options {
            search: Search::Provider,
            ..Options::default()
        };
        let completion = client
            .stream("nemotron-vl", options, &conversation(), &mut |event| {
                if let Event::Text(piece) = event {
                    text.push_str(piece);
                }
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
                    output_tokens: 9,
                    web_searches: 0,
                }
            }
        );

        let request = &server.requests()[0];
        assert_eq!(request.path, "/v1/chat/completions");
        assert_eq!(request.header("authorization"), Some("Bearer sk-test"));
        assert_eq!(request.header("x-opencode-session"), Some("conversation-1"));
        let body: Value = serde_json::from_str(&request.body).unwrap();
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert!(
            body.get("tools").is_none(),
            "no built-in search in this format"
        );
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
            client.stream("m", Options::default(), &conversation(), &mut |_| {
                ControlFlow::Continue(())
            })
        };

        assert!(
            matches!(ask(), Err(ProviderError::Http { status: 429, message }) if message == "rate limited")
        );
        assert!(matches!(ask(), Err(ProviderError::Api(message)) if message == "model overloaded"));
        assert_eq!(ask().unwrap().stop, StopReason::MaxTokens);
    }

    #[test]
    fn calls_the_search_tool_through_function_calling() {
        let server = MockServer::start(vec![
            sse(&[
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"web_search","arguments":""}}]}}]}"#,
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"query\":"}}]}}]}"#,
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"stardew rabbit foot\"}"}}]},"finish_reason":"tool_calls"}]}"#,
                r#"{"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":12}}"#,
                "[DONE]",
            ]),
            sse(&[
                r#"{"choices":[{"delta":{"content":"Rabbits drop it."},"finish_reason":"stop"}]}"#,
                r#"{"choices":[],"usage":{"prompt_tokens":150,"completion_tokens":5}}"#,
                "[DONE]",
            ]),
        ]);
        let client = ChatCompletions::new(
            format!("{}/v1/chat/completions", server.url),
            "sk-test".into(),
        );
        let search = FakeSearch::new("1. Rabbit's Foot");

        let mut text = String::new();
        let options = Options {
            search: Search::Tool(&search),
            ..Options::default()
        };
        let completion = client
            .stream("glm-5.3", options, &conversation(), &mut |event| {
                if let Event::Text(piece) = event {
                    text.push_str(piece);
                }
                ControlFlow::Continue(())
            })
            .unwrap();
        assert_eq!(text, "Rabbits drop it.");
        assert_eq!(search.queries(), ["stardew rabbit foot"]);
        assert_eq!(
            completion.usage,
            Usage {
                input_tokens: 250,
                output_tokens: 17,
                web_searches: 1,
            }
        );

        let requests = server.requests();
        let first: Value = serde_json::from_str(&requests[0].body).unwrap();
        assert_eq!(first["tools"][0]["function"]["name"], "web_search");
        assert!(
            first["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("You can search the web")
        );
        let second: Value = serde_json::from_str(&requests[1].body).unwrap();
        let messages = second["messages"].as_array().unwrap();
        assert_eq!(
            messages[4],
            json!({ "role": "assistant", "content": null, "tool_calls": [{
                "id": "call_1", "type": "function",
                "function": { "name": "web_search", "arguments": "{\"query\":\"stardew rabbit foot\"}" },
            }] })
        );
        assert_eq!(
            messages[5],
            json!({ "role": "tool", "tool_call_id": "call_1", "content": "1. Rabbit's Foot" })
        );
    }
}
