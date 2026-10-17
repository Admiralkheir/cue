//! Native window tweaks that GPUI doesn't expose.

use anyhow::{Result, bail};
use gpui_kit::Window;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// Lets mouse events fall through the window to whatever is underneath it, i.e. the game.
pub fn set_click_through(window: &Window, enabled: bool) -> Result<()> {
    // Fully qualified: gpui's own `Window::window_handle` returns an `AnyWindowHandle`.
    let handle = HasWindowHandle::window_handle(window)?;
    match handle.as_raw() {
        #[cfg(target_os = "macos")]
        RawWindowHandle::AppKit(handle) => macos::set_click_through(handle, enabled),
        #[cfg(target_os = "windows")]
        RawWindowHandle::Win32(handle) => win32::set_click_through(handle, enabled),
        other => bail!("click-through isn't supported for {other:?}"),
    }
}

/// Strips the system frame so only what the overlay draws is visible.
pub fn make_frameless(window: &Window) -> Result<()> {
    let handle = HasWindowHandle::window_handle(window)?;
    match handle.as_raw() {
        #[cfg(target_os = "macos")]
        RawWindowHandle::AppKit(handle) => macos::make_frameless(handle),
        // GPUI already creates pop-up windows without a frame on Windows.
        #[cfg(target_os = "windows")]
        RawWindowHandle::Win32(_) => Ok(()),
        other => bail!("frameless windows aren't supported for {other:?}"),
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use anyhow::{Context, Result};
    use objc2::rc::Retained;
    use objc2_app_kit::{NSView, NSWindow, NSWindowStyleMask};
    use raw_window_handle::AppKitWindowHandle;

    fn ns_window(handle: AppKitWindowHandle) -> Result<Retained<NSWindow>> {
        // SAFETY: GPUI hands out the live content view of an open window, and GPUI runs
        // window callbacks on the main thread, which AppKit requires.
        let view: &NSView = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
        view.window().context("the view isn't attached to a window")
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
        Ok(())
    }
}

#[cfg(target_os = "windows")]
mod win32 {
    use anyhow::Result;
    use raw_window_handle::Win32WindowHandle;
    use windows::Win32::Foundation::{COLORREF, HWND};
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongPtrW, LWA_ALPHA, SetLayeredWindowAttributes, SetWindowLongPtrW,
        WS_EX_LAYERED, WS_EX_TRANSPARENT,
    };

    // Top-level windows only pass clicks through when they are both layered and transparent.
    pub fn set_click_through(handle: Win32WindowHandle, enabled: bool) -> Result<()> {
        let hwnd = HWND(handle.hwnd.get() as *mut _);
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
}
