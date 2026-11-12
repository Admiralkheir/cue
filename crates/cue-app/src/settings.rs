//! The settings window: provider keys, model picks for each tier, game profiles, overlay.

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cue_core::providers;
use cue_core::update::{self, Release};
use cue_core::{
    Config, Hotkeys, ModelInfo, OverlaySettings, ProviderAccount, ProviderKind, Tier, recommend,
    secrets,
};
use gpui_kit::component::Disableable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::select::{Select, SelectEvent};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::*;

use crate::app_state::AppState;
use crate::game_editor;
use crate::hotkeys::{self, HotkeyService};
use crate::model_picker::{self, ModelChoice, ModelSelect};
use crate::overlay::OverlayHandle;
use crate::themes::{self, THEMES};
use crate::updates::{self, UpdateStatus};

const BUG_REPORT_URL: &str =
    "https://github.com/Admiralkheir/cue/issues/new?template=bug_report.yml";
const RELEASES_URL: &str = "https://github.com/Admiralkheir/cue/releases/latest";
const MUTED: u32 = 0x9aa0a6;
const ERROR: u32 = 0xf28b82;
const WARNING: u32 = 0xfdd663;
const ACCENT: u32 = 0x8ab4f8;

/// Where an account's model list stands. Loaded lists live in `AppState::models`.
enum ModelStatus {
    Loading,
    Loaded(usize),
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
    model_status: HashMap<String, ModelStatus>,
    fast_select: ModelSelect,
    deep_select: ModelSelect,
    opacity: Entity<SliderState>,
    ask_input: Entity<InputState>,
    ask_deep_input: Entity<InputState>,
    show_input: Entity<InputState>,
    /// Bumped on every slider move, so a drag saves the config once, at the end.
    pending_saves: u64,
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
        let fast_select = model_picker::new_select(window, cx);
        let deep_select = model_picker::new_select(window, cx);
        let config = cx.global::<AppState>().config.clone();
        let opacity = cx.new(|_| {
            SliderState::new()
                .min(OverlaySettings::MIN_OPACITY)
                .max(1.0)
                .step(0.05)
                .default_value(config.overlay.opacity())
        });
        let ask_input =
            cx.new(|cx| InputState::new(window, cx).default_value(config.hotkeys.ask.clone()));
        let ask_deep_input =
            cx.new(|cx| InputState::new(window, cx).default_value(config.hotkeys.ask_deep.clone()));
        let show_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Off")
                .default_value(config.hotkeys.show.clone())
        });
        let subscriptions = vec![
            cx.subscribe_in(&fast_select, window, |this, _, event, _, cx| {
                this.on_model_picked(Tier::Fast, event, cx)
            }),
            cx.subscribe_in(&deep_select, window, |this, _, event, _, cx| {
                this.on_model_picked(Tier::Deep, event, cx)
            }),
            cx.subscribe_in(
                &opacity,
                window,
                |this, _, event: &SliderEvent, _, cx| match event {
                    SliderEvent::Change(value) | SliderEvent::Release(value) => {
                        this.set_opacity(value.start(), cx)
                    }
                },
            ),
        ];

        let mut settings = Self {
            overlay,
            new_kind: ProviderKind::Anthropic,
            key_input,
            model_status: HashMap::new(),
            fast_select,
            deep_select,
            opacity,
            ask_input,
            ask_deep_input,
            show_input,
            pending_saves: 0,
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
        self.model_status
            .insert(account.id.clone(), ModelStatus::Loading);
        cx.notify();

        let id = account.id.clone();
        let request = cx.background_spawn(async move {
            let key = secrets::api_key(&account.id).map_err(|err| err.to_string())?;
            let provider = providers::connect(&account, key).map_err(|err| err.to_string())?;
            provider.list_models().map_err(|err| err.to_string())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result: Result<Vec<ModelInfo>, String> = request.await;
            this.update_in(cx, |this, window, cx| {
                let status = match result {
                    Ok(models) => {
                        tracing::info!(account = %id, models = models.len(), "listed models");
                        let status = ModelStatus::Loaded(models.len());
                        cx.global_mut::<AppState>()
                            .models
                            .insert(id.clone(), models);
                        status
                    }
                    Err(err) => {
                        tracing::warn!(account = %id, "couldn't list models: {err}");
                        ModelStatus::Failed(err)
                    }
                };
                this.model_status.insert(id, status);
                this.apply_recommendations(cx);
                this.refresh_selects(window, cx);
                game_editor::refresh_models(cx);
            })
        })
        .detach();
    }

    /// Fills each unset tier with the recommended model. The user can change it anytime.
    fn apply_recommendations(&mut self, cx: &mut Context<Self>) {
        let state = cx.global::<AppState>();
        let available = model_picker::available(&state.config, &state.models);
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
        for (tier, select) in [
            (Tier::Fast, self.fast_select.clone()),
            (Tier::Deep, self.deep_select.clone()),
        ] {
            let state = cx.global::<AppState>();
            let available = model_picker::available(&state.config, &state.models);
            let recommended = recommend(tier, &available);
            let choices = model_picker::choices(&available, recommended.as_ref());
            let current = state.config.model_for(tier).cloned();
            model_picker::fill(&select, choices, &current, window, cx);
        }
        cx.notify();
    }

    fn on_model_picked(
        &mut self,
        tier: Tier,
        event: &SelectEvent<Vec<ModelChoice>>,
        cx: &mut Context<Self>,
    ) {
        let SelectEvent::Confirm(Some(Some(model))) = event else {
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
                    tracing::error!("couldn't store the key: {err}");
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
        state.config.forget_account(id);
        state.models.remove(id);
        self.message = state
            .save()
            .err()
            .map(|err| Message::Error(format!("Couldn't save settings: {err:#}")));
        self.model_status.remove(id);

        let id = id.to_owned();
        cx.background_spawn(async move {
            if let Err(err) = secrets::delete_api_key(&id) {
                tracing::error!(account = %id, "couldn't delete the key: {err}");
            }
        })
        .detach();
        self.apply_recommendations(cx);
        self.refresh_selects(window, cx);
        game_editor::refresh_models(cx);
    }

    fn save_now(&mut self, cx: &mut Context<Self>) {
        self.message = cx
            .global::<AppState>()
            .save()
            .err()
            .map(|err| Message::Error(format!("Couldn't save settings: {err:#}")));
        cx.notify();
    }

    fn save_soon(&mut self, cx: &mut Context<Self>) {
        self.pending_saves += 1;
        let save = self.pending_saves;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(400))
                .await;
            this.update(cx, |this, cx| {
                if this.pending_saves == save {
                    this.save_now(cx);
                }
            })
        })
        .detach();
    }

    fn set_theme(&mut self, theme: &str, cx: &mut Context<Self>) {
        cx.global_mut::<AppState>().config.overlay.theme = theme.to_owned();
        self.save_now(cx);
        // The overlay previews the change right away.
        cx.refresh_windows();
    }

    fn set_opacity(&mut self, opacity: f32, cx: &mut Context<Self>) {
        cx.global_mut::<AppState>().config.overlay.opacity = opacity;
        self.save_soon(cx);
        cx.refresh_windows();
    }

    fn apply_hotkeys(&mut self, cx: &mut Context<Self>) {
        let shortcuts = Hotkeys {
            ask: self.ask_input.read(cx).value().trim().to_owned(),
            ask_deep: self.ask_deep_input.read(cx).value().trim().to_owned(),
            show: self.show_input.read(cx).value().trim().to_owned(),
        };
        let bound = if cx.has_global::<HotkeyService>() {
            cx.update_global::<HotkeyService, _>(|service, _| service.bind(&shortcuts))
        } else {
            Err(anyhow::anyhow!("the hotkey listener isn't running"))
        };
        match bound {
            Ok(()) => {
                let show = if shortcuts.show.is_empty() {
                    "no shortcut to bring back the last conversation".to_owned()
                } else {
                    format!(
                        "{} to bring back the last conversation",
                        hotkeys::pretty(&shortcuts.show)
                    )
                };
                let message = format!(
                    "Shortcuts updated: {} for fast, {} for deep, {show}.",
                    hotkeys::pretty(&shortcuts.ask),
                    hotkeys::pretty(&shortcuts.ask_deep)
                );
                cx.global_mut::<AppState>().config.hotkeys = shortcuts;
                self.save_now(cx);
                if self.message.is_none() {
                    self.message = Some(Message::Info(message));
                }
                cx.refresh_windows();
            }
            Err(err) => {
                self.message = Some(Message::Error(format!("Kept the old shortcuts: {err:#}")));
                cx.notify();
            }
        }
    }

    fn reset_hotkeys(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let defaults = Hotkeys::default();
        self.ask_input
            .update(cx, |input, cx| input.set_value(defaults.ask, window, cx));
        self.ask_deep_input.update(cx, |input, cx| {
            input.set_value(defaults.ask_deep, window, cx)
        });
        self.show_input
            .update(cx, |input, cx| input.set_value(defaults.show, window, cx));
        self.apply_hotkeys(cx);
    }

    fn render_appearance(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let overlay = &cx.global::<AppState>().config.overlay;
        let current = themes::find(&overlay.theme).id;
        let percent = (overlay.opacity() * 100.0).round();
        let swatches = THEMES.iter().map(|theme| {
            let selected = theme.id == current;
            div()
                .id(theme.id)
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .rounded_md()
                .border_2()
                .border_color(if selected {
                    rgb(theme.accent)
                } else {
                    rgba(0xffffff26)
                })
                .bg(theme.swatch())
                .cursor_pointer()
                .child(div().size_3().rounded_full().bg(rgb(theme.accent)))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(theme.text))
                        .child(theme.name),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.set_theme(theme.id, cx)))
        });

        section("Appearance")
            .child(div().text_sm().child("Overlay theme"))
            .child(div().flex().flex_wrap().gap_2().children(swatches))
            .child(
                div()
                    .text_sm()
                    .child(format!("Overlay background opacity: {percent}%")),
            )
            .child(Slider::new(&self.opacity))
            .child(div().text_xs().text_color(rgb(MUTED)).child(
                "Drag the overlay by its header to move it; Cue remembers where you leave it.",
            ))
    }

    fn render_hotkeys(&self, cx: &mut Context<Self>) -> impl IntoElement {
        section("Hotkeys")
            .child(div().text_xs().text_color(rgb(MUTED)).child(
                "Modifiers and a key joined with +, like CmdOrCtrl+Shift+Space. Modifiers: \
                 CmdOrCtrl, Cmd, Ctrl, Alt, Shift. Keys: A–Z, 0–9, F1–F12, Space, Enter, Tab, \
                 arrow keys and more.",
            ))
            .child(div().text_sm().child("Ask (fast)"))
            .child(Input::new(&self.ask_input))
            .child(div().text_sm().child("Ask deeper"))
            .child(Input::new(&self.ask_deep_input))
            .child(div().text_sm().child("Bring back the last conversation"))
            .child(Input::new(&self.show_input))
            .child(div().text_xs().text_color(rgb(MUTED)).child(
                "Shows the overlay again after Esc, without a new capture. Leave it empty to \
                 turn it off.",
            ))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("apply-hotkeys")
                            .primary()
                            .label("Apply")
                            .on_click(cx.listener(|this, _, _, cx| this.apply_hotkeys(cx))),
                    )
                    .child(
                        Button::new("reset-hotkeys")
                            .ghost()
                            .label("Reset to defaults")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.reset_hotkeys(window, cx)),
                            ),
                    ),
            )
    }

    fn render_troubleshooting(&self) -> impl IntoElement {
        let dir = Config::log_dir();
        let location = dir.as_ref().map_or_else(
            || "This system has no log directory.".to_owned(),
            |dir| format!("Logs are kept for a week in {}", dir.display()),
        );
        section("Troubleshooting")
            .child(div().text_xs().text_color(rgb(MUTED)).child(location))
            .child(div().text_xs().text_color(rgb(MUTED)).child(
                "When reporting a bug, attach the latest log. Logs never contain API keys, \
                 screenshots, questions, answers or window titles.",
            ))
            .children(dir.map(|dir| {
                Button::new("open-logs")
                    .outline()
                    .label("Open log folder")
                    .on_click(move |_, _, cx| {
                        // The folder only exists once something has been logged.
                        let _ = std::fs::create_dir_all(&dir);
                        cx.open_with_system(&dir);
                    })
            }))
            .child(
                Button::new("report-bug")
                    .ghost()
                    .label("Report a bug on GitHub")
                    .on_click(|_, _, cx| cx.open_url(BUG_REPORT_URL)),
            )
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
            let status = match self.model_status.get(&account.id) {
                Some(ModelStatus::Loaded(count)) => div()
                    .text_color(rgb(MUTED))
                    .child(format!("{count} models")),
                Some(ModelStatus::Failed(err)) => div().text_color(rgb(ERROR)).child(err.clone()),
                Some(ModelStatus::Loading) | None => {
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

impl Settings {
    fn set_update_settings(
        &mut self,
        change: impl FnOnce(&mut cue_core::UpdateSettings),
        cx: &mut Context<Self>,
    ) {
        change(&mut cx.global_mut::<AppState>().config.updates);
        self.save_now(cx);
    }

    /// The notice at the top of the window while a newer version is waiting or installing.
    fn render_update_banner(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let status = cx.global::<AppState>().update.clone();
        let (release, text, color, can_act) = match &status {
            UpdateStatus::Available(release) => (
                release,
                format!(
                    "Cue {} is available. You have {}.",
                    release.version,
                    update::CURRENT_VERSION
                ),
                ACCENT,
                true,
            ),
            UpdateStatus::Installing(release) => (
                release,
                format!("Downloading and installing Cue {}…", release.version),
                ACCENT,
                false,
            ),
            UpdateStatus::Restarting(release) => (
                release,
                format!("Cue {} is installed. Restarting…", release.version),
                ACCENT,
                false,
            ),
            UpdateStatus::Failed {
                release: Some(release),
                error,
            } => (release, error.clone(), ERROR, true),
            _ => return None,
        };
        let blocker = update::install_blocker();
        let actions = can_act.then(|| {
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .child(update_button(release, blocker.is_some()))
                .child(
                    Button::new("release-notes")
                        .ghost()
                        .label("What's new")
                        .on_click({
                            let url = release.notes_url.clone();
                            move |_, _, cx| cx.open_url(&url)
                        }),
                )
        });
        Some(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .rounded_md()
                .border_1()
                .border_color(rgb(color))
                .child(div().text_sm().text_color(rgb(color)).child(text))
                .children(can_act.then_some(blocker).flatten().map(|reason| {
                    div().text_xs().text_color(rgb(MUTED)).child(format!(
                        "{reason} Download the new version from GitHub instead."
                    ))
                }))
                .children(actions),
        )
    }

    fn render_updates(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = cx.global::<AppState>();
        let settings = state.config.updates;
        let status = match &state.update {
            UpdateStatus::Checking => Some(("Checking for updates…".to_owned(), MUTED)),
            UpdateStatus::UpToDate => Some(("You have the latest version.".to_owned(), MUTED)),
            UpdateStatus::Failed {
                release: None,
                error,
            } => Some((error.clone(), ERROR)),
            // The banner at the top covers the rest.
            _ => None,
        };
        let busy = state.update.busy();
        let blocker = update::install_blocker();

        section("Updates")
            .child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                "You're running Cue {}. Cue asks GitHub for new releases; nothing about you \
                 is sent.",
                update::CURRENT_VERSION
            )))
            .children(
                status.map(|(text, color)| div().text_sm().text_color(rgb(color)).child(text)),
            )
            .child(
                Button::new("check-updates")
                    .outline()
                    .label("Check for updates")
                    .disabled(busy)
                    .on_click(|_, _, cx| updates::check(false, cx)),
            )
            .child(
                Checkbox::new("check-on-startup")
                    .label("Check for updates when Cue starts")
                    .checked(settings.checks_on_startup())
                    .disabled(settings.install_automatically)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        let checked = *checked;
                        this.set_update_settings(|updates| updates.check_on_startup = checked, cx)
                    })),
            )
            .child(
                Checkbox::new("install-automatically")
                    .label("Install updates automatically when Cue starts")
                    .checked(settings.install_automatically)
                    // Still lets it be turned off, which also frees "check on startup".
                    .disabled(blocker.is_some() && !settings.install_automatically)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        let checked = *checked;
                        this.set_update_settings(
                            |updates| updates.install_automatically = checked,
                            cx,
                        )
                    })),
            )
            .child(
                div().text_xs().text_color(rgb(MUTED)).child(match blocker {
                    Some(reason) => format!("{reason} New versions can be downloaded from GitHub."),
                    None => "An automatic update downloads the new version, checks it against the \
                         release's checksums and restarts Cue into it."
                        .to_owned(),
                }),
            )
    }

    fn render_games(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = cx.global::<AppState>();
        let profiles = state.config.games.iter().map(|game| {
            let mut details = vec![format!("app: {}", game.app)];
            if let Some(title) = &game.title_contains {
                details.push(format!("title contains \"{title}\""));
            }
            if game.fast_model.is_some() || game.deep_model.is_some() {
                details.push("own models".into());
            }
            if !game.notes.is_empty() {
                details.push("notes".into());
            }
            let id = game.id.clone();
            row(game.name.clone(), details.join(" · ")).child(
                Button::new(SharedString::from(format!("edit-{}", game.id)))
                    .ghost()
                    .label("Edit")
                    .on_click(move |_, _, cx| game_editor::edit_profile(&id, cx)),
            )
        });
        let recent = state
            .recent_windows
            .iter()
            .filter(|recent| state.config.game_for(&recent.app, &recent.title).is_none())
            .enumerate()
            .map(|(index, recent)| {
                let (app, title) = (recent.app.clone(), recent.title.clone());
                row(recent.app.clone(), recent.title.clone()).child(
                    Button::new(("add-game", index))
                        .outline()
                        .label("Add profile")
                        .on_click(move |_, _, cx| game_editor::edit_window(&app, &title, cx)),
                )
            })
            .collect::<Vec<_>>();
        let empty = state.config.games.is_empty() && recent.is_empty();

        section("Games")
            .child(div().text_xs().text_color(rgb(MUTED)).child(
                "A profile adds your notes to every question about a game and can pick its own \
                 models. Cue applies it whenever the game's app is the one it captures.",
            ))
            .children(profiles)
            .children((!recent.is_empty()).then(|| {
                div()
                    .text_sm()
                    .text_color(rgb(MUTED))
                    .child("Recently captured")
            }))
            .children(recent)
            .children(empty.then(|| {
                div()
                    .text_sm()
                    .text_color(rgb(MUTED))
                    .child("Capture a game with the hotkey and it shows up here.")
            }))
    }
}

/// Installs the release, or opens its download page when this copy can't update itself.
fn update_button(release: &Release, blocked: bool) -> Button {
    let button = Button::new("install-update").primary();
    if blocked {
        return button
            .label(format!("Download Cue {}", release.version))
            .on_click(|_, _, cx| cx.open_url(RELEASES_URL));
    }
    let release = release.clone();
    button
        .label(format!("Update to {}", release.version))
        .on_click(move |_, _, cx| updates::install(release.clone(), cx))
}

fn row(title: impl Into<SharedString>, detail: impl Into<SharedString>) -> Div {
    div().flex().items_center().justify_between().gap_3().child(
        div()
            .flex()
            .flex_col()
            .min_w_0()
            .child(div().truncate().child(title.into()))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .truncate()
                    .child(detail.into()),
            ),
    )
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
        let config = &cx.global::<AppState>().config;
        let (ask, ask_deep) = (
            hotkeys::pretty(&config.hotkeys.ask),
            hotkeys::pretty(&config.hotkeys.ask_deep),
        );
        let show_hint = if config.hotkeys.show.is_empty() {
            "This button brings back the last conversation without a new capture.".to_owned()
        } else {
            format!(
                "{}, or this button, brings back the last conversation without a new capture.",
                hotkeys::pretty(&config.hotkeys.show)
            )
        };
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
            .children(self.render_update_banner(cx))
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
                    .child(div().text_sm().child(format!("Fast — quick questions ({ask})")))
                    .child(Select::new(&self.fast_select).placeholder("Pick a model (add a key first if the list is empty)"))
                    .child(div().text_sm().child(format!("Deep — strategy ({ask_deep})")))
                    .child(Select::new(&self.deep_select).placeholder("Pick a model (add a key first if the list is empty)")),
            )
            .child(self.render_games(cx))
            .child(self.render_appearance(cx))
            .child(self.render_hotkeys(cx))
            .child(self.render_updates(cx))
            .child(self.render_troubleshooting())
            .child(
                section("Overlay")
                    .child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                        "Esc hides the overlay. {ask} or {ask_deep} captures the game and brings it \
                         back. Going back to the game without Esc leaves the answer on screen, with \
                         clicks passing through; a hotkey then brings that conversation back, and \
                         another press captures anew. {show_hint}"
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
