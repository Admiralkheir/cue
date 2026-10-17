//! Suggests a model for each tier from the models the user's keys can reach.
//! These are only suggestions; the user always makes the final pick.

use serde::{Deserialize, Serialize};

use crate::provider::{ModelInfo, ModelRef, ProviderAccount, ProviderKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Default for quick in-game questions: low latency, low cost.
    Fast,
    /// "Think deeper" for strategy questions: slower, more capable.
    Deep,
}

/// The models one account listed.
#[derive(Debug, Clone, Copy)]
pub struct AccountModels<'a> {
    pub account: &'a ProviderAccount,
    pub models: &'a [ModelInfo],
}

/// Preferred model ids for a provider and tier, best first.
fn preferences(kind: ProviderKind, tier: Tier) -> &'static [&'static str] {
    match (kind, tier) {
        (ProviderKind::Anthropic | ProviderKind::OpenCodeZen, Tier::Fast) => {
            &["claude-haiku-5-5", "claude-haiku-4-5"]
        }
        (ProviderKind::Anthropic | ProviderKind::OpenCodeZen, Tier::Deep) => {
            &["claude-sonnet-5-5", "claude-sonnet-5", "claude-opus-5-5"]
        }
        // TODO: OpenAI, Gemini and Ollama preferences get added with their providers.
        _ => &[],
    }
}

/// Gateways like OpenCode Zen may namespace ids, e.g. `anthropic/claude-haiku-5-5`.
fn id_matches(model_id: &str, preferred: &str) -> bool {
    model_id == preferred || model_id.rsplit('/').next() == Some(preferred)
}

/// Picks the first account, in the user's order, that lists a preferred model for
/// `tier`, and within it the most preferred model.
pub fn recommend(tier: Tier, accounts: &[AccountModels<'_>]) -> Option<ModelRef> {
    accounts.iter().find_map(|entry| {
        preferences(entry.account.kind, tier)
            .iter()
            .find_map(|preferred| {
                entry
                    .models
                    .iter()
                    .find(|model| id_matches(&model.id, preferred))
                    .map(|model| ModelRef {
                        account_id: entry.account.id.clone(),
                        model: model.id.clone(),
                    })
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(id: &str, kind: ProviderKind) -> ProviderAccount {
        ProviderAccount {
            id: id.into(),
            kind,
            label: None,
            base_url: None,
        }
    }

    fn models(ids: &[&str]) -> Vec<ModelInfo> {
        ids.iter().copied().map(ModelInfo::new).collect()
    }

    #[test]
    fn picks_most_preferred_model_the_key_can_reach() {
        let anthropic = account("a", ProviderKind::Anthropic);
        let listed = models(&["claude-opus-5-5", "claude-sonnet-5", "claude-haiku-4-5"]);
        let accounts = [AccountModels {
            account: &anthropic,
            models: &listed,
        }];

        assert_eq!(
            recommend(Tier::Fast, &accounts).map(|m| m.model).as_deref(),
            Some("claude-haiku-4-5")
        );
        assert_eq!(
            recommend(Tier::Deep, &accounts).map(|m| m.model).as_deref(),
            Some("claude-sonnet-5")
        );
    }

    #[test]
    fn falls_through_to_the_next_account() {
        let openai = account("o", ProviderKind::OpenAi);
        let zen = account("z", ProviderKind::OpenCodeZen);
        let openai_models = models(&["some-openai-model"]);
        let zen_models = models(&["anthropic/claude-haiku-5-5"]);
        let accounts = [
            AccountModels {
                account: &openai,
                models: &openai_models,
            },
            AccountModels {
                account: &zen,
                models: &zen_models,
            },
        ];

        assert_eq!(
            recommend(Tier::Fast, &accounts),
            Some(ModelRef {
                account_id: "z".into(),
                model: "anthropic/claude-haiku-5-5".into()
            })
        );
    }

    #[test]
    fn no_recommendation_without_a_known_model() {
        let ollama = account("l", ProviderKind::Ollama);
        let listed = models(&["llava"]);
        assert_eq!(
            recommend(
                Tier::Fast,
                &[AccountModels {
                    account: &ollama,
                    models: &listed
                }]
            ),
            None
        );
    }
}
