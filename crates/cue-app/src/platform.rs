//! Native window tweaks that GPUI doesn't expose.
//!
//! On Windows the changes are applied once the current GPUI event or update is over (see
//! [`after_update`]), so errors there are logged rather than returned. Click-through is the
//! exception: its caller gets the outcome back, because the overlay keeps track of it.

use anyhow::{Result, bail};
use gpui_kit::{App, Task, Window};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

fn raw_handle(window: &Window) -> Result<RawWindowHandle> {
    // Fully qualified: gpui's own `Window::window_handle` returns an `AnyWindowHandle`.
    Ok(HasWindowHandle::window_handle(window)?.as_raw())
}

/// Runs a Win32 call once the GPUI event or update we're in has finished.
///
/// Calls like `ShowWindow` deliver messages to the window before they return. GPUI can't
/// handle those while the app is borrowed, as it is in every event handler and update: it
/// logs "RefCell already borrowed" and drops them, so its idea of whether the window is
/// visible, and where, goes stale.
#[cfg(target_os = "windows")]
fn after_update(cx: &App, call: impl FnOnce() -> Result<()> + 'static) {
    cx.foreground_executor()
        .spawn(async move {
            if let Err(err) = call() {
                tracing::warn!("window change failed: {err:#}");
            }
        })
        .detach();
}

/// Lets mouse events fall through the window to whatever is underneath it, i.e. the game.
///
/// An error means the platform can't do it at all. Otherwise the returned task finishes
/// once the change has been made, with its outcome: right away on macOS, after the current
/// update on Windows.
// Only the macOS and Windows arms read `enabled`, and only Windows needs `cx`.
#[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
pub fn set_click_through(window: &Window, enabled: bool, cx: &App) -> Result<Task<Result<()>>> {
    match raw_handle(window)? {
        #[cfg(target_os = "macos")]
        RawWindowHandle::AppKit(handle) => {
            Ok(Task::ready(macos::set_click_through(handle, enabled)))
        }
        #[cfg(target_os = "windows")]
        RawWindowHandle::Win32(handle) => Ok(cx
            .foreground_executor()
            .spawn(async move { win32::set_click_through(handle, enabled) })),
        other => bail!("click-through isn't supported for {other:?}"),
    }
}

/// Strips the system frame so only what the overlay draws is visible.
#[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
pub fn make_frameless(window: &Window, cx: &App) -> Result<()> {
    match raw_handle(window)? {
        #[cfg(target_os = "macos")]
        RawWindowHandle::AppKit(handle) => macos::make_frameless(handle),
        #[cfg(target_os = "windows")]
        RawWindowHandle::Win32(handle) => {
            after_update(cx, move || win32::make_frameless(handle));
            Ok(())
        }
        other => bail!("frameless windows aren't supported for {other:?}"),
    }
}

/// Shows the window and gives it the keyboard so the player can type.
///
/// Neither macOS nor Windows lets a background app take the keyboard, so Cue becomes
/// the active app and the game loses focus until the overlay is hidden again.
#[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
pub fn show_and_focus(window: &Window, cx: &App) -> Result<()> {
    match raw_handle(window)? {
        #[cfg(target_os = "macos")]
        RawWindowHandle::AppKit(handle) => macos::show_and_focus(handle),
        #[cfg(target_os = "windows")]
        RawWindowHandle::Win32(handle) => {
            after_update(cx, move || win32::show_and_focus(handle));
            Ok(())
        }
        other => bail!("focusing isn't supported for {other:?}"),
    }
}

/// The process id of the frontmost app, where the platform can tell reliably.
pub fn frontmost_app_pid() -> Option<u32> {
    #[cfg(target_os = "macos")]
    return macos::frontmost_app_pid();
    #[cfg(not(target_os = "macos"))]
    None
}

/// Starts moving the window with the mouse; call it from a left mouse-down handler.
pub fn start_drag(window: &Window) {
    #[cfg(target_os = "windows")]
    if let Ok(RawWindowHandle::Win32(handle)) = raw_handle(window) {
        win32::start_drag(handle);
        return;
    }
    // GPUI implements this on macOS and Linux, but not on Windows.
    window.start_window_move();
}

/// Whether key presses currently go to this window.
pub fn has_keyboard(window: &Window) -> bool {
    match raw_handle(window) {
        #[cfg(target_os = "macos")]
        Ok(RawWindowHandle::AppKit(handle)) => macos::has_keyboard(handle),
        #[cfg(target_os = "windows")]
        Ok(RawWindowHandle::Win32(handle)) => win32::has_keyboard(handle),
        _ => false,
    }
}

/// Shows the window without taking the keyboard from the app in front.
#[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
pub fn show(window: &Window, cx: &App) -> Result<()> {
    match raw_handle(window)? {
        #[cfg(target_os = "macos")]
        RawWindowHandle::AppKit(handle) => macos::show(handle),
        #[cfg(target_os = "windows")]
        RawWindowHandle::Win32(handle) => {
            after_update(cx, move || win32::show(handle));
            Ok(())
        }
        other => bail!("showing isn't supported for {other:?}"),
    }
}

/// Hides the window and hands the keyboard back to the app that had it (the game).
#[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
pub fn hide(window: &Window, cx: &App) -> Result<()> {
    match raw_handle(window)? {
        #[cfg(target_os = "macos")]
        RawWindowHandle::AppKit(handle) => macos::hide(handle),
        #[cfg(target_os = "windows")]
        RawWindowHandle::Win32(handle) => {
            after_update(cx, move || win32::hide(handle));
            Ok(())
        }
        other => bail!("hiding isn't supported for {other:?}"),
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::RefCell;

    use anyhow::{Context, Result};
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationOptions, NSRunningApplication, NSView, NSWindow,
        NSWindowStyleMask, NSWorkspace,
    };
    use raw_window_handle::AppKitWindowHandle;

    thread_local! {
        /// The app that was in front, normally the game, when the overlay took the keyboard.
        static PREVIOUS_APP: RefCell<Option<Retained<NSRunningApplication>>> =
            const { RefCell::new(None) };
    }

    fn ns_view(handle: &AppKitWindowHandle) -> &NSView {
        // SAFETY: GPUI hands out the live content view of an open window, and GPUI runs
        // window callbacks on the main thread, which AppKit requires.
        unsafe { handle.ns_view.cast::<NSView>().as_ref() }
    }

    fn ns_window(handle: AppKitWindowHandle) -> Result<Retained<NSWindow>> {
        ns_view(&handle)
            .window()
            .context("the view isn't attached to a window")
    }

    pub fn set_click_through(handle: AppKitWindowHandle, enabled: bool) -> Result<()> {
        ns_window(handle)?.setIgnoresMouseEvents(enabled);
        Ok(())
    }

    // GPUI opens title-less windows as titled with a full-size content view, so AppKit
    // still draws a rounded frame and shadow. GPUI's panel class can still become key
    // without a title bar, so typing into the overlay keeps working.
    pub fn make_frameless(handle: AppKitWindowHandle) -> Result<()> {
        let window = ns_window(handle)?;
        window.setStyleMask(NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel);
        window.setHasShadow(false);
        // Swapping the style mask rebuilds the frame view and leaves the window itself as
        // first responder, so key presses would beep instead of reaching GPUI.
        window.makeFirstResponder(Some(ns_view(&handle)));
        Ok(())
    }

    pub fn show_and_focus(handle: AppKitWindowHandle) -> Result<()> {
        let mtm = MainThreadMarker::new().context("AppKit calls must run on the main thread")?;
        let window = ns_window(handle)?;
        let view = ns_view(&handle);

        let own_pid = NSRunningApplication::currentApplication().processIdentifier();
        let frontmost = NSWorkspace::sharedWorkspace().frontmostApplication();
        if let Some(app) = frontmost.filter(|app| app.processIdentifier() != own_pid) {
            PREVIOUS_APP.with_borrow_mut(|previous| *previous = Some(app));
        }

        // A panel of a background app can't take the keyboard, even a non-activating one,
        // so Cue has to become the active app. Activation raises only the key and main
        // windows, so making the overlay both keeps the settings window where it is.
        window.makeKeyAndOrderFront(None);
        window.makeMainWindow();
        window.makeFirstResponder(Some(view));
        #[allow(deprecated)] // Its replacement, `activate`, needs macOS 14.
        NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
        Ok(())
    }

    pub fn has_keyboard(handle: AppKitWindowHandle) -> bool {
        ns_window(handle).is_ok_and(|window| window.isKeyWindow())
    }

    pub fn frontmost_app_pid() -> Option<u32> {
        let app = NSWorkspace::sharedWorkspace().frontmostApplication()?;
        u32::try_from(app.processIdentifier()).ok()
    }

    pub fn show(handle: AppKitWindowHandle) -> Result<()> {
        ns_window(handle)?.orderFrontRegardless();
        Ok(())
    }

    pub fn hide(handle: AppKitWindowHandle) -> Result<()> {
        ns_window(handle)?.orderOut(None);
        if let Some(app) = PREVIOUS_APP.with_borrow_mut(Option::take) {
            app.activateWithOptions(NSApplicationActivationOptions::empty());
        }
        Ok(())
    }
}

#[cfg(target_os = "windows")]
mod win32 {
    use anyhow::Result;
    use raw_window_handle::Win32WindowHandle;
    use windows::Win32::Foundation::{COLORREF, HWND};
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetForegroundWindow, GetWindowLongPtrW, IsIconic, LWA_ALPHA, SW_HIDE,
        SW_RESTORE, SW_SHOW, SW_SHOWNOACTIVATE, SetForegroundWindow, SetLayeredWindowAttributes,
        SetWindowLongPtrW, ShowWindow, WS_EX_LAYERED, WS_EX_TRANSPARENT,
    };

    fn hwnd(handle: Win32WindowHandle) -> HWND {
        HWND(handle.hwnd.get() as *mut _)
    }

    // Top-level windows only pass clicks through when they are both layered and transparent.
    pub fn set_click_through(handle: Win32WindowHandle, enabled: bool) -> Result<()> {
        let hwnd = hwnd(handle);
        let flags = (WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0) as isize;
        // SAFETY: `hwnd` belongs to a live GPUI window owned by this thread.
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            let style = if enabled {
                style | flags
            } else {
                style & !flags
            };
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style);
            if enabled {
                // A layered window stays invisible until it gets its attributes.
                SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA)?;
            }
        }
        Ok(())
    }

    // GPUI pop-ups have no title bar, but on Windows 11 DWM still rounds the corners and
    // draws a border and shadow around the whole window, well outside the overlay's panel.
    pub fn make_frameless(handle: Win32WindowHandle) -> Result<()> {
        use windows::Win32::Graphics::Dwm::{
            DWMNCRP_DISABLED, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_NCRENDERING_POLICY,
            DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND, DWMWINDOWATTRIBUTE,
            DwmSetWindowAttribute,
        };

        fn set<T>(
            hwnd: HWND,
            attribute: DWMWINDOWATTRIBUTE,
            value: T,
        ) -> windows::core::Result<()> {
            // SAFETY: `value` lives across the call and the size matches its type.
            unsafe {
                DwmSetWindowAttribute(
                    hwnd,
                    attribute,
                    (&raw const value).cast(),
                    size_of::<T>() as u32,
                )
            }
        }

        let hwnd = hwnd(handle);
        // Turns off the shadow, which is drawn as part of the non-client area.
        set(hwnd, DWMWA_NCRENDERING_POLICY, DWMNCRP_DISABLED)?;
        // Windows 10 doesn't round corners or draw this border, and rejects both settings.
        let _ = set(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND);
        let _ = set(hwnd, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE);
        Ok(())
    }

    pub fn show_and_focus(handle: Win32WindowHandle) -> Result<()> {
        let hwnd = hwnd(handle);
        // SAFETY: as above. The return values only report the previous visibility and
        // whether Windows honored the focus request, which it may refuse.
        unsafe {
            // A minimized window, such as the settings window, only comes back when restored.
            let show = if IsIconic(hwnd).as_bool() {
                SW_RESTORE
            } else {
                SW_SHOW
            };
            let _ = ShowWindow(hwnd, show);
            let _ = SetForegroundWindow(hwnd);
        }
        Ok(())
    }

    pub fn start_drag(handle: Win32WindowHandle) {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
        use windows::Win32::UI::WindowsAndMessaging::{
            HTCAPTION, PostMessageW, SC_MOVE, WM_SYSCOMMAND,
        };
        // Ask for a title-bar drag, so Windows runs its own move loop. Posted, not sent:
        // the loop runs GPUI's timers, such as the input's blinking cursor, and those
        // panic if they fire while the mouse-down handler calling this still holds the app.
        // A system command rather than a fake WM_NCLBUTTONDOWN, which GPUI would turn
        // back into a mouse-down for this handler.
        // SAFETY: `hwnd` belongs to a live GPUI window owned by this thread.
        unsafe {
            let _ = ReleaseCapture();
            let _ = PostMessageW(
                Some(hwnd(handle)),
                WM_SYSCOMMAND,
                WPARAM((SC_MOVE | HTCAPTION) as usize),
                LPARAM(0),
            );
        }
    }

    pub fn has_keyboard(handle: Win32WindowHandle) -> bool {
        // SAFETY: no preconditions.
        unsafe { GetForegroundWindow() == hwnd(handle) }
    }

    pub fn show(handle: Win32WindowHandle) -> Result<()> {
        // SAFETY: as above.
        unsafe {
            let _ = ShowWindow(hwnd(handle), SW_SHOWNOACTIVATE);
        }
        Ok(())
    }

    pub fn hide(handle: Win32WindowHandle) -> Result<()> {
        // SAFETY: as above.
        unsafe {
            let _ = ShowWindow(hwnd(handle), SW_HIDE);
        }
        Ok(())
    }
}
