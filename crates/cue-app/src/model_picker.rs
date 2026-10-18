//! The model dropdown shared by the settings window and the game profile editor.

use std::collections::HashMap;

use cue_core::{AccountModels, Config, ModelInfo, ModelRef};
use gpui_kit::component::IndexPath;
use gpui_kit::component::select::{SelectItem, SelectState};
use gpui_kit::*;

/// One entry in a model picker. `None` stands for "use the default model".
#[derive(Clone)]
pub struct ModelChoice {
    title: SharedString,
    value: Option<ModelRef>,
}

impl SelectItem for ModelChoice {
    type Value = Option<ModelRef>;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &Option<ModelRef> {
        &self.value
    }
}

pub type ModelSelect = Entity<SelectState<Vec<ModelChoice>>>;

pub fn new_select(window: &mut Window, cx: &mut App) -> ModelSelect {
    cx.new(|cx| SelectState::new(Vec::new(), None, window, cx))
}

/// The accounts whose model lists have loaded, in the user's order.
pub fn available<'a>(
    config: &'a Config,
    models: &'a HashMap<String, Vec<ModelInfo>>,
) -> Vec<AccountModels<'a>> {
    config
        .accounts
        .iter()
        .filter_map(|account| {
            let models = models.get(&account.id)?;
            Some(AccountModels { account, models })
        })
        .collect()
}

/// Every model the user's keys reach that can take a screenshot, labeled with its account.
pub fn choices(
    available: &[AccountModels<'_>],
    recommended: Option<&ModelRef>,
) -> Vec<ModelChoice> {
    available
        .iter()
        .flat_map(|entry| {
            entry
                .models
                .iter()
                .filter(|model| model.vision != Some(false))
                .map(move |model| {
                    let value = ModelRef {
                        account_id: entry.account.id.clone(),
                        model: model.id.clone(),
                    };
                    let name = model.display_name.as_deref().unwrap_or(&model.id);
                    let badge = if recommended == Some(&value) {
                        " (recommended)"
                    } else {
                        ""
                    };
                    ModelChoice {
                        title: format!("{name} · {}{badge}", entry.account.display_name()).into(),
                        value: Some(value),
                    }
                })
        })
        .collect()
}

/// A first entry that falls back to the global pick, described as `default_label`.
pub fn default_choice(default_label: &str) -> ModelChoice {
    ModelChoice {
        title: format!("Use the default ({default_label})").into(),
        value: None,
    }
}

/// Replaces the picker's entries and selects `current`.
pub fn fill(
    select: &ModelSelect,
    choices: Vec<ModelChoice>,
    current: &Option<ModelRef>,
    window: &mut Window,
    cx: &mut App,
) {
    let selected = choices
        .iter()
        .position(|choice| &choice.value == current)
        .map(IndexPath::new);
    select.update(cx, |select, cx| {
        select.set_items(choices, window, cx);
        select.set_selected_index(selected, window, cx);
    });
}
