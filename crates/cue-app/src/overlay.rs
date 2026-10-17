//! The translucent panel that floats above the game.

use std::sync::Arc;

use anyhow::Result;
use cue_core::{Capture, Tier, capture_focused_window};
use gpui_kit::*;

use crate::platform;

const SIZE: Size<Pixels> = Size {
    width: px(380.),
    height: px(340.),
};
const MARGIN: Pixels = px(24.);
const MUTED: u32 = 0x9aa0a6;

enum Status {
    Idle,
    Capturing(Tier),
    Captured {
        tier: Tier,
        capture: Capture,
        thumbnail: Arc<Image>,
    },
    Failed(String),
}

pub struct Overlay {
    click_through: bool,
    status: Status,
}

impl Overlay {
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

    /// Captures the frontmost window for a question to the `tier` model.
    pub fn ask(&mut self, tier: Tier, cx: &mut Context<Self>) {
        if matches!(self.status, Status::Capturing(_)) {
            return;
        }
        self.status = Status::Capturing(tier);
        cx.notify();

        let own_pid = std::process::id();
        let capture = cx.background_spawn(async move { capture_focused_window(own_pid) });
        cx.spawn(async move |this, cx| {
            let status = match capture.await {
                Ok(capture) => {
                    let jpeg = capture.frame.jpeg.clone();
                    Status::Captured {
                        tier,
                        capture,
                        thumbnail: Arc::new(Image::from_bytes(ImageFormat::Jpeg, jpeg)),
                    }
                }
                Err(err) => Status::Failed(err.to_string()),
            };
            this.update(cx, |overlay, cx| {
                overlay.status = status;
                cx.notify();
            })
        })
        .detach();
    }

    fn render_status(&self) -> AnyElement {
        match &self.status {
            Status::Idle => div()
                .text_sm()
                .child("Press the hotkey to ask about what's on screen.")
                .into_any_element(),
            Status::Capturing(_) => div()
                .text_sm()
                .text_color(rgb(MUTED))
                .child("Capturing…")
                .into_any_element(),
            Status::Captured {
                capture, thumbnail, ..
            } => {
                let title = if capture.title.is_empty() {
                    capture.app_name.clone()
                } else {
                    format!("{} — {}", capture.app_name, capture.title)
                };
                let frame = &capture.frame;
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        img(thumbnail.clone())
                            .w_full()
                            .h(px(190.))
                            .object_fit(ObjectFit::Contain)
                            .rounded_md(),
                    )
                    .child(div().text_sm().truncate().child(title))
                    .child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                        "{}×{} → {}×{} JPEG, {} KB",
                        capture.source_width,
                        capture.source_height,
                        frame.width,
                        frame.height,
                        frame.jpeg.len().div_ceil(1024)
                    )))
                    .into_any_element()
            }
            Status::Failed(message) => div()
                .text_sm()
                .text_color(rgb(0xf28b82))
                .child(message.clone())
                .into_any_element(),
        }
    }
}

impl Render for Overlay {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let tier = match self.status {
            Status::Capturing(tier) | Status::Captured { tier, .. } => Some(tier),
            Status::Idle | Status::Failed(_) => None,
        };
        let badge = match tier {
            Some(Tier::Fast) => "fast · ",
            Some(Tier::Deep) => "deep · ",
            None => "",
        };
        let mode = if self.click_through {
            "click-through"
        } else {
            "interactive"
        };

        // The window itself is transparent; only this rounded panel is drawn.
        div().size_full().p_2().child(
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap_3()
                .p_4()
                .rounded_xl()
                .bg(rgba(0x101218e0))
                .border_1()
                .border_color(rgba(0xffffff26))
                .shadow_lg()
                .text_color(rgb(0xe8eaed))
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .items_center()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child("Cue"))
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(MUTED))
                                .child(format!("{badge}{mode}")),
                        ),
                )
                .child(self.render_status()),
        )
    }
}

/// Opens the overlay in the top-right corner of the primary display without taking
/// focus away from the game.
pub fn open(cx: &mut App) -> Result<WindowHandle<Overlay>> {
    let screen = cx
        .primary_display()
        .map(|display| display.visible_bounds())
        .unwrap_or_else(|| Bounds::new(Point::default(), size(px(1280.), px(800.))));
    let origin = point(screen.right() - SIZE.width - MARGIN, screen.top() + MARGIN);

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
    // Plain gpui window: gpui-kit's Root would paint the theme background over it.
    cx.open_window(options, |window, cx| {
        if let Err(err) = platform::make_frameless(window) {
            eprintln!("cue: couldn't remove the overlay frame: {err:#}");
        }
        cx.new(|_| Overlay {
            click_through: false,
            status: Status::Idle,
        })
    })
}
