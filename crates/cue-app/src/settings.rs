//! The settings window: provider keys, model picks for each tier, hotkeys.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use cue_core::providers;
use cue_core::{
    AccountModels, ModelInfo, ModelRef, ProviderAccount, ProviderKind, Tier, recommend, secrets,
};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::{Disableable, IndexPath};
use gpui_kit::*;

use crate::app_state::AppState;
use crate::overlay::OverlayHandle;

const MUTED: u32 = 0x9aa0a6;
const ERROR: u32 = 0xf28b82;
const WARNING: u32 = 0xfdd663;

/// One entry in a tier's model picker.
#[derive(Clone)]
struct ModelChoice {
    title: SharedString,
    value: ModelRef,
}

impl SelectItem for ModelChoice {
    type Value = ModelRef;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &ModelRef {
        &self.value
    }
}

type ModelSelect = Entity<SelectState<Vec<ModelChoice>>>;

enum Models {
    Loading,
    Loaded(Vec<ModelInfo>),
    Failed(String),
}

enum Message {
    Info(String),
    Error(String),
}

pub struct Settings {
    overlay: OverlayHandle,
    new_kind: ProviderKind,
    key_input: Entity<InputState>,
    models: HashMap<String, Models>,
    fast_select: ModelSelect,
    deep_select: ModelSelect,
    message: Option<Message>,
    _subscriptions: Vec<Subscription>,
}

impl Settings {
    pub fn new(overlay: OverlayHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let key_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("API key")
        });
        let fast_select = cx.new(|cx| SelectState::new(Vec::new(), None, window, cx));
        let deep_select = cx.new(|cx| SelectState::new(Vec::new(), None, window, cx));
        let subscriptions = vec![
            cx.subscribe_in(&fast_select, window, |this, _, event, _, cx| {
                this.on_model_picked(Tier::Fast, event, cx)
            }),
            cx.subscribe_in(&deep_select, window, |this, _, event, _, cx| {
                this.on_model_picked(Tier::Deep, event, cx)
            }),
        ];

        let mut settings = Self {
            overlay,
            new_kind: ProviderKind::Anthropic,
            key_input,
            models: HashMap::new(),
            fast_select,
            deep_select,
            message: cx
                .global::<AppState>()
                .load_error
                .clone()
                .map(Message::Error),
            _subscriptions: subscriptions,
        };
        for account in cx.global::<AppState>().config.accounts.clone() {
            settings.load_models(account, window, cx);
        }
        settings
    }

    fn load_models(
        &mut self,
        account: ProviderAccount,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.models.insert(account.id.clone(), Models::Loading);
        cx.notify();

        let id = account.id.clone();
        let request = cx.background_spawn(async move {
            let key = secrets::api_key(&account.id).map_err(|err| err.to_string())?;
            let provider = providers::connect(&account, key).map_err(|err| err.to_string())?;
            provider.list_models().map_err(|err| err.to_string())
        });
        cx.spawn_in(window, async move |this, cx| {
            let models = match request.await {
                Ok(models) => Models::Loaded(models),
                Err(err) => Models::Failed(err),
            };
            this.update_in(cx, |this, window, cx| {
                this.models.insert(id, models);
                this.apply_recommendations(cx);
                this.refresh_selects(window, cx);
            })
        })
        .detach();
    }

    fn account_models<'a>(&'a self, accounts: &'a [ProviderAccount]) -> Vec<AccountModels<'a>> {
        accounts
            .iter()
            .filter_map(|account| match self.models.get(&account.id) {
                Some(Models::Loaded(models)) => Some(AccountModels { account, models }),
                _ => None,
            })
            .collect()
    }

    /// Fills each unset tier with the recommended model. The user can change it anytime.
    fn apply_recommendations(&mut self, cx: &mut Context<Self>) {
        let accounts = cx.global::<AppState>().config.accounts.clone();
        let available = self.account_models(&accounts);
        let fast = recommend(Tier::Fast, &available);
        let deep = recommend(Tier::Deep, &available);
        // Without a stronger model that takes images, "deep" uses the fast pick.
        let deep_falls_back = deep.is_none() && fast.is_some();
        let deep = deep.or_else(|| fast.clone());

        let state = cx.global_mut::<AppState>();
        let mut picked = false;
        for (slot, pick) in [
            (&mut state.config.fast_model, fast),
            (&mut state.config.deep_model, deep),
        ] {
            if slot.is_none() && pick.is_some() {
                *slot = pick;
                picked = true;
            }
        }
        if !picked {
            return;
        }

        self.message = Some(match state.save() {
            Ok(()) if deep_falls_back => Message::Info(
                "Picked the recommended fast model. None of your keys has a stronger model \
                 that takes images, so deep answers use it too. You can change both below."
                    .into(),
            ),
            Ok(()) => {
                Message::Info("Picked the recommended models. You can change them below.".into())
            }
            Err(err) => Message::Error(format!("Couldn't save settings: {err:#}")),
        });
    }

    fn refresh_selects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let config = cx.global::<AppState>().config.clone();
        let available = self.account_models(&config.accounts);
        for (tier, select) in [
            (Tier::Fast, self.fast_select.clone()),
            (Tier::Deep, self.deep_select.clone()),
        ] {
            let recommended = recommend(tier, &available);
            let choices: Vec<ModelChoice> = available
                .iter()
                .flat_map(|entry| {
                    entry
                        .models
                        .iter()
                        // Cue always sends a screenshot.
                        .filter(|model| model.vision != Some(false))
                        .map(|model| {
                            let value = ModelRef {
                                account_id: entry.account.id.clone(),
                                model: model.id.clone(),
                            };
                            let name = model.display_name.as_deref().unwrap_or(&model.id);
                            let badge = if recommended.as_ref() == Some(&value) {
                                " (recommended)"
                            } else {
                                ""
                            };
                            let title = format!("{name} · {}{badge}", entry.account.display_name());
                            ModelChoice {
                                title: title.into(),
                                value,
                            }
                        })
                })
                .collect();
            let selected = config
                .model_for(tier)
                .and_then(|current| choices.iter().position(|choice| &choice.value == current))
                .map(IndexPath::new);
            select.update(cx, |select, cx| {
                select.set_items(choices, window, cx);
                select.set_selected_index(selected, window, cx);
            });
        }
        cx.notify();
    }

    fn on_model_picked(
        &mut self,
        tier: Tier,
        event: &SelectEvent<Vec<ModelChoice>>,
        cx: &mut Context<Self>,
    ) {
        let SelectEvent::Confirm(Some(model)) = event else {
            return;
        };
        let state = cx.global_mut::<AppState>();
        match tier {
            Tier::Fast => state.config.fast_model = Some(model.clone()),
            Tier::Deep => state.config.deep_model = Some(model.clone()),
        }
        self.message = state
            .save()
            .err()
            .map(|err| Message::Error(format!("Couldn't save settings: {err:#}")));
        cx.notify();
    }

    fn add_account(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let kind = self.new_kind;
        let key = self.key_input.read(cx).value().trim().to_owned();
        if kind.needs_api_key() && key.is_empty() {
            self.message = Some(Message::Error(format!(
                "Paste your {} API key first.",
                kind.display_name()
            )));
            cx.notify();
            return;
        }

        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |time| time.as_millis());
        let account = ProviderAccount {
            id: format!("{}-{millis}", kind.slug()),
            kind,
            label: None,
            base_url: None,
        };
        let save_key = {
            let id = account.id.clone();
            cx.background_spawn(async move {
                if key.is_empty() {
                    Ok(())
                } else {
                    secrets::set_api_key(&id, &key)
                }
            })
        };
        cx.spawn_in(window, async move |this, cx| {
            let saved = save_key.await;
            this.update_in(cx, |this, window, cx| {
                if let Err(err) = saved {
                    this.message = Some(Message::Error(format!("Couldn't store the key: {err}")));
                    cx.notify();
                    return;
                }
                this.key_input
                    .update(cx, |input, cx| input.set_value("", window, cx));
                let state = cx.global_mut::<AppState>();
                state.config.accounts.push(account.clone());
                this.message = state
                    .save()
                    .err()
                    .map(|err| Message::Error(format!("Couldn't save settings: {err:#}")));
                this.load_models(account, window, cx);
            })
        })
        .detach();
    }

    fn remove_account(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let state = cx.global_mut::<AppState>();
        state.config.accounts.retain(|account| account.id != id);
        for model in [&mut state.config.fast_model, &mut state.config.deep_model] {
            if model.as_ref().is_some_and(|model| model.account_id == id) {
                *model = None;
            }
        }
        self.message = state
            .save()
            .err()
            .map(|err| Message::Error(format!("Couldn't save settings: {err:#}")));
        self.models.remove(id);

        let id = id.to_owned();
        cx.background_spawn(async move {
            if let Err(err) = secrets::delete_api_key(&id) {
                eprintln!("cue: couldn't delete the key for {id}: {err}");
            }
        })
        .detach();
        self.apply_recommendations(cx);
        self.refresh_selects(window, cx);
    }

    fn show_overlay(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = self
            .overlay
            .update(cx, |overlay, window, cx| overlay.show(window, cx))
        {
            self.message = Some(Message::Error(format!(
                "Couldn't show the overlay: {err:#}"
            )));
            cx.notify();
        }
    }

    fn toggle_click_through(&mut self, cx: &mut Context<Self>) {
        let result = self.overlay.update(cx, |overlay, window, cx| {
            let enabled = !overlay.click_through();
            overlay.set_click_through(enabled, window, cx)
        });
        if let Ok(Err(err)) | Err(err) = result {
            self.message = Some(Message::Error(format!(
                "Couldn't change click-through: {err:#}"
            )));
        }
        cx.notify();
    }

    fn render_accounts(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let accounts = cx.global::<AppState>().config.accounts.clone();
        let rows = accounts.into_iter().map(|account| {
            let status = match self.models.get(&account.id) {
                Some(Models::Loaded(models)) => div()
                    .text_color(rgb(MUTED))
                    .child(format!("{} models", models.len())),
                Some(Models::Failed(err)) => div().text_color(rgb(ERROR)).child(err.clone()),
                Some(Models::Loading) | None => {
                    div().text_color(rgb(MUTED)).child("Loading models…")
                }
            };
            let id = account.id.clone();
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .child(account.display_name().to_owned())
                        .child(status.text_xs().truncate()),
                )
                .child(
                    Button::new(SharedString::from(format!("remove-{}", account.id)))
                        .ghost()
                        .label("Remove")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.remove_account(&id, window, cx)
                        })),
                )
        });

        let kinds = ProviderKind::ALL
            .into_iter()
            .filter(|kind| kind.needs_api_key())
            .map(|kind| {
                let supported = providers::is_supported(kind);
                let label = if supported {
                    kind.display_name().to_owned()
                } else {
                    format!("{} (soon)", kind.display_name())
                };
                let button = Button::new(kind.slug()).label(label).disabled(!supported);
                let button = if kind == self.new_kind {
                    button.primary()
                } else {
                    button.outline()
                };
                button.on_click(cx.listener(move |this, _, _, cx| {
                    this.new_kind = kind;
                    cx.notify();
                }))
            });

        section("Provider keys")
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("Keys are stored in your system keychain, never in Cue's config file."),
            )
            .children(rows)
            .child(div().flex().flex_wrap().gap_2().children(kinds))
            .children((self.new_kind == ProviderKind::OpenCodeGo).then(|| {
                div().text_xs().text_color(rgb(WARNING)).child(
                    "OpenCode Go is meant for coding agents (see opencode.ai/docs/go). Cue \
                     identifies itself honestly, but OpenCode may limit or flag game questions \
                     sent through it. OpenCode Zen or a direct provider key avoids this.",
                )
            }))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.key_input).mask_toggle()),
                    )
                    .child(
                        Button::new("add-account")
                            .primary()
                            .label("Add key")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.add_account(window, cx)),
                            ),
                    ),
            )
    }
}

fn section(title: &'static str) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
}

impl Render for Settings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hotkeys = cx.global::<AppState>().config.hotkeys.clone();
        let click_through = self.overlay.read(cx).click_through();

        div()
            .id("settings")
            .size_full()
            .overflow_y_scroll()
            .p_5()
            .flex()
            .flex_col()
            .gap_5()
            .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).child("Cue settings"))
            .children(self.message.as_ref().map(|message| match message {
                Message::Info(text) => div().text_sm().text_color(rgb(MUTED)).child(text.clone()),
                Message::Error(text) => div().text_sm().text_color(rgb(ERROR)).child(text.clone()),
            }))
            .child(self.render_accounts(cx))
            .child(
                section("Models")
                    .child(div().text_xs().text_color(rgb(MUTED)).child(
                        "Cue only suggests models. Pick any model your keys can reach that accepts images.",
                    ))
                    .child(div().text_sm().child(format!("Fast — quick questions ({})", hotkeys.ask)))
                    .child(Select::new(&self.fast_select).placeholder("Pick a model (add a key first if the list is empty)"))
                    .child(div().text_sm().child(format!("Deep — strategy ({})", hotkeys.ask_deep)))
                    .child(Select::new(&self.deep_select).placeholder("Pick a model (add a key first if the list is empty)")),
            )
            .child(
                section("Overlay")
                    .child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                        "Esc hides the overlay. {} or {} captures the game and brings it back; \
                         this button brings back the last conversation without a new capture.",
                        hotkeys.ask, hotkeys.ask_deep
                    )))
                    .child(
                        Button::new("show-overlay")
                            .primary()
                            .label("Show overlay")
                            .on_click(cx.listener(|this, _, _, cx| this.show_overlay(cx))),
                    )
                    .child(
                        Button::new("toggle-click-through")
                            .outline()
                            .label(if click_through {
                                "Clicks pass through the overlay (click to make it interactive)"
                            } else {
                                "The overlay takes clicks (click to pass them through)"
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_click_through(cx))),
                    ),
            )
    }
}
