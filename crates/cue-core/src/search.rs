//! Web search through a SearXNG instance the user runs, usually on their own machine. Free,
//! and no search provider sees who is asking.

use std::time::Duration;

use serde_json::Value;

use crate::providers::WebSearch;
use crate::providers::http;

pub const DEFAULT_SEARXNG_URL: &str = "http://localhost:8080";
/// Results handed to the model per search.
const RESULTS: usize = 6;
/// Snippets past this are cut, to keep searches cheap in tokens.
const SNIPPET_CHARS: usize = 400;

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error(
        "SearXNG doesn't serve JSON results. Add `json` under `search: formats:` in its \
         settings.yml and restart it"
    )]
    JsonDisabled,
    #[error("SearXNG returned HTTP {0}")]
    Http(u16),
    #[error("couldn't reach SearXNG at {url}: {message}")]
    Network { url: String, message: String },
    #[error("SearXNG sent something other than search results: {0}")]
    Protocol(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

pub struct SearxNg {
    base_url: String,
    agent: ureq::Agent,
}

impl SearxNg {
    pub fn new(base_url: &str) -> Self {
        let base_url = base_url.trim().trim_end_matches('/');
        Self {
            base_url: if base_url.is_empty() {
                DEFAULT_SEARXNG_URL.to_owned()
            } else {
                base_url.to_owned()
            },
            // A search holds up the answer, so don't wait long for one.
            agent: http::agent_config()
                .timeout_connect(Some(Duration::from_secs(5)))
                .timeout_global(Some(Duration::from_secs(20)))
                .build()
                .into(),
        }
    }

    pub fn results(&self, query: &str) -> Result<Vec<SearchResult>, SearchError> {
        let url = format!("{}/search", self.base_url);
        let response = self
            .agent
            .get(&url)
            .query("q", query)
            .query("format", "json")
            .call()
            .map_err(|err| SearchError::Network {
                url: self.base_url.clone(),
                message: err.to_string(),
            })?;
        match response.status().as_u16() {
            200 => {}
            403 => return Err(SearchError::JsonDisabled),
            status => return Err(SearchError::Http(status)),
        }
        let body = response
            .into_body()
            .read_to_string()
            .map_err(|err| SearchError::Network {
                url: self.base_url.clone(),
                message: err.to_string(),
            })?;
        let body: Value =
            serde_json::from_str(&body).map_err(|err| SearchError::Protocol(err.to_string()))?;
        let results = body["results"]
            .as_array()
            .ok_or_else(|| SearchError::Protocol("no `results` list".into()))?;
        Ok(results
            .iter()
            .filter_map(|result| {
                Some(SearchResult {
                    title: result["title"].as_str()?.trim().to_owned(),
                    url: result["url"].as_str()?.to_owned(),
                    snippet: result["content"]
                        .as_str()
                        .unwrap_or_default()
                        .trim()
                        .to_owned(),
                })
            })
            .take(RESULTS)
            .collect())
    }
}

/// The results as text for the model.
fn format_results(results: &[SearchResult]) -> String {
    if results.is_empty() {
        return "No results.".into();
    }
    results
        .iter()
        .enumerate()
        .map(|(index, result)| {
            let mut snippet: String = result.snippet.chars().take(SNIPPET_CHARS).collect();
            if snippet.len() < result.snippet.len() {
                snippet.push('…');
            }
            format!("{}. {}\n{}\n{snippet}", index + 1, result.title, result.url)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

impl WebSearch for SearxNg {
    fn search(&self, query: &str) -> Result<String, String> {
        self.results(query)
            .map(|results| format_results(&results))
            .map_err(|err| err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::mock::{MockServer, Reply};

    #[test]
    fn searches_and_formats_the_top_results() {
        let long = "x".repeat(500);
        let server = MockServer::start(vec![Reply::json(
            200,
            &format!(
                r#"{{"query":"q","results":[
                    {{"title":" Torrent ","url":"https://a","content":"The spectral steed."}},
                    {{"title":"No url"}},
                    {{"title":"Long","url":"https://b","content":"{long}"}}]}}"#
            ),
        )]);
        let searx = SearxNg::new(&format!("{}/", server.url));

        let text = searx.search("elden ring torrent").unwrap();
        assert!(text.starts_with("1. Torrent\nhttps://a\nThe spectral steed.\n\n2. Long\n"));
        assert!(text.ends_with(&format!("{}…", "x".repeat(400))));
        assert_eq!(
            server.requests()[0].path,
            "/search?q=elden%20ring%20torrent&format=json"
        );
    }

    #[test]
    fn explains_a_missing_json_format() {
        let server = MockServer::start(vec![
            Reply::new(403, "text/html", "Forbidden"),
            Reply::json(200, r#"{"results":[]}"#),
        ]);
        let searx = SearxNg::new(&server.url);
        assert!(matches!(searx.results("x"), Err(SearchError::JsonDisabled)));
        assert_eq!(searx.search("x").unwrap(), "No results.");
    }

    #[test]
    fn falls_back_to_the_default_url() {
        assert_eq!(SearxNg::new("  ").base_url, DEFAULT_SEARXNG_URL);
    }
}
