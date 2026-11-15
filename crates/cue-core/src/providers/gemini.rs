//! The Gemini API (`generateContent`), used by Google's models and gateways serving them.

use std::ops::ControlFlow;

use serde_json::{Value, json};

use super::answer::{self, Answer, TOOL_DESCRIPTION, TOOL_NAME};
use super::http::{self, error_message};
use super::{Completion, OnEvent, Options, ProviderError, Search, StopReason, Usage, max_tokens};
use crate::capture::EncodedFrame;
use crate::conversation::{Conversation, Role};
use crate::provider::Effort;

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

    fn request_body(
        model: &str,
        options: Options<'_>,
        conversation: &Conversation,
        contents: &[Value],
    ) -> Value {
        let tools = match options.search {
            Search::Off => None,
            Search::Provider => Some(json!([{ "google_search": {} }])),
            Search::Tool(_) => Some(json!([{ "functionDeclarations": [{
                "name": TOOL_NAME,
                "description": TOOL_DESCRIPTION,
                "parameters": answer::tool_parameters(),
            }] }])),
        };
        let system = answer::system_prompt(&conversation.system, tools.is_some());
        let mut body = json!({
            "systemInstruction": { "parts": [{ "text": system }] },
            "contents": contents,
            "generationConfig": { "maxOutputTokens": max_tokens(options.effort) },
        });
        if let Some(thinking) = thinking_config(model, options.effort) {
            body["generationConfig"]["thinkingConfig"] = thinking;
        }
        if let Some(tools) = tools {
            body["tools"] = tools;
        }
        body
    }

    pub fn stream(
        &self,
        model: &str,
        options: Options<'_>,
        conversation: &Conversation,
        on_event: &mut OnEvent<'_>,
    ) -> Result<Completion, ProviderError> {
        let mut answer = Answer::new(options.search, on_event);
        let mut contents = contents(conversation);
        loop {
            answer.next_round()?;
            let body = Self::request_body(model, options, conversation, &contents);
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
            let mut response = http::send_json(request, &body)?;
            let parts = match read_stream(&mut response, &mut answer)? {
                Round::Done(stop) => return Ok(answer.finish(stop)),
                Round::ToolCalls(parts) => parts,
            };
            let mut results = Vec::new();
            for call in parts.iter().filter_map(|part| part.get("functionCall")) {
                let Some(result) = answer.call_tool(&call["args"]) else {
                    return Ok(answer.finish(StopReason::Other("function_call".into())));
                };
                let ControlFlow::Continue(result) = result else {
                    return Ok(answer.finish(StopReason::Cancelled));
                };
                let mut response = json!({ "name": TOOL_NAME, "response": { "result": result } });
                if let Some(id) = call.get("id") {
                    response["id"] = id.clone();
                }
                results.push(json!({ "functionResponse": response }));
            }
            // The model's parts go back unchanged: they carry its thought signatures.
            contents.push(json!({ "role": "model", "parts": parts }));
            contents.push(json!({ "role": "user", "parts": results }));
        }
    }
}

/// Gemini 2.x models take a thinking budget in tokens; later ones take a level.
fn thinking_config(model: &str, effort: Effort) -> Option<Value> {
    let level = effort.level()?;
    let model = model.rsplit('/').next().unwrap_or(model);
    Some(if model.starts_with("gemini-2") {
        let budget = match effort {
            Effort::Low => 1_024,
            Effort::Medium => 4_096,
            _ => 12_000,
        };
        json!({ "thinkingBudget": budget })
    } else {
        json!({ "thinkingLevel": level })
    })
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

enum Round {
    Done(StopReason),
    /// The model called Cue's search tool; these are all the parts of its turn.
    ToolCalls(Vec<Value>),
}

/// A part that is nothing but text, which can be merged with the text next to it.
fn plain_text(part: &Value) -> Option<&str> {
    let object = part.as_object()?;
    (object.len() == 1).then(|| object.get("text")?.as_str())?
}

fn read_stream(
    body: &mut ureq::Body,
    answer: &mut Answer<'_, '_, '_>,
) -> Result<Round, ProviderError> {
    let mut usage = Usage::default();
    let mut stop = None;
    let mut parts: Vec<Value> = Vec::new();

    for chunk in http::json_events(body) {
        let Some(chunk) = chunk? else { break };
        if let Some(message) = chunk.get("error").and_then(|_| error_message(&chunk)) {
            return Err(ProviderError::Api(message));
        }
        // The whole prompt was blocked, so no candidates follow.
        if let Some(reason) = chunk["promptFeedback"]["blockReason"].as_str() {
            answer.add_usage(usage);
            return Ok(Round::Done(StopReason::Refusal(Some(format!(
                "blocked: {reason}"
            )))));
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
        if let Some(queries) = candidate["groundingMetadata"]["webSearchQueries"].as_array() {
            usage.web_searches = queries.len() as u32;
        }
        let new_parts = candidate["content"]["parts"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        for part in new_parts {
            // Thought summaries are marked `thought: true`; only the answer is shown.
            if part["thought"] != true
                && let Some(text) = part["text"].as_str()
                && answer.text(text).is_break()
            {
                answer.add_usage(usage);
                return Ok(Round::Done(StopReason::Cancelled));
            }
            // Text streams in many small parts; keep it as one part per run of text.
            let last_is_text = parts.last().is_some_and(|last| plain_text(last).is_some());
            match (plain_text(part), parts.last_mut()) {
                (Some(text), Some(last)) if last_is_text => {
                    if let Value::String(existing) = &mut last["text"] {
                        existing.push_str(text);
                    }
                }
                _ => parts.push(part.clone()),
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

    answer.add_usage(usage);
    let stop = stop.ok_or_else(|| {
        ProviderError::Protocol("the stream ended without a finish reason".into())
    })?;
    if stop == StopReason::EndTurn && parts.iter().any(|part| part.get("functionCall").is_some()) {
        return Ok(Round::ToolCalls(parts));
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

    fn ask_with(
        client: &Gemini,
        model: &str,
        options: Options<'_>,
    ) -> (Result<Completion, ProviderError>, String) {
        let mut text = String::new();
        let result = client.stream(model, options, &conversation(), &mut |event| {
            if let Event::Text(piece) = event {
                text.push_str(piece);
            }
            ControlFlow::Continue(())
        });
        (result, text)
    }

    fn ask(client: &Gemini) -> (Result<Completion, ProviderError>, String) {
        ask_with(client, "gemini-3.5-flash", Options::default())
    }

    fn body(request: &crate::providers::mock::Request) -> Value {
        serde_json::from_str(&request.body).unwrap()
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
                    output_tokens: 52,
                    web_searches: 0,
                }
            }
        );

        let request = &server.requests()[0];
        assert_eq!(
            request.path,
            "/v1beta/models/gemini-3.5-flash:streamGenerateContent?alt=sse"
        );
        assert_eq!(request.header("x-goog-api-key"), Some("g-test"));
        let body = body(request);
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "be brief");
        assert!(body["generationConfig"].get("thinkingConfig").is_none());
        assert!(body.get("tools").is_none());
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
    fn sets_thinking_by_model_generation() {
        assert_eq!(
            thinking_config("gemini-3.5-flash", Effort::Medium),
            Some(json!({ "thinkingLevel": "medium" }))
        );
        assert_eq!(
            thinking_config("gemini-2.5-pro", Effort::Low),
            Some(json!({ "thinkingBudget": 1024 }))
        );
        assert_eq!(thinking_config("gemini-4-argon", Effort::Default), None);
    }

    #[test]
    fn grounds_with_google_search_and_counts_queries() {
        let server = MockServer::start(vec![sse(&[
            r#"{"candidates":[{"content":{"parts":[{"text":"It's in Limgrave."}]},"finishReason":"STOP","groundingMetadata":{"webSearchQueries":["a","b"]}}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":5}}"#,
        ])]);
        let client = Gemini::new(format!("{}/v1beta/models", server.url), "g-test".into());

        let options = Options {
            effort: Effort::High,
            search: Search::Provider,
        };
        let (result, text) = ask_with(&client, "gemini-3.5-flash", options);
        assert_eq!(text, "It's in Limgrave.");
        assert_eq!(result.unwrap().usage.web_searches, 2);
        let body = body(&server.requests()[0]);
        assert_eq!(body["tools"], json!([{ "google_search": {} }]));
        assert_eq!(
            body["generationConfig"]["thinkingConfig"],
            json!({ "thinkingLevel": "high" })
        );
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 16000);
    }

    #[test]
    fn calls_the_search_tool_and_sends_signatures_back() {
        let server = MockServer::start(vec![
            sse(&[
                r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Let me "}]}}]}"#,
                r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"check."}]}}]}"#,
                r#"{"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"web_search","args":{"query":"terraria plantera"}},"thoughtSignature":"sig"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":50,"candidatesTokenCount":5}}"#,
            ]),
            sse(&[
                r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Break a bulb."}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":80,"candidatesTokenCount":4}}"#,
            ]),
        ]);
        let client = Gemini::new(format!("{}/v1beta/models", server.url), "g-test".into());
        let search = FakeSearch::new("1. Plantera's Bulb");

        let options = Options {
            search: Search::Tool(&search),
            ..Options::default()
        };
        let (result, text) = ask_with(&client, "gemini-3.5-flash", options);
        assert_eq!(text, "Let me check.\n\nBreak a bulb.");
        assert_eq!(search.queries(), ["terraria plantera"]);
        assert_eq!(
            result.unwrap().usage,
            Usage {
                input_tokens: 130,
                output_tokens: 9,
                web_searches: 1,
            }
        );

        let requests = server.requests();
        assert_eq!(
            body(&requests[0])["tools"][0]["functionDeclarations"][0]["name"],
            "web_search"
        );
        let contents = body(&requests[1])["contents"].as_array().unwrap().clone();
        assert_eq!(
            contents[3],
            json!({ "role": "model", "parts": [
                { "text": "Let me check." },
                { "functionCall": { "name": "web_search", "args": { "query": "terraria plantera" } }, "thoughtSignature": "sig" },
            ] })
        );
        assert_eq!(
            contents[4],
            json!({ "role": "user", "parts": [{ "functionResponse": {
                "name": "web_search", "response": { "result": "1. Plantera's Bulb" },
            } }] })
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
