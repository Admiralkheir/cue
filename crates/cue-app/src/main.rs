// Release builds on Windows are GUI apps; debug builds keep a console for the logs.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app_state;
mod game_editor;
mod hotkeys;
mod logging;
mod model_picker;
mod overlay;
mod platform;
mod settings;
mod single_instance;
mod themes;
mod updates;

use cue_core::Tier;
use futures::StreamExt;
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;

use crate::app_state::AppState;
use crate::overlay::OverlayHandle;
use crate::settings::Settings;

fn main() {
    logging::init();
    let relaunched = std::env::args().any(|arg| arg == cue_core::update::RELAUNCHED_ARG);
    if relaunched {
        tracing::info!(
            version = cue_core::update::CURRENT_VERSION,
            "restarted after an update"
        );
    }
    let Some(mut show_requests) = single_instance::claim(relaunched) else {
        tracing::info!("Cue is already running; asked it to show its settings");
        return;
    };
    gpui_kit::application().run(|cx| {
        gpui_kit::init(cx);
        // Games are mostly dark, and the overlay is designed for a dark theme.
        Theme::change(ThemeMode::Dark, None, cx);

        // Every window reads the shared state while rendering, so it goes in first.
        cx.set_global(AppState::load());
        let overlay = overlay::open(cx).expect("failed to open the overlay");

        let hotkey_config = cx.global::<AppState>().config.hotkeys.clone();
        let started = hotkeys::start().and_then(|(mut service, presses)| {
            let bound = service.bind(&hotkey_config);
            match &bound {
                Ok(()) => tracing::info!(
                    fast = %hotkey_config.ask,
                    deep = %hotkey_config.ask_deep,
                    "hotkeys registered"
                ),
                Err(err) => tracing::warn!("hotkeys not registered: {err:#}"),
            }
            // Kept even if binding failed, so new shortcuts can be set in settings.
            cx.set_global(service);
            bound.map(|()| presses)
        });
        match started {
            Ok(mut presses) => {
                let overlay = overlay.clone();
                cx.spawn(async move |cx| {
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

        let mut settings =
            open_settings(overlay.clone(), cx).expect("failed to open the settings window");
        cx.activate(true);
        // The settings window, open on every start, shows what the check finds.
        updates::on_startup(cx);

        // Launching Cue again brings the settings back, reopening them if they were closed.
        cx.spawn(async move |cx| {
            while show_requests.next().await.is_some() {
                let shown =
                    settings.update(cx, |_, window, cx| platform::show_and_focus(window, cx));
                match shown {
                    Ok(Ok(())) => {}
                    Ok(Err(err)) => tracing::warn!("couldn't show the settings: {err:#}"),
                    Err(_) => match cx.update(|cx| open_settings(overlay.clone(), cx)) {
                        Ok(reopened) => settings = reopened,
                        Err(err) => tracing::warn!("couldn't reopen the settings: {err:#}"),
                    },
                }
            }
        })
        .detach();
    });
}

fn open_settings(overlay: OverlayHandle, cx: &mut App) -> anyhow::Result<AnyWindowHandle> {
    let bounds = Bounds::centered(None, size(px(600.), px(820.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        // Shown in the taskbar and Alt+Tab on Windows, and in the Window menu on macOS.
        titlebar: Some(TitlebarOptions {
            title: Some("Cue".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let (window, _) = gpui_kit::open_window(options, cx, move |window, cx| {
        cx.new(|cx| Settings::new(overlay, window, cx))
    })?;
    Ok(window)
}
