mod app_state;
mod hotkeys;
mod overlay;
mod platform;
mod settings;

use cue_core::Tier;
use futures::StreamExt;
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;

use crate::app_state::AppState;
use crate::settings::Settings;

fn main() {
    gpui_kit::application().run(|cx| {
        gpui_kit::init(cx);
        // Games are mostly dark, and the overlay is designed for a dark theme.
        Theme::change(ThemeMode::Dark, None, cx);

        // Every window reads the shared state while rendering, so it goes in first.
        cx.set_global(AppState::load());
        let overlay = overlay::open(cx).expect("failed to open the overlay");

        let hotkey_config = cx.global::<AppState>().config.hotkeys.clone();
        match hotkeys::register(&hotkey_config) {
            Ok((manager, mut presses)) => {
                let overlay = overlay.clone();
                cx.spawn(async move |cx| {
                    // Dropping the manager would unregister the hotkeys.
                    let _manager = manager;
                    while let Some(tier) = presses.next().await {
                        overlay
                            .update(cx, |overlay, window, cx| overlay.ask(tier, window, cx))
                            .ok();
                    }
                })
                .detach();
            }
            Err(err) => {
                let message = format!("Hotkeys are disabled: {err:#}");
                let state = cx.global_mut::<AppState>();
                state.load_error = Some(match state.load_error.take() {
                    Some(load) => format!("{load}\n{message}"),
                    None => message,
                });
            }
        }

        // Development aid: `CUE_ASK_AFTER=3` acts as if the fast hotkey was pressed after
        // three seconds, for testing without a keyboard shortcut.
        if let Some(delay) = std::env::var("CUE_ASK_AFTER")
            .ok()
            .and_then(|s| s.parse().ok())
        {
            let overlay = overlay.clone();
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(delay))
                    .await;
                overlay
                    .update(cx, |overlay, window, cx| {
                        overlay.ask(Tier::Fast, window, cx)
                    })
                    .ok();
            })
            .detach();
        }

        let bounds = Bounds::centered(None, size(px(560.), px(640.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, move |window, cx| {
            cx.new(|cx| Settings::new(overlay, window, cx))
        })
        .expect("failed to open the settings window");
        cx.activate(true);
    });
}
