//! System-wide shortcuts that fire while the game has focus.

use anyhow::{Context, Result};
use cue_core::{Hotkeys, Tier};
use futures::channel::mpsc;
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

/// Registers the ask shortcuts and returns a stream with one tier per key press.
///
/// The hotkeys stay registered for as long as the returned manager is alive. Must be
/// called on the main thread.
pub fn register(config: &Hotkeys) -> Result<(GlobalHotKeyManager, mpsc::UnboundedReceiver<Tier>)> {
    let ask = parse(&config.ask)?;
    let ask_deep = parse(&config.ask_deep)?;

    let manager = GlobalHotKeyManager::new().context("couldn't start the hotkey listener")?;
    for (hotkey, text) in [(ask, &config.ask), (ask_deep, &config.ask_deep)] {
        manager
            .register(hotkey)
            .with_context(|| format!("couldn't register {text}; another app may be using it"))?;
    }

    let (presses, receiver) = mpsc::unbounded();
    let (ask_id, ask_deep_id) = (ask.id(), ask_deep.id());
    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        if event.state() != HotKeyState::Pressed {
            return;
        }
        let tier = match event.id() {
            id if id == ask_id => Tier::Fast,
            id if id == ask_deep_id => Tier::Deep,
            _ => return,
        };
        // Fails only once the app is shutting down and the receiver is gone.
        let _ = presses.unbounded_send(tier);
    }));

    Ok((manager, receiver))
}

fn parse(text: &str) -> Result<HotKey> {
    text.parse()
        .with_context(|| format!("invalid hotkey {text:?}"))
}
