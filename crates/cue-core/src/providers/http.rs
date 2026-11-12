//! HTTP plumbing shared by the provider clients.

use std::io::BufReader;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::Value;
use ureq::RequestBuilder;
use ureq::config::ConfigBuilder;
use ureq::typestate::{AgentScope, WithBody, WithoutBody};

use super::ProviderError;
use super::sse::Events;
use crate::capture::EncodedFrame;

/// Sent with every request so providers can tell Cue's traffic apart.
pub const USER_AGENT: &str = concat!("cue/", env!("CARGO_PKG_VERSION"));

/// What every request Cue makes shares; callers add the timeouts that suit them.
pub fn agent_config() -> ConfigBuilder<AgentScope> {
    ureq::Agent::config_builder()
        .user_agent(USER_AGENT)
        // Read error bodies ourselves instead of getting a bare status code.
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(15)))
}

pub fn agent() -> ureq::Agent {
    agent_config()
        .timeout_recv_response(Some(Duration::from_secs(120)))
        .build()
        .into()
}

/// Sends a JSON body. Returns the response body on success, the provider's error otherwise.
pub fn send_json(
    request: RequestBuilder<WithBody>,
    body: &Value,
) -> Result<ureq::Body, ProviderError> {
    let response = request
        .header("content-type", "application/json")
        .send(body.to_string())
        .map_err(network)?;
    let status = response.status().as_u16();
    let mut body = response.into_body();
    if status != 200 {
        return Err(http_error(status, &body.read_to_string().map_err(network)?));
    }
    Ok(body)
}

pub fn get_json(request: RequestBuilder<WithoutBody>) -> Result<Value, ProviderError> {
    let response = request.call().map_err(network)?;
    let status = response.status().as_u16();
    let body = response.into_body().read_to_string().map_err(network)?;
    if status != 200 {
        return Err(http_error(status, &body));
    }
    serde_json::from_str(&body).map_err(protocol)
}

/// The JSON payload of each server-sent event in a streamed body. A `[DONE]` marker, which
/// ends Chat Completions streams, comes through as `None`.
pub fn json_events(
    body: &mut ureq::Body,
) -> impl Iterator<Item = Result<Option<Value>, ProviderError>> + '_ {
    Events::new(BufReader::new(body.as_reader())).map(|event| {
        let event = event.map_err(|err| ProviderError::Network(err.to_string()))?;
        if event.data == "[DONE]" {
            return Ok(None);
        }
        serde_json::from_str(&event.data)
            .map(Some)
            .map_err(protocol)
    })
}

/// A readable message from an error body: `{"error":{"message":…}}`, `{"error":"…"}`,
/// `{"message":…}`, or a list of those (Gemini streams errors that way).
pub fn error_message(body: &Value) -> Option<String> {
    let body = body.get(0).unwrap_or(body);
    let error = &body["error"];
    error["message"]
        .as_str()
        .or_else(|| error.as_str())
        .or_else(|| body["message"].as_str())
        .map(str::to_owned)
}

/// The [`error_message`] in an error body, or the start of the body when it has none.
pub fn error_text(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|body| error_message(&body))
        .unwrap_or_else(|| body.chars().take(200).collect())
}

pub fn http_error(status: u16, body: &str) -> ProviderError {
    ProviderError::Http {
        status,
        message: error_text(body),
    }
}

pub fn network(err: ureq::Error) -> ProviderError {
    ProviderError::Network(err.to_string())
}

fn protocol(err: serde_json::Error) -> ProviderError {
    ProviderError::Protocol(err.to_string())
}

pub fn base64(frame: &EncodedFrame) -> String {
    BASE64.encode(&frame.jpeg)
}

pub fn data_url(frame: &EncodedFrame) -> String {
    format!("data:{};base64,{}", EncodedFrame::MEDIA_TYPE, base64(frame))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn finds_error_messages_in_every_shape() {
        let message = |body: Value| error_message(&body);
        assert_eq!(
            message(json!({ "error": { "message": "a" } })).as_deref(),
            Some("a")
        );
        assert_eq!(message(json!({ "error": "b" })).as_deref(), Some("b"));
        assert_eq!(message(json!({ "message": "c" })).as_deref(), Some("c"));
        assert_eq!(
            message(json!([{ "error": { "message": "d" } }])).as_deref(),
            Some("d")
        );
        assert_eq!(message(json!({ "nope": 1 })), None);
    }
}
