//! Grabs the game window and prepares the frame for a vision model.

use std::fmt;

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ExtendedColorType, ImageEncoder, RgbaImage};
use xcap::Window;

/// Longest edge sent to the model. 1080p keeps text legible at a moderate token cost.
pub const MAX_LONG_EDGE: u32 = 1920;
const JPEG_QUALITY: u8 = 85;
/// Anything smaller is a tooltip, menu or palette rather than the game itself.
const MIN_WIDTH: u32 = 320;
const MIN_HEIGHT: u32 = 240;

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("no game window found; bring the game to the front and try again")]
    NoTarget,
    #[error("screen capture failed: {0}")]
    Capture(#[from] xcap::XCapError),
    #[error("couldn't encode the capture: {0}")]
    Encode(#[from] image::ImageError),
}

/// What we know about an on-screen window when picking the capture target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub pid: u32,
    pub focused: bool,
    pub minimized: bool,
    pub width: u32,
    pub height: u32,
}

/// Picks the topmost sizeable window of the frontmost app, skipping Cue's own windows.
/// `windows` must be ordered front to back.
pub fn choose_target(windows: &[WindowInfo], own_pid: u32) -> Option<usize> {
    windows.iter().position(|window| {
        window.focused
            && !window.minimized
            && window.pid != own_pid
            && window.width >= MIN_WIDTH
            && window.height >= MIN_HEIGHT
    })
}

/// An encoded frame of the game window, ready to send to a model.
#[derive(Clone)]
pub struct Capture {
    pub app_name: String,
    pub title: String,
    pub source_width: u32,
    pub source_height: u32,
    pub frame: EncodedFrame,
}

impl fmt::Debug for Capture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Capture")
            .field("app_name", &self.app_name)
            .field("title", &self.title)
            .field("source", &(self.source_width, self.source_height))
            .field("frame", &self.frame)
            .finish()
    }
}

#[derive(Clone)]
pub struct EncodedFrame {
    pub width: u32,
    pub height: u32,
    pub jpeg: Vec<u8>,
}

impl EncodedFrame {
    pub const MEDIA_TYPE: &str = "image/jpeg";
}

impl fmt::Debug for EncodedFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}x{} JPEG, {} bytes",
            self.width,
            self.height,
            self.jpeg.len()
        )
    }
}

/// Captures the frontmost app's main window. Blocking; run it off the UI thread.
///
/// `frontmost_pid` is the frontmost app as the caller sees it. Pass it when you can:
/// xcap's own focus check can be stale inside a long-running macOS app.
pub fn capture_focused_window(
    own_pid: u32,
    frontmost_pid: Option<u32>,
) -> Result<Capture, CaptureError> {
    let windows = Window::all()?;
    let infos: Vec<WindowInfo> = windows
        .iter()
        .map(|window| window_info(window, frontmost_pid))
        .collect();
    let target = &windows[choose_target(&infos, own_pid).ok_or(CaptureError::NoTarget)?];

    let image = target.capture_image()?;
    let (source_width, source_height) = image.dimensions();
    Ok(Capture {
        app_name: target.app_name().unwrap_or_default(),
        title: target.title().unwrap_or_default(),
        source_width,
        source_height,
        frame: encode_jpeg(image, MAX_LONG_EDGE)?,
    })
}

fn window_info(window: &Window, frontmost_pid: Option<u32>) -> WindowInfo {
    let pid = window.pid().unwrap_or_default();
    WindowInfo {
        pid,
        focused: match frontmost_pid {
            Some(frontmost) => pid == frontmost,
            None => window.is_focused().unwrap_or(false),
        },
        minimized: window.is_minimized().unwrap_or(false),
        width: window.width().unwrap_or_default(),
        height: window.height().unwrap_or_default(),
    }
}

/// Shrinks the frame to fit `max_long_edge`, keeping its aspect ratio, and encodes it as JPEG.
pub fn encode_jpeg(
    image: RgbaImage,
    max_long_edge: u32,
) -> Result<EncodedFrame, image::ImageError> {
    let mut image = DynamicImage::ImageRgba8(image);
    if image.width().max(image.height()) > max_long_edge {
        image = image.resize(max_long_edge, max_long_edge, FilterType::Triangle);
    }
    // JPEG has no alpha channel.
    let rgb = image.into_rgb8();
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, JPEG_QUALITY).write_image(
        &rgb,
        rgb.width(),
        rgb.height(),
        ExtendedColorType::Rgb8,
    )?;
    Ok(EncodedFrame {
        width: rgb.width(),
        height: rgb.height(),
        jpeg,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(pid: u32, focused: bool, width: u32, height: u32) -> WindowInfo {
        WindowInfo {
            pid,
            focused,
            minimized: false,
            width,
            height,
        }
    }

    #[test]
    fn picks_topmost_sizeable_window_of_the_frontmost_app() {
        let windows = [
            window(7, false, 1920, 1080),
            window(42, true, 200, 30),
            window(42, true, 1600, 900),
            window(42, true, 1280, 720),
        ];
        assert_eq!(choose_target(&windows, 1), Some(2));
    }

    #[test]
    fn skips_own_and_minimized_windows() {
        let mut minimized = window(42, true, 1600, 900);
        minimized.minimized = true;
        let windows = [window(1, true, 380, 300), minimized];
        assert_eq!(choose_target(&windows, 1), None);
    }

    #[test]
    fn downscales_large_frames_to_the_long_edge() {
        let frame = encode_jpeg(RgbaImage::new(3840, 2160), MAX_LONG_EDGE).unwrap();
        assert_eq!((frame.width, frame.height), (1920, 1080));
        assert_eq!(
            &frame.jpeg[..2],
            &[0xFF, 0xD8],
            "JPEG start-of-image marker"
        );
    }

    #[test]
    fn keeps_small_frames_as_they_are() {
        let frame = encode_jpeg(RgbaImage::new(800, 600), MAX_LONG_EDGE).unwrap();
        assert_eq!((frame.width, frame.height), (800, 600));
    }
}
