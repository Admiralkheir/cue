//! A question about one capture, plus its follow-ups.

use crate::capture::{Capture, EncodedFrame};
use crate::games::GameProfile;

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
    /// A conversation about `capture`, with the player's profile for the game if they have one.
    pub fn about(capture: &Capture, game: Option<&GameProfile>) -> Self {
        Self {
            id: crate::unique_id("conversation"),
            system: system_prompt(&capture.app_name, &capture.title, game),
            image: Some(capture.frame.clone()),
            turns: Vec::new(),
        }
    }

    /// Picks up profile changes made since the capture, like new notes.
    pub fn refresh_game(&mut self, capture: &Capture, game: Option<&GameProfile>) {
        self.system = system_prompt(&capture.app_name, &capture.title, game);
    }

    pub fn push(&mut self, role: Role, text: impl Into<String>) {
        self.turns.push(Turn {
            role,
            text: text.into(),
        });
    }
}

fn system_prompt(app_name: &str, title: &str, game: Option<&GameProfile>) -> String {
    let window = match (app_name.is_empty(), title.is_empty()) {
        (false, false) => format!("\"{app_name}\" (window title: \"{title}\")"),
        (false, true) => format!("\"{app_name}\""),
        (true, false) => format!("a window titled \"{title}\""),
        (true, true) => "their game".to_owned(),
    };
    let game_line = match game {
        Some(game) => format!("They are playing {}.", game.name),
        None => "If you can tell which game this is from the screenshot, use what you know \
                 about it."
            .to_owned(),
    };
    let mut prompt = format!(
        "You are Cue, an assistant for someone who is playing a video game right now. \
         The attached screenshot is the current frame of {window}. {game_line}\n\n\
         They read your answer in a small overlay on top of the game, mid-play, so lead with \
         the answer and keep it to a few short sentences or bullet points. Refer to what is \
         visible in the screenshot when it helps.\n\n\
         If you aren't sure about an item, stat, quest step or mechanic, say so rather than \
         guessing, and if the screenshot doesn't show enough to answer, say what you can see \
         and what you'd need."
    );
    if let Some(notes) = game
        .map(|game| game.notes.trim())
        .filter(|notes| !notes.is_empty())
    {
        prompt.push_str(
            "\n\nThe player's own notes about their game follow. Respect them, including any \
             limits on spoilers:\n<player_notes>\n",
        );
        prompt.push_str(notes);
        prompt.push_str("\n</player_notes>");
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_prompt_names_the_game_window() {
        let prompt = system_prompt("Hades II", "Hades II", None);
        assert!(prompt.contains("\"Hades II\" (window title: \"Hades II\")"));
        assert!(prompt.contains("If you can tell which game this is"));
        assert!(system_prompt("", "", None).contains("current frame of their game"));
    }

    #[test]
    fn system_prompt_carries_the_profile() {
        let mut game = GameProfile::for_window("java", "Minecraft 1.21");
        game.name = "Minecraft".into();
        let prompt = system_prompt("java", "Minecraft 1.21", Some(&game));
        assert!(prompt.contains("They are playing Minecraft."));
        assert!(!prompt.contains("player_notes"), "no notes, no notes block");

        game.notes = "Hardcore world. No spoilers past the Nether.".into();
        let prompt = system_prompt("java", "Minecraft 1.21", Some(&game));
        assert!(prompt.contains(
            "<player_notes>\nHardcore world. No spoilers past the Nether.\n</player_notes>"
        ));
    }
}
