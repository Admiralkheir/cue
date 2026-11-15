//! Claude through the Anthropic Messages API, over raw HTTP (there's no official Rust SDK).

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::sync::Mutex;

use serde_json::{Value, json};

use super::answer::{self, Answer, MAX_SEARCHES, TOOL_DESCRIPTION, TOOL_NAME};
use super::http::{self, error_message};
use super::{
    Completion, OnEvent, Options, Provider, ProviderError, Search, StopReason, Usage, max_tokens,
};
use crate::capture::EncodedFrame;
use crate::conversation::{Conversation, Role};
use crate::provider::{Effort, ModelInfo};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

/// What we need to know about a model before calling it.
#[derive(Debug, Clone, Copy, Default)]
struct Capabilities {
    /// Which of `low`, `medium` and `high` the model accepts as `effort`.
    effort: [bool; 3],
}

impl Capabilities {
    fn from_model(model: &Value) -> Self {
        let effort = &model["capabilities"]["effort"];
        let level = |name: &str| effort["supported"] == true && effort[name]["supported"] == true;
        Self {
            effort: [level("low"), level("medium"), level("high")],
        }
    }

    /// Fallback for gateways that don't serve the model lookup, like OpenCode.
    fn known(model: &str) -> Self {
        // Models documented to accept `effort` from `low` to `high`.
        const EFFORT: &[&str] = &[
            "claude-haiku-5-5",
            "claude-sonnet-5-5",
            "claude-sonnet-5",
            "claude-sonnet-4-6",
            "claude-opus-5-5",
            "claude-opus-5",
            "claude-opus-4-8",
            "claude-opus-4-7",
            "claude-opus-4-6",
            "claude-fable-5-2",
            "claude-fable-5-1",
            "claude-fable-5",
        ];
        let known = EFFORT.contains(&model);
        Self { effort: [known; 3] }
    }

    fn accepts(&self, effort: Effort) -> bool {
        match effort {
            Effort::Default => false,
            Effort::Low => self.effort[0],
            Effort::Medium => self.effort[1],
            Effort::High => self.effort[2],
        }
    }
}

/// The newest web search tool the model takes. The dynamic-filtering version needs the
/// newer Opus, Sonnet and Fable models; Haiku and older models take the basic one.
fn web_search_tool(model: &str) -> &'static str {
    const DYNAMIC_FILTERING: &[&str] = &[
        "claude-fable-",
        "claude-mythos-",
        "claude-opus-5",
        "claude-opus-4-8",
        "claude-opus-4-7",
        "claude-opus-4-6",
        "claude-sonnet-5",
        "claude-sonnet-4-6",
    ];
    let model = model.rsplit('/').next().unwrap_or(model);
    if DYNAMIC_FILTERING
        .iter()
        .any(|family| model.starts_with(family))
    {
        "web_search_20260209"
    } else {
        "web_search_20250305"
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

    fn request_body(
        &self,
        model: &str,
        options: Options<'_>,
        conversation: &Conversation,
        messages: &[Value],
    ) -> Value {
        let tools = match options.search {
            Search::Off => None,
            Search::Provider => Some(json!([{
                "type": web_search_tool(model),
                "name": "web_search",
                "max_uses": MAX_SEARCHES,
            }])),
            Search::Tool(_) => Some(json!([{
                "name": TOOL_NAME,
                "description": TOOL_DESCRIPTION,
                "input_schema": answer::tool_parameters(),
            }])),
        };
        let mut body = json!({
            "model": model,
            "max_tokens": max_tokens(options.effort),
            "stream": true,
            "system": answer::system_prompt(&conversation.system, tools.is_some()),
            "messages": messages,
        });
        if let Some(tools) = tools {
            body["tools"] = tools;
        }
        if let Some(level) = options.effort.level()
            && self.capabilities(model).accepts(options.effort)
        {
            body["output_config"] = json!({ "effort": level });
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

/// Adds the model's content blocks to the conversation unchanged (thinking blocks have to
/// come back exactly as they were), continuing its turn if it already has one.
fn push_assistant(messages: &mut Vec<Value>, content: Vec<Value>) {
    if let Some(last) = messages.last_mut()
        && last["role"] == "assistant"
        && let Some(blocks) = last["content"].as_array_mut()
    {
        blocks.extend(content);
        return;
    }
    messages.push(json!({ "role": "assistant", "content": content }));
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
        options: Options<'_>,
        conversation: &Conversation,
        on_event: &mut OnEvent<'_>,
    ) -> Result<Completion, ProviderError> {
        let mut answer = Answer::new(options.search, on_event);
        let mut messages = messages(conversation);
        loop {
            answer.next_round()?;
            let body = self.request_body(model, options, conversation, &messages);
            let mut request = self
                .agent
                .post(format!("{}/v1/messages", self.base_url))
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", API_VERSION);
            if let Some(name) = self.session_header {
                request = request.header(name, &conversation.id);
            }
            let mut response = http::send_json(request, &body)?;
            let (stop, content) = read_stream(&mut response, &mut answer)?;
            match stop {
                Stop::Done(stop) => return Ok(answer.finish(stop)),
                // The provider's search loop paused; sending its turn back resumes it.
                Stop::PauseTurn => push_assistant(&mut messages, content),
                Stop::ToolUse => {
                    let mut results = Vec::new();
                    for call in content.iter().filter(|block| block["type"] == "tool_use") {
                        let Some(result) = answer.call_tool(&call["input"]) else {
                            return Ok(answer.finish(StopReason::Other("tool_use".into())));
                        };
                        let ControlFlow::Continue(result) = result else {
                            return Ok(answer.finish(StopReason::Cancelled));
                        };
                        results.push(json!({
                            "type": "tool_result",
                            "tool_use_id": call["id"],
                            "content": result,
                        }));
                    }
                    push_assistant(&mut messages, content);
                    messages.push(json!({ "role": "user", "content": results }));
                }
            }
        }
    }
}

enum Stop {
    Done(StopReason),
    /// The provider's own tool loop paused and wants the turn sent back.
    PauseTurn,
    /// The model called Cue's search tool.
    ToolUse,
}

/// Reads one response, passing text on as it comes, and returns how it stopped along with
/// its content blocks.
fn read_stream(
    body: &mut ureq::Body,
    answer: &mut Answer<'_, '_, '_>,
) -> Result<(Stop, Vec<Value>), ProviderError> {
    let mut usage = Usage::default();
    let mut stop = None;
    let mut blocks: Vec<Value> = Vec::new();
    // Tool input arrives as pieces of JSON, by block index.
    let mut inputs: HashMap<usize, String> = HashMap::new();

    let cancelled = |answer: &mut Answer<'_, '_, '_>, usage| {
        answer.add_usage(usage);
        Ok((Stop::Done(StopReason::Cancelled), Vec::new()))
    };

    for data in http::json_events(body) {
        let Some(data) = data? else { break };
        let index = data["index"].as_u64().unwrap_or(0) as usize;
        match data["type"].as_str() {
            Some("message_start") => {
                usage.input_tokens = data["message"]["usage"]["input_tokens"]
                    .as_u64()
                    .unwrap_or(0);
            }
            Some("content_block_start") => {
                if blocks.len() <= index {
                    blocks.resize(index + 1, Value::Null);
                }
                blocks[index] = data["content_block"].clone();
            }
            Some("content_block_delta") => {
                let delta = &data["delta"];
                let block = blocks.get_mut(index);
                match delta["type"].as_str() {
                    Some("text_delta") => {
                        let text = delta["text"].as_str().unwrap_or_default();
                        if let Some(block) = block {
                            append(&mut block["text"], text);
                        }
                        if answer.text(text).is_break() {
                            return cancelled(answer, usage);
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(block) = block {
                            append(
                                &mut block["thinking"],
                                delta["thinking"].as_str().unwrap_or_default(),
                            );
                        }
                    }
                    Some("signature_delta") => {
                        if let Some(block) = block {
                            block["signature"] = delta["signature"].clone();
                        }
                    }
                    Some("citations_delta") => {
                        if let Some(block) = block {
                            match block["citations"].as_array_mut() {
                                Some(citations) => citations.push(delta["citation"].clone()),
                                None => block["citations"] = json!([delta["citation"]]),
                            }
                        }
                    }
                    Some("input_json_delta") => inputs
                        .entry(index)
                        .or_default()
                        .push_str(delta["partial_json"].as_str().unwrap_or_default()),
                    _ => {}
                }
            }
            Some("content_block_stop") => {
                let Some(block) = blocks.get_mut(index) else {
                    continue;
                };
                if let Some(input) = inputs.remove(&index).filter(|input| !input.is_empty()) {
                    block["input"] = serde_json::from_str(&input).unwrap_or_else(|_| json!({}));
                }
                if block["type"] == "server_tool_use"
                    && block["name"] == "web_search"
                    && answer
                        .provider_search(block["input"]["query"].as_str())
                        .is_break()
                {
                    return cancelled(answer, usage);
                }
            }
            Some("message_delta") => {
                let delta_usage = &data["usage"];
                if let Some(output) = delta_usage["output_tokens"].as_u64() {
                    usage.output_tokens = output;
                }
                // With server tools the final count includes the search results.
                if let Some(input) = delta_usage["input_tokens"].as_u64() {
                    usage.input_tokens = input;
                }
                if let Some(searches) =
                    delta_usage["server_tool_use"]["web_search_requests"].as_u64()
                {
                    usage.web_searches = searches as u32;
                }
                stop = data["delta"]["stop_reason"]
                    .as_str()
                    .map(|reason| match reason {
                        "end_turn" | "stop_sequence" => Stop::Done(StopReason::EndTurn),
                        "max_tokens" => Stop::Done(StopReason::MaxTokens),
                        "pause_turn" => Stop::PauseTurn,
                        "tool_use" => Stop::ToolUse,
                        "refusal" => Stop::Done(StopReason::Refusal(
                            data["delta"]["stop_details"]["explanation"]
                                .as_str()
                                .map(str::to_owned),
                        )),
                        other => Stop::Done(StopReason::Other(other.to_owned())),
                    });
            }
            Some("error") => {
                let message = error_message(&data).unwrap_or_else(|| "unknown error".into());
                return Err(ProviderError::Api(message));
            }
            Some("message_stop") => break,
            // Pings carry nothing.
            _ => {}
        }
    }

    answer.add_usage(usage);
    let stop = stop
        .ok_or_else(|| ProviderError::Protocol("the stream ended without a stop reason".into()))?;
    blocks.retain(|block| !block.is_null());
    Ok((stop, blocks))
}

fn append(field: &mut Value, text: &str) {
    match field {
        Value::String(existing) => existing.push_str(text),
        _ => *field = Value::String(text.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::Turn;
    use crate::providers::Event;
    use crate::providers::mock::{FakeSearch, MockServer, Reply, sse};

    const MODEL_WITH_EFFORT: &str = r#"{"id":"claude-haiku-5-5","display_name":"Claude Haiku 5.5",
        "capabilities":{"image_input":{"supported":true},
        "effort":{"supported":true,"low":{"supported":true},"medium":{"supported":false},"high":{"supported":true}}}}"#;

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

    fn low() -> Options<'static> {
        Options {
            effort: Effort::Low,
            ..Options::default()
        }
    }

    /// Streams an answer and returns it with every search the model started.
    fn ask(
        client: &Anthropic,
        options: Options<'_>,
    ) -> (
        Result<Completion, ProviderError>,
        String,
        Vec<Option<String>>,
    ) {
        let mut text = String::new();
        let mut searches = Vec::new();
        let result = client.stream("claude-haiku-5-5", options, &conversation(), &mut |event| {
            match event {
                Event::Text(piece) => text.push_str(piece),
                Event::Searching(query) => searches.push(query.map(str::to_owned)),
            }
            ControlFlow::Continue(())
        });
        (result, text, searches)
    }

    fn body(request: &crate::providers::mock::Request) -> Value {
        serde_json::from_str(&request.body).unwrap()
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
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
                r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Take the "}}"#,
                r#"{"type":"ping"}"#,
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"north gate."}}"#,
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":42}}"#,
                r#"{"type":"message_stop"}"#,
            ]),
        ]);
        let client = Anthropic::new("sk-test".into(), Some(&server.url));

        let (result, text, searches) = ask(&client, low());
        assert_eq!(text, "Take the north gate.");
        assert!(searches.is_empty());
        assert_eq!(
            result.unwrap(),
            Completion {
                stop: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 2900,
                    output_tokens: 42,
                    web_searches: 0,
                }
            }
        );

        let requests = server.requests();
        assert_eq!(requests[0].path, "/v1/models/claude-haiku-5-5");
        let body = body(&requests[1]);
        assert_eq!(body["output_config"]["effort"], "low");
        assert_eq!(body["max_tokens"], 4096);
        assert_eq!(body["stream"], true);
        assert!(body.get("tools").is_none());
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
    fn sends_only_effort_levels_the_model_takes() {
        let done = r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#;
        let server = MockServer::start(vec![
            Reply::json(200, MODEL_WITH_EFFORT),
            sse(&[done]),
            sse(&[done]),
            sse(&[done]),
        ]);
        let client = Anthropic::new("sk-test".into(), Some(&server.url));

        for effort in [Effort::High, Effort::Medium, Effort::Default] {
            ask(
                &client,
                Options {
                    effort,
                    ..Options::default()
                },
            )
            .0
            .unwrap();
        }
        let requests = server.requests();
        assert_eq!(body(&requests[1])["output_config"]["effort"], "high");
        assert_eq!(body(&requests[1])["max_tokens"], 16000);
        assert!(
            body(&requests[2]).get("output_config").is_none(),
            "this model has no medium"
        );
        assert_eq!(body(&requests[2])["max_tokens"], 8192);
        assert!(body(&requests[3]).get("output_config").is_none());
    }

    #[test]
    fn provider_search_resumes_paused_turns() {
        let server = MockServer::start(vec![
            sse(&[
                r#"{"type":"message_start","message":{"usage":{"input_tokens":3000}}}"#,
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"srv_1","name":"web_search","input":{}}}"#,
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"query\":\"elden "}}"#,
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"ring torrent\"}"}}"#,
                r#"{"type":"content_block_stop","index":0}"#,
                r#"{"type":"content_block_start","index":1,"content_block":{"type":"web_search_tool_result","tool_use_id":"srv_1","content":[]}}"#,
                r#"{"type":"content_block_stop","index":1}"#,
                r#"{"type":"message_delta","delta":{"stop_reason":"pause_turn"},"usage":{"input_tokens":5000,"output_tokens":20,"server_tool_use":{"web_search_requests":1}}}"#,
            ]),
            sse(&[
                r#"{"type":"message_start","message":{"usage":{"input_tokens":5100}}}"#,
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Ride "}}"#,
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"citations_delta","citation":{"type":"web_search_result_location","url":"https://a"}}}"#,
                r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Torrent."}}"#,
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":30,"server_tool_use":{"web_search_requests":0}}}"#,
            ]),
        ]);
        let client = Anthropic::new("sk-test".into(), Some(&format!("{}/", server.url)));

        let options = Options {
            search: Search::Provider,
            ..Options::default()
        };
        let (result, text, searches) = ask(&client, options);
        assert_eq!(text, "Ride Torrent.", "cited pieces join without a break");
        assert_eq!(searches, [Some("elden ring torrent".to_owned())]);
        assert_eq!(
            result.unwrap().usage,
            Usage {
                input_tokens: 10_100,
                output_tokens: 50,
                web_searches: 1,
            }
        );

        let requests = server.requests();
        let first = body(&requests[0]);
        assert_eq!(
            first["tools"],
            json!([{ "type": "web_search_20250305", "name": "web_search", "max_uses": 4 }])
        );
        assert!(
            first["system"]
                .as_str()
                .unwrap()
                .contains("You can search the web")
        );
        let resumed = body(&requests[1]);
        let messages = resumed["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 4, "the paused turn goes back as is");
        assert_eq!(messages[3]["role"], "assistant");
        assert_eq!(
            messages[3]["content"][0]["input"],
            json!({ "query": "elden ring torrent" })
        );
        assert_eq!(messages[3]["content"][1]["type"], "web_search_tool_result");
    }

    #[test]
    fn calls_the_search_tool_and_sends_thinking_back() {
        let server = MockServer::start(vec![
            sse(&[
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"look it up"}}"#,
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}"#,
                r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Checking."}}"#,
                r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"tu_1","name":"web_search","input":{}}}"#,
                r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"query\":\"hades ii aspects\"}"}}"#,
                r#"{"type":"content_block_stop","index":2}"#,
                r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":10}}"#,
            ]),
            sse(&[
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Use the staff."}}"#,
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}"#,
            ]),
        ]);
        let client = Anthropic::new("sk-test".into(), Some(&server.url));
        let search = FakeSearch::new("1. Aspects of the Witch's Staff");

        let options = Options {
            search: Search::Tool(&search),
            ..Options::default()
        };
        let (result, text, searches) = ask(&client, options);
        assert_eq!(text, "Checking.\n\nUse the staff.");
        assert_eq!(searches, [Some("hades ii aspects".to_owned())]);
        assert_eq!(search.queries(), ["hades ii aspects"]);
        let completion = result.unwrap();
        assert_eq!(completion.stop, StopReason::EndTurn);
        assert_eq!(completion.usage.web_searches, 1);
        assert_eq!(completion.usage.output_tokens, 15);

        let requests = server.requests();
        let first = body(&requests[0]);
        assert_eq!(first["tools"][0]["name"], "web_search");
        assert!(first["tools"][0]["input_schema"]["properties"]["query"].is_object());
        let messages = body(&requests[1])["messages"].as_array().unwrap().clone();
        assert_eq!(
            messages[3]["content"][0],
            json!({ "type": "thinking", "thinking": "look it up", "signature": "sig" })
        );
        assert_eq!(messages[3]["content"][1]["text"], "Checking.");
        assert_eq!(
            messages[3]["content"][2]["input"],
            json!({ "query": "hades ii aspects" })
        );
        assert_eq!(
            messages[4],
            json!({ "role": "user", "content": [{
                "type": "tool_result",
                "tool_use_id": "tu_1",
                "content": "1. Aspects of the Witch's Staff",
            }] })
        );
    }

    #[test]
    fn picks_the_web_search_version_for_the_model() {
        assert_eq!(web_search_tool("claude-opus-5-5"), "web_search_20260209");
        assert_eq!(web_search_tool("claude-sonnet-4-6"), "web_search_20260209");
        assert_eq!(
            web_search_tool("anthropic/claude-fable-5-2"),
            "web_search_20260209"
        );
        assert_eq!(web_search_tool("claude-haiku-5-5"), "web_search_20250305");
        assert_eq!(web_search_tool("claude-opus-4-5"), "web_search_20250305");
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
            sse(&[
                r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":4096}}"#,
            ]),
        ]);
        let client = Anthropic::new("sk-test".into(), Some(&server.url));

        assert_eq!(
            ask(&client, low()).0.unwrap().stop,
            StopReason::Refusal(Some("policy".into()))
        );
        assert!(matches!(
            ask(&client, low()).0,
            Err(ProviderError::Http { status: 401, message }) if message == "invalid x-api-key"
        ));
        assert!(matches!(ask(&client, low()).0, Err(ProviderError::Api(m)) if m == "Overloaded"));
        assert_eq!(ask(&client, low()).0.unwrap().stop, StopReason::MaxTokens);
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
            .stream("m", Options::default(), &conversation(), &mut |event| {
                if let Event::Text(piece) = event {
                    seen.push(piece.to_owned());
                }
                ControlFlow::Break(())
            })
            .unwrap();
        assert_eq!(completion.stop, StopReason::Cancelled);
        assert_eq!(seen, ["one"]);
    }
}
