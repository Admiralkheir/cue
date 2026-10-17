//! A question about one capture, plus its follow-ups.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::capture::{Capture, EncodedFrame};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub role: Role,
    pub text: String,
}

/// The screenshot goes with the first user turn; follow-ups reuse it.
#[derive(Debug, Clone)]
pub struct Conversation {
    /// Stable for the whole conversation; gateways use it for routing and caching.
    pub id: String,
    pub system: String,
    pub image: Option<EncodedFrame>,
    pub turns: Vec<Turn>,
}

impl Conversation {
    pub fn about(capture: &Capture) -> Self {
        Self {
            id: new_id(),
            system: system_prompt(&capture.app_name, &capture.title),
            image: Some(capture.frame.clone()),
            turns: Vec::new(),
        }
    }

    pub fn push(&mut self, role: Role, text: impl Into<String>) {
        self.turns.push(Turn {
            role,
            text: text.into(),
        });
    }
}

fn new_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_nanos());
    format!("cue-{nanos:x}-{:x}", NEXT.fetch_add(1, Ordering::Relaxed))
}

fn system_prompt(app_name: &str, title: &str) -> String {
    let window = match (app_name.is_empty(), title.is_empty()) {
        (false, false) => format!("\"{app_name}\" (window title: \"{title}\")"),
        (false, true) => format!("\"{app_name}\""),
        (true, false) => format!("a window titled \"{title}\""),
        (true, true) => "their game".to_owned(),
    };
    format!(
        "You are Cue, an assistant for someone who is playing a video game right now. \
         The attached screenshot is the current frame of {window}.\n\n\
         They read your answer in a small overlay on top of the game, mid-play, so lead with \
         the answer and keep it to a few short sentences or bullet points. Refer to what is \
         visible in the screenshot when it helps.\n\n\
         If you aren't sure about an item, stat, quest step or mechanic, say so rather than \
         guessing, and if the screenshot doesn't show enough to answer, say what you can see \
         and what you'd need."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_prompt_names_the_game_window() {
        let prompt = system_prompt("Hades II", "Hades II");
        assert!(prompt.contains("\"Hades II\" (window title: \"Hades II\")"));
        assert!(system_prompt("", "").contains("current frame of their game"));
    }
}
