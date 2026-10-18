//! The translucent panel that floats above the game: capture, question, streamed answer.

use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use cue_core::providers::{self, Completion, ProviderError, StopReason};
use cue_core::{
    Capture, Conversation, OverlayPosition, ProviderAccount, Role, Tier, capture_focused_window,
    secrets,
};
use futures::StreamExt;
use futures::channel::mpsc;
use gpui_kit::base::Root;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::text::TextView;
use gpui_kit::*;

use crate::app_state::AppState;
use crate::themes::{self, OverlayTheme};
use crate::{game_editor, hotkeys, platform};

const SIZE: Size<Pixels> = Size {
    width: px(420.),
    height: px(600.),
};
const MARGIN: Pixels = px(24.);
/// How long the overlay has to sit still after a drag before its position is saved.
const SAVE_POSITION_AFTER: Duration = Duration::from_millis(500);

enum Status {
    Idle,
    Capturing(Tier),
    Failed(String),
    Session(Box<Session>),
}

/// One capture and the questions asked about it.
struct Session {
    id: u64,
    tier: Tier,
    capture: Capture,
    thumbnail: Arc<Image>,
    conversation: Conversation,
    /// The answer still streaming in; it joins `conversation` once complete.
    pending: Option<Pending>,
    note: Option<Note>,
}

struct Pending {
    text: String,
    cancel: Arc<AtomicBool>,
}

enum Note {
    Info(String),
    Error(String),
}

pub struct Overlay {
    click_through: bool,
    status: Status,
    next_session_id: u64,
    input: Entity<InputState>,
    scroll: ScrollHandle,
    /// Bumped on every move, so only the last move of a drag gets saved.
    position_saves: u64,
    _subscriptions: Vec<Subscription>,
}

impl Overlay {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Ask about this screen…"));
        let on_enter =
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.submit(window, cx);
                }
            });
        let on_move = cx.observe_window_bounds(window, |this, window, cx| {
            this.save_position_later(window, cx)
        });
        Self {
            click_through: true,
            status: Status::Idle,
            next_session_id: 0,
            input,
            scroll: ScrollHandle::new(),
            position_saves: 0,
            _subscriptions: vec![on_enter, on_move],
        }
    }

    /// Remembers where the player dragged the overlay, once it has stopped moving.
    fn save_position_later(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.position_saves += 1;
        let save = self.position_saves;
        let origin = window.bounds().origin;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_POSITION_AFTER).await;
            this.update(cx, |this, cx| {
                if this.position_saves != save {
                    return;
                }
                let state = cx.global_mut::<AppState>();
                let position = OverlayPosition {
                    x: f32::from(origin.x),
                    y: f32::from(origin.y),
                };
                if state.config.overlay.position == Some(position) {
                    return;
                }
                state.config.overlay.position = Some(position);
                if let Err(err) = state.save() {
                    eprintln!("cue: couldn't save the overlay position: {err:#}");
                }
            })
        })
        .detach();
    }

    pub fn click_through(&self) -> bool {
        self.click_through
    }

    pub fn set_click_through(
        &mut self,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        platform::set_click_through(window, enabled)?;
        self.click_through = enabled;
        cx.notify();
        Ok(())
    }

    fn session_mut(&mut self, id: u64) -> Option<&mut Session> {
        match &mut self.status {
            Status::Session(session) if session.id == id => Some(session.as_mut()),
            _ => None,
        }
    }

    /// Captures the game, then brings the overlay up for a question to the `tier` model.
    pub fn ask(&mut self, tier: Tier, window: &mut Window, cx: &mut Context<Self>) {
        match &self.status {
            Status::Capturing(_) => return,
            Status::Session(session) => {
                if let Some(pending) = &session.pending {
                    pending.cancel.store(true, Ordering::Relaxed);
                }
            }
            _ => {}
        }
        self.status = Status::Capturing(tier);
        cx.notify();

        // Capture before showing the overlay: on Windows, focusing it first would make
        // the overlay the foreground window instead of the game.
        let own_pid = std::process::id();
        // Read on the main thread, where AppKit keeps it current.
        let frontmost_pid = platform::frontmost_app_pid();
        let capture =
            cx.background_spawn(async move { capture_focused_window(own_pid, frontmost_pid) });
        cx.spawn_in(window, async move |this, cx| {
            let capture = capture.await;
            this.update_in(cx, |overlay, window, cx| {
                overlay.start_session(tier, capture, window, cx)
            })
        })
        .detach();
    }

    fn start_session(
        &mut self,
        tier: Tier,
        capture: Result<Capture, cue_core::CaptureError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.status = match capture {
            Ok(capture) => {
                self.next_session_id += 1;
                let jpeg = capture.frame.jpeg.clone();
                let state = cx.global_mut::<AppState>();
                state.remember_window(&capture.app_name, &capture.title);
                let game = state.config.game_for(&capture.app_name, &capture.title);
                let conversation = Conversation::about(&capture, game);
                // The settings window lists recently captured windows.
                cx.refresh_windows();
                Status::Session(Box::new(Session {
                    id: self.next_session_id,
                    tier,
                    conversation,
                    thumbnail: Arc::new(Image::from_bytes(ImageFormat::Jpeg, jpeg)),
                    capture,
                    pending: None,
                    note: None,
                }))
            }
            Err(err) => Status::Failed(err.to_string()),
        };

        let shown = platform::show_and_focus(window)
            .and_then(|()| self.set_click_through(false, window, cx));
        if let Err(err) = shown {
            eprintln!("cue: couldn't bring up the overlay: {err:#}");
        }
        self.input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();

        // The OS may refuse to hand Cue the keyboard. Clicking the panel still works, since
        // a click gives a non-activating panel the keyboard without activating Cue.
        let id = self.next_session_id;
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(400))
                .await;
            this.update_in(cx, |overlay, window, cx| {
                if platform::has_keyboard(window) {
                    return;
                }
                if let Some(session) = overlay.session_mut(id) {
                    session.note =
                        Some(Note::Info("Click the question box to start typing.".into()));
                    cx.notify();
                }
            })
        })
        .detach();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let question = self.input.read(cx).value().trim().to_owned();
        let Status::Session(session) = &mut self.status else {
            return;
        };
        if question.is_empty() || session.pending.is_some() {
            return;
        }

        let config = &cx.global::<AppState>().config;
        let game = config.game_for(&session.capture.app_name, &session.capture.title);
        // Notes may have changed since the capture.
        session.conversation.refresh_game(&session.capture, game);
        let target = config.model_for_game(session.tier, game).and_then(|model| {
            Some((
                config.account(&model.account_id)?.clone(),
                model.model.clone(),
            ))
        });
        let Some((account, model)) = target else {
            let tier = match session.tier {
                Tier::Fast => "fast",
                Tier::Deep => "deep",
            };
            session.note = Some(Note::Error(format!(
                "Pick a {tier} model in Cue's settings first."
            )));
            cx.notify();
            return;
        };

        session.conversation.push(Role::User, question);
        let cancel = Arc::new(AtomicBool::new(false));
        session.pending = Some(Pending {
            text: String::new(),
            cancel: cancel.clone(),
        });
        session.note = None;
        let (id, tier, conversation) = (session.id, session.tier, session.conversation.clone());
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.scroll.scroll_to_bottom();
        cx.notify();

        let (deltas_tx, mut deltas) = mpsc::unbounded::<String>();
        // Shown under the answer, so it's clear which key paid for it.
        let label = format!("{model} · {}", account.display_name());
        let request = cx.background_spawn(async move {
            request_answer(&account, &model, tier, &conversation, &cancel, &deltas_tx)
                .map(|completion| (label, completion))
        });
        cx.spawn_in(window, async move |this, cx| {
            while let Some(piece) = deltas.next().await {
                if this
                    .update(cx, |overlay, cx| overlay.append(id, &piece, cx))
                    .is_err()
                {
                    return;
                }
            }
            let result = request.await;
            this.update_in(cx, |overlay, window, cx| {
                overlay.finish(id, result, window, cx)
            })
            .ok();
        })
        .detach();
    }

    fn append(&mut self, id: u64, piece: &str, cx: &mut Context<Self>) {
        if let Some(pending) = self
            .session_mut(id)
            .and_then(|session| session.pending.as_mut())
        {
            pending.text.push_str(piece);
            self.scroll.scroll_to_bottom();
            cx.notify();
        }
    }

    fn finish(
        &mut self,
        id: u64,
        result: Result<(String, Completion), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session_mut(id) else {
            return;
        };
        let Some(pending) = session.pending.take() else {
            return;
        };

        let completion = match result {
            Ok((label, completion)) => Some((label, completion)),
            Err(message) => {
                session.note = Some(Note::Error(message));
                None
            }
        };
        let answered = matches!(&completion, Some((_, c)) if c.stop != StopReason::Cancelled)
            && !pending.text.is_empty();
        if answered {
            session.conversation.push(Role::Assistant, pending.text);
        } else if let Some(question) = session.conversation.turns.pop() {
            // No answer: put the question back so it can be retried or edited.
            let question = question.text;
            self.input
                .update(cx, |input, cx| input.set_value(question, window, cx));
        }

        let Some(session) = self.session_mut(id) else {
            return;
        };
        if let Some((label, Completion { stop, usage })) = completion {
            session.note = match stop {
                StopReason::EndTurn => Some(Note::Info(format!(
                    "{label} · {} in / {} out tokens",
                    usage.input_tokens, usage.output_tokens
                ))),
                StopReason::MaxTokens => {
                    Some(Note::Error("The answer hit the length limit.".into()))
                }
                StopReason::Refusal(explanation) => Some(Note::Error(match explanation {
                    Some(explanation) => format!("The model declined to answer: {explanation}"),
                    None => "The model declined to answer.".into(),
                })),
                StopReason::Cancelled => None,
                StopReason::Other(reason) => Some(Note::Info(format!("Stopped: {reason}"))),
            };
        }
        cx.notify();
    }

    /// Brings the overlay back without a new capture. With a capture on hand it takes the
    /// keyboard, so the conversation can go on; otherwise it just shows the hint.
    pub fn show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shown = if matches!(self.status, Status::Session(_)) {
            platform::show_and_focus(window)
                .and_then(|()| self.set_click_through(false, window, cx))
                .map(|()| self.input.update(cx, |input, cx| input.focus(window, cx)))
        } else {
            platform::show(window)
        };
        if let Err(err) = shown {
            eprintln!("cue: couldn't show the overlay: {err:#}");
        }
    }

    /// Hides the overlay and hands the keyboard back to the game.
    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let hidden = platform::hide(window).and_then(|()| self.set_click_through(true, window, cx));
        if let Err(err) = hidden {
            eprintln!("cue: couldn't hide the overlay: {err:#}");
        }
    }

    fn render_session(
        &self,
        session: &Session,
        theme: &OverlayTheme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let capture = &session.capture;
        let game = cx
            .global::<AppState>()
            .config
            .game_for(&capture.app_name, &capture.title);
        // A profile names the game; otherwise show what was captured.
        let title = match game {
            Some(game) => game.name.clone(),
            None if capture.title.is_empty() || capture.title == capture.app_name => {
                capture.app_name.clone()
            }
            None => format!("{} — {}", capture.app_name, capture.title),
        };
        let notes_label = match game {
            Some(game) if !game.notes.is_empty() => "Edit notes",
            Some(_) => "Add notes",
            None => "Add a profile",
        };
        let (app, window_title) = (capture.app_name.clone(), capture.title.clone());
        let turns = session
            .conversation
            .turns
            .iter()
            .enumerate()
            .map(|(index, turn)| match turn.role {
                Role::User => div()
                    .text_sm()
                    .text_color(rgb(theme.muted))
                    .child(format!("› {}", turn.text))
                    .into_any_element(),
                Role::Assistant => {
                    TextView::markdown(("turn", index), turn.text.clone()).into_any_element()
                }
            });
        let pending = session.pending.as_ref().map(|pending| {
            if pending.text.is_empty() {
                div()
                    .text_sm()
                    .text_color(rgb(theme.muted))
                    .child("Thinking…")
                    .into_any_element()
            } else {
                TextView::markdown("pending", pending.text.clone()).into_any_element()
            }
        });

        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .gap_3()
                    .items_center()
                    .child(
                        img(session.thumbnail.clone())
                            .w(px(96.))
                            .h(px(54.))
                            .object_fit(ObjectFit::Cover)
                            .rounded_md(),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .child(div().text_sm().truncate().child(title))
                            .child(div().text_xs().text_color(rgb(theme.muted)).child(format!(
                                "{}×{} · {} KB",
                                capture.frame.width,
                                capture.frame.height,
                                capture.frame.jpeg.len().div_ceil(1024)
                            )))
                            .child(
                                div()
                                    .id("game-notes")
                                    .text_xs()
                                    .text_color(rgb(theme.accent))
                                    .cursor_pointer()
                                    .hover(|style| style.underline())
                                    .child(notes_label)
                                    .on_click(move |_, _, cx| {
                                        game_editor::edit_window(&app, &window_title, cx)
                                    }),
                            ),
                    ),
            )
            .child(
                div()
                    .id("conversation")
                    .flex()
                    .flex_col()
                    .gap_2()
                    .max_h(px(330.))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(turns)
                    .children(pending),
            )
            .children(session.note.as_ref().map(|note| {
                match note {
                    Note::Info(text) => div()
                        .text_xs()
                        .text_color(rgb(theme.muted))
                        .child(text.clone()),
                    Note::Error(text) => div()
                        .text_xs()
                        .text_color(rgb(theme.error))
                        .child(text.clone()),
                }
            }))
            .child(Input::new(&self.input))
    }
}

impl Render for Overlay {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let config = &cx.global::<AppState>().config;
        let theme = themes::find(&config.overlay.theme);
        let opacity = config.overlay.opacity();
        let (ask, ask_deep) = (
            hotkeys::pretty(&config.hotkeys.ask),
            hotkeys::pretty(&config.hotkeys.ask_deep),
        );
        let tier = match &self.status {
            Status::Capturing(tier) => Some(*tier),
            Status::Session(session) => Some(session.tier),
            Status::Idle | Status::Failed(_) => None,
        };
        let header_hint = match tier {
            Some(Tier::Fast) => "fast · esc to hide",
            Some(Tier::Deep) => "deep · esc to hide",
            None => "",
        };
        let body = match &self.status {
            Status::Idle => div()
                .text_sm()
                .child(format!(
                    "Press {ask} to ask about what's on screen, or {ask_deep} for a deeper answer."
                ))
                .into_any_element(),
            Status::Capturing(_) => div()
                .text_sm()
                .text_color(rgb(theme.muted))
                .child("Capturing…")
                .into_any_element(),
            Status::Failed(message) => div()
                .text_sm()
                .text_color(rgb(theme.error))
                .child(message.clone())
                .into_any_element(),
            Status::Session(session) => self.render_session(session, theme, cx).into_any_element(),
        };

        // The window is transparent; only this rounded panel is drawn.
        div()
            .size_full()
            .p_2()
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.dismiss(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .p_4()
                    .rounded_xl()
                    .bg(theme.background(opacity))
                    .border_1()
                    .border_color(theme.border())
                    .shadow_lg()
                    .text_color(rgb(theme.text))
                    .child(
                        // The header is the drag handle.
                        div()
                            .id("drag-handle")
                            .flex()
                            .justify_between()
                            .items_center()
                            .cursor_grab()
                            .on_mouse_down(MouseButton::Left, |_, window, _| {
                                platform::start_drag(window)
                            })
                            .child(div().font_weight(FontWeight::SEMIBOLD).child("Cue"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(theme.muted))
                                    .child(header_hint),
                            ),
                    )
                    .child(body),
            )
    }
}

/// Runs one streamed request on a background thread, forwarding text as it arrives.
fn request_answer(
    account: &ProviderAccount,
    model: &str,
    tier: Tier,
    conversation: &Conversation,
    cancel: &AtomicBool,
    deltas: &mpsc::UnboundedSender<String>,
) -> Result<Completion, String> {
    let name = account.display_name();
    let describe = |err: ProviderError| {
        let rejected_key = matches!(
            err,
            ProviderError::MissingKey
                | ProviderError::Http {
                    status: 401 | 403,
                    ..
                }
        );
        let hint = if rejected_key {
            " Check this key, or pick a model from another key in Cue's settings."
        } else {
            ""
        };
        format!("{name}: {err}{hint}")
    };
    let key = secrets::api_key(&account.id).map_err(|err| format!("{name}: {err}"))?;
    let provider = providers::connect(account, key).map_err(describe)?;
    provider
        .stream(model, tier, conversation, &mut |piece| {
            let stopped =
                cancel.load(Ordering::Relaxed) || deltas.unbounded_send(piece.to_owned()).is_err();
            if stopped {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        })
        .map_err(describe)
}

/// A handle to the overlay window and its view.
#[derive(Clone)]
pub struct OverlayHandle {
    window: AnyWindowHandle,
    view: Entity<Overlay>,
}

impl OverlayHandle {
    pub fn update<C: AppContext, R>(
        &self,
        cx: &mut C,
        update: impl FnOnce(&mut Overlay, &mut Window, &mut Context<Overlay>) -> R,
    ) -> Result<R> {
        let view = self.view.clone();
        self.window.update(cx, |_, window, cx| {
            view.update(cx, |overlay, cx| update(overlay, window, cx))
        })
    }

    pub fn read<'a>(&self, cx: &'a App) -> &'a Overlay {
        self.view.read(cx)
    }
}

/// Opens the overlay where the player last left it, or in the top-right corner of the
/// primary display, without taking focus away from the game. While idle it lets clicks
/// through to the game.
pub fn open(cx: &mut App) -> Result<OverlayHandle> {
    let saved = cx
        .global::<AppState>()
        .config
        .overlay
        .position
        .map(|position| point(px(position.x), px(position.y)))
        // A display may have been unplugged since.
        .filter(|origin| {
            cx.displays()
                .iter()
                .any(|display| display.bounds().contains(origin))
        });
    let origin = saved.unwrap_or_else(|| {
        let screen = cx
            .primary_display()
            .map(|display| display.visible_bounds())
            .unwrap_or_else(|| Bounds::new(Point::default(), size(px(1280.), px(800.))));
        point(screen.right() - SIZE.width - MARGIN, screen.top() + MARGIN)
    });

    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(origin, SIZE))),
        titlebar: None,
        focus: false,
        kind: WindowKind::PopUp,
        is_resizable: false,
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    };
    let (window, view) = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| Overlay::new(window, cx))
    })?;
    window.update(cx, |_, window, cx| {
        if let Err(err) = platform::make_frameless(window) {
            eprintln!("cue: couldn't remove the overlay frame: {err:#}");
        }
        // gpui-kit's Root paints the theme background; the overlay draws its own panel.
        Root::update(window, cx, |root, _, _| {
            root.style().background = Some(transparent_black().into());
        });
        platform::set_click_through(window, true)
    })??;
    Ok(OverlayHandle { window, view })
}
