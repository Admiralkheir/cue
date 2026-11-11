//! Keeps Cue to one running copy.
//!
//! A second copy couldn't register the hotkeys, which the first one holds. On Windows,
//! launching Cue again from the Start menu would start one, so a later launch asks the
//! running copy to show its settings and exits. macOS does this itself for app bundles.

use futures::channel::mpsc;

/// Claims the running copy for this process.
///
/// Returns `None` when Cue is already running: that copy has been asked to show its
/// settings, and this one should exit. Otherwise returns a stream that yields whenever a
/// later launch asks for the settings.
///
/// With `wait_for_previous`, set when an update restarts Cue, a running copy is the old
/// version on its way out: this waits a while for it to exit instead.
pub fn claim(wait_for_previous: bool) -> Option<mpsc::UnboundedReceiver<()>> {
    #[cfg(target_os = "windows")]
    return win32::claim(wait_for_previous);
    #[cfg(not(target_os = "windows"))]
    {
        let _ = wait_for_previous;
        // Nothing sends, so the stream simply ends.
        let (_, requests) = mpsc::unbounded();
        Some(requests)
    }
}

#[cfg(target_os = "windows")]
mod win32 {
    use futures::channel::mpsc;
    use std::time::{Duration, Instant};

    use windows::Win32::Foundation::{
        CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_OBJECT_0,
    };
    use windows::Win32::System::Threading::{
        CreateEventW, INFINITE, SetEvent, WaitForSingleObject,
    };
    use windows::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};
    use windows::core::w;

    /// How long a restarted copy waits for the old one to exit.
    const RESTART_WAIT: Duration = Duration::from_secs(15);

    pub fn claim(wait_for_previous: bool) -> Option<mpsc::UnboundedReceiver<()>> {
        let deadline = Instant::now() + RESTART_WAIT;
        loop {
            match try_claim() {
                Claim::Taken(event) if wait_for_previous && Instant::now() < deadline => {
                    // SAFETY: `event` is the handle just opened; it's not used again.
                    unsafe {
                        let _ = CloseHandle(event);
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                Claim::Taken(event) => {
                    // SAFETY: `event` is the handle just opened. Only the foreground app may
                    // hand the foreground on, and a freshly launched one is it: without the
                    // grant, Windows would only flash the running copy's taskbar button.
                    unsafe {
                        let _ = AllowSetForegroundWindow(ASFW_ANY);
                        let _ = SetEvent(event);
                    }
                    return None;
                }
                Claim::Ours(requests) => return Some(requests),
            }
        }
    }

    enum Claim {
        /// Another copy holds the event, opened here as well.
        Taken(HANDLE),
        Ours(mpsc::UnboundedReceiver<()>),
    }

    fn try_claim() -> Claim {
        // The first copy creates the event and keeps it open for as long as it runs; later
        // launches open the same one and set it. "Local\" keeps it to this sign-in session,
        // so other users on the machine run their own copy.
        // SAFETY: plain call with a static name; the handle is checked below.
        let event = match unsafe {
            CreateEventW(
                None,
                false,
                false,
                w!("Local\\Admiralkheir.Cue.ShowSettings"),
            )
        } {
            Ok(event) => event,
            Err(err) => {
                tracing::warn!("couldn't check for a running copy of Cue: {err}");
                let (_, requests) = mpsc::unbounded();
                return Claim::Ours(requests);
            }
        };
        // SAFETY: read right after the call that set it.
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            return Claim::Taken(event);
        }

        let (requests_tx, requests) = mpsc::unbounded();
        // A HANDLE holds a raw pointer, which can't cross threads as is.
        let event = event.0 as usize;
        let spawned = std::thread::Builder::new()
            .name("cue-single-instance".into())
            .spawn(move || {
                let event = HANDLE(event as *mut _);
                // SAFETY: the handle is never closed while the process runs.
                while unsafe { WaitForSingleObject(event, INFINITE) } == WAIT_OBJECT_0 {
                    if requests_tx.unbounded_send(()).is_err() {
                        break;
                    }
                }
            });
        if let Err(err) = spawned {
            tracing::warn!("couldn't wait for later launches of Cue: {err}");
        }
        Claim::Ours(requests)
    }
}
