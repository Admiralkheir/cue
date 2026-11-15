//! The notification-area icon on Windows and the menu bar icon on macOS.
//!
//! Closing the settings window leaves Cue running for the hotkeys, so the icon is how the
//! player gets the settings back, or quits Cue for good.

use futures::channel::mpsc;

/// What the player asked for from the icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    ShowSettings,
    Quit,
}

/// Keeps the icon on screen while it lives.
pub struct Tray {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    _icon: tray_icon::TrayIcon,
}

/// Adds the icon and returns a stream of what the player picks from it. Must be called on
/// the main thread once the app is running.
///
/// Returns `None` where there's no icon: on Linux, or if the platform refused it.
pub fn start() -> Option<(Tray, mpsc::UnboundedReceiver<Request>)> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    match imp::start() {
        Ok(started) => return Some(started),
        Err(err) => tracing::warn!("couldn't add the tray icon: {err:#}"),
    }
    None
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod imp {
    use anyhow::{Context, Result};
    use futures::channel::mpsc;
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    use super::{Request, Tray};

    const SHOW_ID: &str = "show-settings";
    const QUIT_ID: &str = "quit";

    #[cfg(target_os = "windows")]
    const QUIT_LABEL: &str = "Exit";
    #[cfg(target_os = "macos")]
    const QUIT_LABEL: &str = "Quit Cue";

    pub fn start() -> Result<(Tray, mpsc::UnboundedReceiver<Request>)> {
        let (requests, receiver) = mpsc::unbounded();

        let menu = Menu::with_items(&[
            &MenuItem::with_id(SHOW_ID, "Open Cue", true, None),
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id(QUIT_ID, QUIT_LABEL, true, None),
        ])
        .context("couldn't build the tray menu")?;

        // Sends fail only once the app is shutting down and the receiver is gone.
        let menu_requests = requests.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let request = if event.id == SHOW_ID {
                Request::ShowSettings
            } else if event.id == QUIT_ID {
                Request::Quit
            } else {
                return;
            };
            let _ = menu_requests.unbounded_send(request);
        }));
        // On Windows a left click opens the settings, and the menu is on the right button.
        // On macOS both buttons open the menu, as menu bar icons do there.
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let _ = requests.unbounded_send(Request::ShowSettings);
            }
        }));

        let icon = TrayIconBuilder::new()
            .with_icon(icon()?)
            .with_tooltip("Cue")
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(cfg!(target_os = "macos"))
            .build()
            .context("couldn't create the tray icon")?;
        Ok((Tray { _icon: icon }, receiver))
    }

    /// The app icon that build.rs embeds as resource 1, at the size the taskbar wants.
    #[cfg(target_os = "windows")]
    fn icon() -> Result<Icon> {
        use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSMICON, SM_CYSMICON};
        // SAFETY: no preconditions.
        let size = unsafe { (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON)) };
        let size = u32::try_from(size.0)
            .ok()
            .zip(u32::try_from(size.1).ok())
            .filter(|&(width, height)| width > 0 && height > 0);
        Icon::from_resource(1, size).context("couldn't load the app icon")
    }

    #[cfg(target_os = "macos")]
    fn icon() -> Result<Icon> {
        let png = include_bytes!("../../../assets/cue.png");
        let image = image::load_from_memory_with_format(png, image::ImageFormat::Png)
            .context("couldn't decode the app icon")?
            .into_rgba8();
        let (width, height) = image.dimensions();
        Icon::from_rgba(image.into_raw(), width, height).context("couldn't load the app icon")
    }
}
