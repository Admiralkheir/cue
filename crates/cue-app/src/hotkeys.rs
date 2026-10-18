//! System-wide shortcuts that fire while the game has focus.

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use cue_core::{Hotkeys, Tier};
use futures::channel::mpsc;
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use gpui_kit::Global;

/// Owns the registered shortcuts; they stay registered while this lives.
pub struct HotkeyService {
    manager: GlobalHotKeyManager,
    active: Vec<HotKey>,
    /// Which tier each registered shortcut asks with, read by the event handler.
    tiers: Arc<Mutex<Vec<(u32, Tier)>>>,
}

impl Global for HotkeyService {}

/// Starts listening and returns a stream with one tier per key press. Nothing is bound
/// until [`HotkeyService::bind`]. Must be called on the main thread.
pub fn start() -> Result<(HotkeyService, mpsc::UnboundedReceiver<Tier>)> {
    let manager = GlobalHotKeyManager::new().context("couldn't start the hotkey listener")?;
    let tiers: Arc<Mutex<Vec<(u32, Tier)>>> = Arc::default();
    let (presses, receiver) = mpsc::unbounded();

    let handler_tiers = tiers.clone();
    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        if event.state() != HotKeyState::Pressed {
            return;
        }
        let tiers = handler_tiers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(&(_, tier)) = tiers.iter().find(|(id, _)| *id == event.id()) {
            // Fails only once the app is shutting down and the receiver is gone.
            let _ = presses.unbounded_send(tier);
        }
    }));

    let service = HotkeyService {
        manager,
        active: Vec::new(),
        tiers,
    };
    Ok((service, receiver))
}

impl HotkeyService {
    /// Swaps in the shortcuts from `config`. If they can't all be registered, the
    /// previous ones stay in place.
    pub fn bind(&mut self, config: &Hotkeys) -> Result<()> {
        let ask = parse(&config.ask)?;
        let ask_deep = parse(&config.ask_deep)?;
        if ask == ask_deep {
            bail!("the fast and deep shortcuts must be different");
        }

        let _ = self.manager.unregister_all(&self.active);
        for (hotkey, text) in [(ask, &config.ask), (ask_deep, &config.ask_deep)] {
            if let Err(err) = self.manager.register(hotkey) {
                let _ = self.manager.unregister_all(&[ask, ask_deep]);
                let _ = self.manager.register_all(&self.active);
                return Err(err).with_context(|| {
                    format!(
                        "couldn't register {}; another app may be using it",
                        pretty(text)
                    )
                });
            }
        }

        self.active = vec![ask, ask_deep];
        *self
            .tiers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            vec![(ask.id(), Tier::Fast), (ask_deep.id(), Tier::Deep)];
        Ok(())
    }
}

fn parse(text: &str) -> Result<HotKey> {
    text.parse()
        .with_context(|| format!("{text:?} isn't a valid shortcut"))
}

/// A shortcut as people read it on this platform: `⌘⇧Space` on macOS,
/// `Ctrl+Shift+Space` elsewhere.
pub fn pretty(text: &str) -> String {
    let parts = text.split('+').map(str::trim).map(|part| {
        let lower = part.to_ascii_lowercase();
        let symbol = match lower.as_str() {
            "cmdorctrl" | "commandorcontrol" | "cmdorcontrol" | "commandorctrl" => {
                if cfg!(target_os = "macos") {
                    "⌘"
                } else {
                    "Ctrl"
                }
            }
            "cmd" | "command" | "super" => {
                if cfg!(target_os = "macos") {
                    "⌘"
                } else {
                    "Win"
                }
            }
            "ctrl" | "control" => {
                if cfg!(target_os = "macos") {
                    "⌃"
                } else {
                    "Ctrl"
                }
            }
            "alt" | "option" => {
                if cfg!(target_os = "macos") {
                    "⌥"
                } else {
                    "Alt"
                }
            }
            "shift" => {
                if cfg!(target_os = "macos") {
                    "⇧"
                } else {
                    "Shift"
                }
            }
            "enter" | "return" => {
                if cfg!(target_os = "macos") {
                    "↩"
                } else {
                    "Enter"
                }
            }
            _ => part,
        };
        symbol.to_owned()
    });
    let separator = if cfg!(target_os = "macos") { "" } else { "+" };
    parts.collect::<Vec<_>>().join(separator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretty_prints_for_the_platform() {
        let shown = pretty("CmdOrCtrl+Shift+Space");
        if cfg!(target_os = "macos") {
            assert_eq!(shown, "⌘⇧Space");
            assert_eq!(pretty("CmdOrCtrl+Shift+Enter"), "⌘⇧↩");
        } else {
            assert_eq!(shown, "Ctrl+Shift+Space");
        }
    }
}
