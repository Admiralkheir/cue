//! What every API format shares while building one answer, possibly over several requests:
//! the running usage, the search tool loop, and the text sent on to the caller.

use std::borrow::Cow;
use std::ops::ControlFlow;

use serde_json::{Value, json};

use super::{Completion, Event, OnEvent, ProviderError, Search, StopReason, Usage, WebSearch};

/// The name of Cue's search tool, for models that call it through function calling.
pub const TOOL_NAME: &str = "web_search";
pub const TOOL_DESCRIPTION: &str = "Search the web. Use it for specifics about the game you \
    aren't sure of: item and boss locations, quest steps, builds, stats, patch changes. \
    Returns titles, links and snippets of the top results.";

/// Searches per answer. Calls past this are told to answer with what the model has.
pub const MAX_SEARCHES: u32 = 4;
/// Requests per answer: a backstop for a model that keeps calling the tool after being told
/// to stop, or a provider that keeps pausing its own search loop.
const MAX_ROUNDS: u32 = MAX_SEARCHES + 3;

/// Added to the system prompt whenever the model can search.
const SEARCH_HINT: &str = "You can search the web. When the answer depends on specifics you \
    aren't sure of, like where an item is, how a quest goes, a build, a stat or a recent \
    patch, search before answering instead of saying you don't know. Keep searches few and \
    focused, put the game's name in them, and still keep the answer short.";

/// JSON Schema for the tool's arguments.
pub fn tool_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": {
                "type": "string",
                "description": "What to search for. Include the game's name.",
            },
        },
        "required": ["query"],
    })
}

/// The system prompt, with a note about searching when the request carries a search tool.
pub fn system_prompt(system: &str, can_search: bool) -> Cow<'_, str> {
    if can_search {
        Cow::Owned(format!("{system}\n\n{SEARCH_HINT}"))
    } else {
        Cow::Borrowed(system)
    }
}

/// Tool arguments sent as a JSON string, as OpenAI-style APIs do.
pub fn parse_arguments(arguments: &str) -> Value {
    serde_json::from_str(arguments).unwrap_or(Value::Null)
}

pub struct Answer<'s, 'e, 'f> {
    tool: Option<&'s dyn WebSearch>,
    on_event: &'e mut OnEvent<'f>,
    usage: Usage,
    searches: u32,
    rounds: u32,
    wrote_text: bool,
    /// A search happened since the last text, so the next text starts a new paragraph.
    searched_since_text: bool,
}

impl<'s, 'e, 'f> Answer<'s, 'e, 'f> {
    pub fn new(search: Search<'s>, on_event: &'e mut OnEvent<'f>) -> Self {
        Self {
            tool: match search {
                Search::Tool(tool) => Some(tool),
                Search::Off | Search::Provider => None,
            },
            on_event,
            usage: Usage::default(),
            searches: 0,
            rounds: 0,
            wrote_text: false,
            searched_since_text: false,
        }
    }

    /// Call before each request.
    pub fn next_round(&mut self) -> Result<(), ProviderError> {
        self.rounds += 1;
        if self.rounds > MAX_ROUNDS {
            return Err(ProviderError::Protocol(
                "the model kept searching without answering".into(),
            ));
        }
        Ok(())
    }

    pub fn add_usage(&mut self, usage: Usage) {
        self.usage.add(usage);
    }

    /// Passes a piece of the answer on.
    pub fn text(&mut self, text: &str) -> ControlFlow<()> {
        if text.is_empty() {
            return ControlFlow::Continue(());
        }
        if self.wrote_text && self.searched_since_text {
            (self.on_event)(Event::Text("\n\n"))?;
        }
        self.searched_since_text = false;
        self.wrote_text = true;
        (self.on_event)(Event::Text(text))
    }

    /// The provider started one of its own searches.
    pub fn provider_search(&mut self, query: Option<&str>) -> ControlFlow<()> {
        self.searched_since_text = true;
        (self.on_event)(Event::Searching(query.filter(|query| !query.is_empty())))
    }

    /// Runs one call of Cue's search tool and returns what to tell the model, or `Break`
    /// when the caller stopped the stream. `None` when this answer has no search tool.
    pub fn call_tool(&mut self, arguments: &Value) -> Option<ControlFlow<(), String>> {
        let tool = self.tool?;
        let query = arguments["query"]
            .as_str()
            .map(str::trim)
            .unwrap_or_default();
        if query.is_empty() {
            return Some(ControlFlow::Continue(
                "The search needs a non-empty `query`.".into(),
            ));
        }
        if self.searches >= MAX_SEARCHES {
            return Some(ControlFlow::Continue(
                "No more searches for this answer. Answer with what you have.".into(),
            ));
        }
        self.searches += 1;
        self.usage.web_searches += 1;
        self.searched_since_text = true;
        if (self.on_event)(Event::Searching(Some(query))).is_break() {
            return Some(ControlFlow::Break(()));
        }
        Some(ControlFlow::Continue(tool.search(query).unwrap_or_else(
            |err| {
                format!(
                    "The search failed: {err}. Answer without it, and mention that the search \
                     failed if it matters."
                )
            },
        )))
    }

    pub fn finish(self, stop: StopReason) -> Completion {
        Completion {
            stop,
            usage: self.usage,
        }
    }
}
