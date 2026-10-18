//! A small window for editing one game profile: its name, how it's matched to a window,
//! the player's notes and optional models. Changes are kept as a draft until Save.

use cue_core::{GameProfile, ModelRef, Tier};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::select::{Select, SelectEvent};
use gpui_kit::*;

use crate::app_state::AppState;
use crate::model_picker::{self, ModelChoice, ModelSelect};

const MUTED: u32 = 0x9aa0a6;
const ERROR: u32 = 0xf28b82;

enum Message {
    Info(String),
    Error(String),
}

pub struct GameEditor {
    draft: GameProfile,
    /// Not in the config yet; Save adds it.
    is_new: bool,
    name: Entity<InputState>,
    app: Entity<InputState>,
    title: Entity<InputState>,
    notes: Entity<TextareaState>,
    fast: ModelSelect,
    deep: ModelSelect,
    message: Option<Message>,
    _subscriptions: Vec<Subscription>,
}

impl GameEditor {
    fn new(draft: GameProfile, is_new: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Game name"));
        let app = cx.new(|cx| InputState::new(window, cx).placeholder("App name"));
        let title = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Optional: text in the window title")
        });
        let notes = cx.new(|cx| {
            TextareaState::new(window, cx).auto_grow(5, 14).placeholder(
                "Difficulty, your build or class, where you are, how much you want spoiled…",
            )
        });
        let fast = model_picker::new_select(window, cx);
        let deep = model_picker::new_select(window, cx);
        let subscriptions = vec![
            cx.subscribe_in(&fast, window, |this, _, event, _, _| {
                this.on_model_picked(Tier::Fast, event)
            }),
            cx.subscribe_in(&deep, window, |this, _, event, _, _| {
                this.on_model_picked(Tier::Deep, event)
            }),
        ];

        let mut editor = Self {
            draft: draft.clone(),
            is_new,
            name,
            app,
            title,
            notes,
            fast,
            deep,
            message: None,
            _subscriptions: subscriptions,
        };
        editor.load(draft, is_new, window, cx);
        editor
    }

    /// Shows `draft` in the form, replacing whatever was being edited.
    fn load(
        &mut self,
        draft: GameProfile,
        is_new: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title_contains = draft.title_contains.clone().unwrap_or_default();
        self.name.update(cx, |input, cx| {
            input.set_value(draft.name.clone(), window, cx)
        });
        self.app.update(cx, |input, cx| {
            input.set_value(draft.app.clone(), window, cx)
        });
        self.title
            .update(cx, |input, cx| input.set_value(title_contains, window, cx));
        self.notes.update(cx, |input, cx| {
            input.set_value(draft.notes.clone(), window, cx)
        });
        self.draft = draft;
        self.is_new = is_new;
        self.message = None;
        self.fill_models(window, cx);
    }

    fn fill_models(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (tier, select) in [
            (Tier::Fast, self.fast.clone()),
            (Tier::Deep, self.deep.clone()),
        ] {
            let state = cx.global::<AppState>();
            let available = model_picker::available(&state.config, &state.models);
            let default_label = state
                .config
                .model_for(tier)
                .map_or_else(|| "none picked yet".to_owned(), |model| model.model.clone());
            let mut choices = vec![model_picker::default_choice(&default_label)];
            choices.extend(model_picker::choices(&available, None));
            let current = self.draft.model_for(tier).cloned();
            model_picker::fill(&select, choices, &current, window, cx);
        }
        cx.notify();
    }

    fn on_model_picked(&mut self, tier: Tier, event: &SelectEvent<Vec<ModelChoice>>) {
        let SelectEvent::Confirm(Some(model)) = event else {
            return;
        };
        let slot: &mut Option<ModelRef> = match tier {
            Tier::Fast => &mut self.draft.fast_model,
            Tier::Deep => &mut self.draft.deep_model,
        };
        *slot = model.clone();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).value().trim().to_owned();
        let app = self.app.read(cx).value().trim().to_owned();
        if name.is_empty() || app.is_empty() {
            self.message = Some(Message::Error(
                "A profile needs a name and an app to match.".into(),
            ));
            cx.notify();
            return;
        }
        let title_contains = self.title.read(cx).value().trim().to_owned();
        self.draft.name = name;
        self.draft.app = app;
        self.draft.title_contains = (!title_contains.is_empty()).then_some(title_contains);
        self.draft.notes = self.notes.read(cx).value().trim().to_owned();

        let draft = self.draft.clone();
        let state = cx.global_mut::<AppState>();
        match state.config.game_mut(&draft.id) {
            Some(existing) => *existing = draft,
            None => state.config.games.push(draft),
        }
        self.message = Some(match state.save() {
            Ok(()) => {
                self.is_new = false;
                Message::Info("Saved. Cue uses it from the next question on.".into())
            }
            Err(err) => Message::Error(format!("Couldn't save: {err:#}")),
        });
        // The settings window and the overlay show profiles too.
        cx.refresh_windows();
    }

    fn delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.draft.id.clone();
        let state = cx.global_mut::<AppState>();
        state.config.games.retain(|game| game.id != id);
        if let Err(err) = state.save() {
            self.message = Some(Message::Error(format!("Couldn't save: {err:#}")));
            cx.notify();
            return;
        }
        cx.refresh_windows();
        window.remove_window();
    }
}

fn field(label: &'static str) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_sm().child(label))
}

fn hint(text: &'static str) -> Div {
    div().text_xs().text_color(rgb(MUTED)).child(text)
}

impl Render for GameEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("game-editor")
            .size_full()
            .overflow_y_scroll()
            .p_5()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap_2()
                    .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).child("Game profile"))
                    .children(self.is_new.then(|| hint("new, not saved yet"))),
            )
            .child(field("Name").child(Input::new(&self.name)))
            .child(
                field("App")
                    .child(Input::new(&self.app))
                    .child(hint("The app Cue captured; the profile applies whenever it's in front.")),
            )
            .child(
                field("Window title contains")
                    .child(Input::new(&self.title))
                    .child(hint("For games that share an app, like Java or an emulator. Leave empty otherwise.")),
            )
            .child(
                field("Notes for Cue")
                    .child(Textarea::new(&self.notes))
                    .child(hint("Sent with every question about this game.")),
            )
            .child(field("Fast model").child(Select::new(&self.fast)))
            .child(field("Deep model").child(Select::new(&self.deep)))
            .children(self.message.as_ref().map(|message| match message {
                Message::Info(text) => div().text_sm().text_color(rgb(MUTED)).child(text.clone()),
                Message::Error(text) => div().text_sm().text_color(rgb(ERROR)).child(text.clone()),
            }))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("save-game")
                            .primary()
                            .label("Save")
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    )
                    .children((!self.is_new).then(|| {
                        Button::new("delete-game")
                            .ghost()
                            .label("Delete profile")
                            .on_click(cx.listener(|this, _, window, cx| this.delete(window, cx)))
                    })),
            )
    }
}

/// The editor window, reused for every profile so there's only ever one.
#[derive(Default)]
struct EditorWindow(Option<(AnyWindowHandle, WeakEntity<GameEditor>)>);

impl Global for EditorWindow {}

/// Opens the profile for a captured window, or a new draft for it.
pub fn edit_window(app: &str, title: &str, cx: &mut App) {
    let (profile, is_new) = match cx.global::<AppState>().config.game_for(app, title) {
        Some(game) => (game.clone(), false),
        None => (GameProfile::for_window(app, title), true),
    };
    open(profile, is_new, cx);
}

pub fn edit_profile(id: &str, cx: &mut App) {
    if let Some(game) = cx.global::<AppState>().config.game(id).cloned() {
        open(game, false, cx);
    }
}

fn open(profile: GameProfile, is_new: bool, cx: &mut App) {
    cx.activate(true);
    let existing = cx
        .try_global::<EditorWindow>()
        .and_then(|editor| editor.0.clone())
        .and_then(|(window, view)| Some((window, view.upgrade()?)));
    if let Some((window, view)) = existing {
        let profile = profile.clone();
        let reused = window.update(cx, |_, window, cx| {
            window.activate_window();
            view.update(cx, |editor, cx| editor.load(profile, is_new, window, cx));
        });
        if reused.is_ok() {
            return;
        }
    }

    let bounds = Bounds::centered(None, size(px(480.), px(640.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        ..Default::default()
    };
    match gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| GameEditor::new(profile, is_new, window, cx))
    }) {
        Ok((window, view)) => cx.set_global(EditorWindow(Some((window, view.downgrade())))),
        Err(err) => tracing::error!("couldn't open the game profile: {err:#}"),
    }
}

/// Refills the open editor's model pickers after the model lists change.
pub fn refresh_models(cx: &mut App) {
    let Some((window, view)) = cx
        .try_global::<EditorWindow>()
        .and_then(|editor| editor.0.clone())
        .and_then(|(window, view)| Some((window, view.upgrade()?)))
    else {
        return;
    };
    window
        .update(cx, |_, window, cx| {
            view.update(cx, |editor, cx| editor.fill_models(window, cx))
        })
        .ok();
}
