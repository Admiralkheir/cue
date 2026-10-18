//! Per-game profiles: the player's notes for the model and optional model picks, matched
//! to the window Cue captured.

use serde::{Deserialize, Serialize};

use crate::provider::ModelRef;
use crate::recommend::Tier;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameProfile {
    pub id: String,
    pub name: String,
    /// The app (process) name the game runs as, compared without regard to case.
    pub app: String,
    /// Narrows the match for games that run under a shared app name, like Java or an
    /// emulator, by looking for this text in the window title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_contains: Option<String>,
    /// Passed to the model with every question about this game.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    /// Overrides the global fast model for this game.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_model: Option<ModelRef>,
    /// Overrides the global deep model for this game.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deep_model: Option<ModelRef>,
}

impl GameProfile {
    /// A new, empty profile for the captured window.
    pub fn for_window(app_name: &str, title: &str) -> Self {
        let name = if app_name.is_empty() { title } else { app_name };
        Self {
            id: crate::unique_id("game"),
            name: name.to_owned(),
            app: app_name.to_owned(),
            title_contains: None,
            notes: String::new(),
            fast_model: None,
            deep_model: None,
        }
    }

    pub fn matches(&self, app_name: &str, title: &str) -> bool {
        let title_matches = match self.title_contains.as_deref().map(str::trim) {
            Some(needle) if !needle.is_empty() => {
                title.to_lowercase().contains(&needle.to_lowercase())
            }
            _ => true,
        };
        !self.app.is_empty() && self.app.eq_ignore_ascii_case(app_name.trim()) && title_matches
    }

    fn is_narrowed(&self) -> bool {
        self.title_contains
            .as_deref()
            .is_some_and(|needle| !needle.trim().is_empty())
    }

    pub fn model_for(&self, tier: Tier) -> Option<&ModelRef> {
        match tier {
            Tier::Fast => self.fast_model.as_ref(),
            Tier::Deep => self.deep_model.as_ref(),
        }
    }
}

/// The best profile for a window. A profile that also matches the title beats one that
/// only matches the app, so one Java profile can't shadow a Minecraft one.
pub fn find_profile<'a>(
    profiles: &'a [GameProfile],
    app_name: &str,
    title: &str,
) -> Option<&'a GameProfile> {
    profiles
        .iter()
        .filter(|profile| profile.matches(app_name, title))
        .max_by_key(|profile| profile.is_narrowed())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(name: &str, app: &str, title_contains: Option<&str>) -> GameProfile {
        GameProfile {
            title_contains: title_contains.map(str::to_owned),
            name: name.to_owned(),
            ..GameProfile::for_window(app, "")
        }
    }

    #[test]
    fn matches_the_app_without_regard_to_case() {
        let profiles = [profile("Hades II", "Hades II", None)];
        assert_eq!(
            find_profile(&profiles, "hades ii", "Hades II").map(|p| p.name.as_str()),
            Some("Hades II")
        );
        assert!(find_profile(&profiles, "Hades", "Hades II").is_none());
    }

    #[test]
    fn prefers_the_profile_that_also_matches_the_title() {
        let profiles = [
            profile("Any Java app", "java", None),
            profile("Minecraft", "java", Some("minecraft")),
        ];
        let name = |title| find_profile(&profiles, "java", title).map(|p| p.name.as_str());
        assert_eq!(name("Minecraft 1.21.4"), Some("Minecraft"));
        assert_eq!(name("Some Tool"), Some("Any Java app"));
    }

    #[test]
    fn a_blank_title_filter_matches_any_title() {
        let profiles = [profile("Game", "game", Some("  "))];
        assert!(find_profile(&profiles, "game", "whatever").is_some());
    }

    #[test]
    fn new_profiles_are_named_after_the_app() {
        let game = GameProfile::for_window("Hollow Knight", "Hollow Knight");
        assert_eq!(
            (game.name.as_str(), game.app.as_str()),
            ("Hollow Knight", "Hollow Knight")
        );
        assert_eq!(GameProfile::for_window("", "Untitled").name, "Untitled");
        assert_ne!(game.id, GameProfile::for_window("Hollow Knight", "").id);
    }
}
