//! The OpenAI Responses API, which OpenAI's own models (and gateways serving them) use.

use std::ops::ControlFlow;

use serde_json::{Value, json};

use super::answer::{self, Answer, TOOL_DESCRIPTION, TOOL_NAME};
use super::http::{self, error_message};
use super::{Completion, OnEvent, Options, ProviderError, Search, StopReason, Usage, max_tokens};
use crate::conversation::{Conversation, Role};

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

    fn request_body(
        &self,
        model: &str,
        options: Options<'_>,
        conversation: &Conversation,
        input: &[Value],
    ) -> Value {
        let reasons = reasons(model);
        let tools = match options.search {
            Search::Off => None,
            Search::Provider => Some(json!([{ "type": "web_search" }])),
            Search::Tool(_) => Some(json!([{
                "type": "function",
                "name": TOOL_NAME,
                "description": TOOL_DESCRIPTION,
                "parameters": answer::tool_parameters(),
            }])),
        };
        let mut body = json!({
            "model": model,
            "instructions": answer::system_prompt(&conversation.system, tools.is_some()),
            "input": input,
            "max_output_tokens": max_tokens(options.effort),
            "stream": true,
            // Screenshots of someone's screen have no reason to be kept server-side.
            "store": false,
        });
        if reasons && let Some(level) = options.effort.level() {
            body["reasoning"] = json!({ "effort": level });
        }
        if let Some(tools) = tools {
            body["tools"] = tools;
            // Without storage, reasoning only carries over a tool call in encrypted form.
            if reasons && matches!(options.search, Search::Tool(_)) {
                body["include"] = json!(["reasoning.encrypted_content"]);
            }
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
        let mut input = input(conversation);
        loop {
            answer.next_round()?;
            let body = self.request_body(model, options, conversation, &input);
            let mut request = self
                .agent
                .post(&self.endpoint)
                .header("authorization", &format!("Bearer {}", self.api_key));
            if let Some(name) = self.session_header {
                request = request.header(name, &conversation.id);
            }
            let mut response = http::send_json(request, &body)?;
            let output = match read_stream(&mut response, &mut answer)? {
                Round::Done(stop) => return Ok(answer.finish(stop)),
                Round::ToolCalls(output) => output,
            };
            let mut results = Vec::new();
            for call in output.iter().filter(|item| item["type"] == "function_call") {
                let arguments =
                    answer::parse_arguments(call["arguments"].as_str().unwrap_or_default());
                let Some(result) = answer.call_tool(&arguments) else {
                    return Ok(answer.finish(StopReason::Other("function_call".into())));
                };
                let ControlFlow::Continue(result) = result else {
                    return Ok(answer.finish(StopReason::Cancelled));
                };
                results.push(json!({
                    "type": "function_call_output",
                    "call_id": call["call_id"],
                    "output": result,
                }));
            }
            // The model's own output, reasoning included, goes back ahead of the results.
            input.extend(output);
            input.extend(results);
        }
    }
}

/// Models that reason and take `reasoning.effort`: GPT-5 and later, and the o-series.
fn reasons(model: &str) -> bool {
    let gpt = ["gpt-5", "gpt-6"]
        .iter()
        .any(|family| model.starts_with(family))
        && !model.contains("chat");
    gpt || ["o1", "o3", "o4"]
        .iter()
        .any(|family| model.starts_with(family))
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

enum Round {
    Done(StopReason),
    /// The model called Cue's search tool; this is all of its output.
    ToolCalls(Vec<Value>),
}

fn read_stream(
    body: &mut ureq::Body,
    answer: &mut Answer<'_, '_, '_>,
) -> Result<Round, ProviderError> {
    let mut refusal = String::new();

    for event in http::json_events(body) {
        let Some(event) = event? else { break };
        let response = &event["response"];
        match event["type"].as_str() {
            Some("response.output_text.delta") => {
                let text = event["delta"].as_str().unwrap_or_default();
                if answer.text(text).is_break() {
                    return Ok(Round::Done(StopReason::Cancelled));
                }
            }
            Some("response.output_item.added") if event["item"]["type"] == "web_search_call" => {
                if answer.provider_search(None).is_break() {
                    return Ok(Round::Done(StopReason::Cancelled));
                }
            }
            Some("response.refusal.delta") => {
                refusal.push_str(event["delta"].as_str().unwrap_or_default())
            }
            Some("response.completed") => {
                answer.add_usage(usage(response));
                let output = response["output"].as_array().cloned().unwrap_or_default();
                if output.iter().any(|item| item["type"] == "function_call") {
                    return Ok(Round::ToolCalls(output));
                }
                return Ok(Round::Done(if refusal.is_empty() {
                    StopReason::EndTurn
                } else {
                    StopReason::Refusal(Some(refusal))
                }));
            }
            Some("response.incomplete") => {
                answer.add_usage(usage(response));
                return Ok(Round::Done(
                    match response["incomplete_details"]["reason"].as_str() {
                        Some("max_output_tokens") => StopReason::MaxTokens,
                        Some("content_filter") => StopReason::Refusal(None),
                        reason => StopReason::Other(reason.unwrap_or("incomplete").to_owned()),
                    },
                ));
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

/// Usage from a final response, counting the provider's own searches.
fn usage(response: &Value) -> Usage {
    let searches = response["output"].as_array().map_or(0, |output| {
        output
            .iter()
            // Opening or reading a page isn't a search.
            .filter(|item| {
                item["type"] == "web_search_call"
                    && matches!(item["action"]["type"].as_str(), None | Some("search"))
            })
            .count()
    });
    Usage {
        input_tokens: response["usage"]["input_tokens"].as_u64().unwrap_or(0),
        output_tokens: response["usage"]["output_tokens"].as_u64().unwrap_or(0),
        web_searches: searches as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::EncodedFrame;
    use crate::conversation::Turn;
    use crate::provider::Effort;
    use crate::providers::Event;
    use crate::providers::mock::{FakeSearch, MockServer, Reply, sse};

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

    fn effort(effort: Effort) -> Options<'static> {
        Options {
            effort,
            ..Options::default()
        }
    }

    fn ask(
        client: &Responses,
        model: &str,
        options: Options<'_>,
    ) -> (Result<Completion, ProviderError>, String, usize) {
        let mut text = String::new();
        let mut searches = 0;
        let result = client.stream(model, options, &conversation(), &mut |event| {
            match event {
                Event::Text(piece) => text.push_str(piece),
                Event::Searching(_) => searches += 1,
            }
            ControlFlow::Continue(())
        });
        (result, text, searches)
    }

    fn body(request: &crate::providers::mock::Request) -> Value {
        serde_json::from_str(&request.body).unwrap()
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

        let (result, text, _) = ask(&client, "gpt-6-luna", effort(Effort::Low));
        assert_eq!(text, "Stay mobile.");
        assert_eq!(
            result.unwrap(),
            Completion {
                stop: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 2100,
                    output_tokens: 30,
                    web_searches: 0,
                }
            }
        );

        let request = &server.requests()[0];
        assert_eq!(request.header("authorization"), Some("Bearer sk-test"));
        assert_eq!(request.header("x-opencode-session"), Some("conversation-1"));
        let body = body(request);
        assert_eq!(body["instructions"], "be brief");
        assert_eq!(body["store"], false);
        assert_eq!(body["reasoning"]["effort"], "low");
        assert!(body.get("tools").is_none());
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
        let server = MockServer::start(vec![
            sse(&[completed]),
            sse(&[completed]),
            sse(&[completed]),
        ]);
        let client = Responses::new(format!("{}/v1/responses", server.url), "sk-test".into());

        ask(&client, "gpt-6-luna", effort(Effort::Default))
            .0
            .unwrap();
        ask(&client, "grok-4.7", effort(Effort::High)).0.unwrap();
        ask(&client, "o4-mini", effort(Effort::Medium)).0.unwrap();
        let requests = server.requests();
        assert!(body(&requests[0]).get("reasoning").is_none());
        assert!(body(&requests[1]).get("reasoning").is_none());
        assert_eq!(body(&requests[2])["reasoning"]["effort"], "medium");
    }

    #[test]
    fn counts_the_providers_own_searches() {
        let server = MockServer::start(vec![sse(&[
            r#"{"type":"response.output_item.added","item":{"type":"web_search_call","id":"ws_1"}}"#,
            r#"{"type":"response.output_item.added","item":{"type":"web_search_call","id":"ws_2"}}"#,
            r#"{"type":"response.output_text.delta","delta":"Patch 1.2 nerfed it."}"#,
            r#"{"type":"response.completed","response":{"usage":{"input_tokens":9,"output_tokens":4},"output":[
                {"type":"web_search_call","action":{"type":"search","query":"x"}},
                {"type":"web_search_call","action":{"type":"open_page","url":"https://a"}},
                {"type":"message","content":[]}]}}"#,
        ])]);
        let client = Responses::new(format!("{}/v1/responses", server.url), "sk-test".into());

        let options = Options {
            search: Search::Provider,
            ..Options::default()
        };
        let (result, text, searches) = ask(&client, "gpt-6-luna", options);
        assert_eq!(text, "Patch 1.2 nerfed it.");
        assert_eq!(searches, 2);
        assert_eq!(
            result.unwrap().usage.web_searches,
            1,
            "opening a page isn't a search"
        );
        let body = body(&server.requests()[0]);
        assert_eq!(body["tools"], json!([{ "type": "web_search" }]));
        assert!(body.get("include").is_none());
    }

    #[test]
    fn calls_the_search_tool_and_sends_the_output_back() {
        let server = MockServer::start(vec![
            sse(&[
                r#"{"type":"response.completed","response":{"usage":{"input_tokens":100,"output_tokens":10},"output":[
                    {"type":"reasoning","id":"rs_1","encrypted_content":"enc"},
                    {"type":"function_call","id":"fc_1","call_id":"call_1","name":"web_search","arguments":"{\"query\":\"silksong needle upgrade\"}"}]}}"#,
            ]),
            sse(&[
                r#"{"type":"response.output_text.delta","delta":"Visit the forge."}"#,
                r#"{"type":"response.completed","response":{"usage":{"input_tokens":200,"output_tokens":20},"output":[]}}"#,
            ]),
        ]);
        let client = Responses::new(format!("{}/v1/responses", server.url), "sk-test".into());
        let search = FakeSearch::new("1. Pinmaster's forge");

        let options = Options {
            effort: Effort::Medium,
            search: Search::Tool(&search),
        };
        let (result, text, searches) = ask(&client, "gpt-6-luna", options);
        assert_eq!(text, "Visit the forge.");
        assert_eq!(searches, 1);
        assert_eq!(search.queries(), ["silksong needle upgrade"]);
        assert_eq!(
            result.unwrap().usage,
            Usage {
                input_tokens: 300,
                output_tokens: 30,
                web_searches: 1,
            }
        );

        let requests = server.requests();
        let first = body(&requests[0]);
        assert_eq!(first["tools"][0]["type"], "function");
        assert_eq!(first["tools"][0]["name"], "web_search");
        assert_eq!(first["include"], json!(["reasoning.encrypted_content"]));
        let input = body(&requests[1])["input"].as_array().unwrap().clone();
        assert_eq!(input.len(), 6);
        assert_eq!(input[3]["type"], "reasoning");
        assert_eq!(input[3]["encrypted_content"], "enc");
        assert_eq!(input[4]["call_id"], "call_1");
        assert_eq!(
            input[5],
            json!({ "type": "function_call_output", "call_id": "call_1", "output": "1. Pinmaster's forge" })
        );
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
        let ask = || ask(&client, "gpt-6-luna", Options::default()).0;

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
