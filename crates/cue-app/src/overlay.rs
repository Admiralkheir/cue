//! The translucent panel that floats above the game.

use anyhow::Result;
use gpui_kit::*;

use crate::platform;

const SIZE: Size<Pixels> = Size {
    width: px(380.),
    height: px(200.),
};
const MARGIN: Pixels = px(24.);

pub struct Overlay {
    click_through: bool,
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
}

impl Render for Overlay {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let mode = if self.click_through {
            "click-through"
        } else {
            "interactive"
        };

        // The window itself is transparent; only this rounded panel is drawn.
        div().size_full().p_2().child(
            div()
                .size_full()
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
                        .child(div().text_xs().text_color(rgb(0x9aa0a6)).child(mode)),
                )
                .child(
                    div()
                        .text_sm()
                        .child("Press the hotkey to ask about what's on screen."),
                ),
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
        })
    })
}
