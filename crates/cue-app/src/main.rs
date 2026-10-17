mod overlay;
mod platform;

use cue_core::{Config, Tier};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::*;

use crate::overlay::Overlay;

/// The regular app window: settings now, later also history.
struct ControlPanel {
    config: Config,
    error: Option<String>,
    overlay: WindowHandle<Overlay>,
}

impl ControlPanel {
    fn model_label(&self, tier: Tier) -> String {
        match self.config.model_for(tier) {
            Some(model) => {
                let account = self
                    .config
                    .account(&model.account_id)
                    .map_or("unknown account", |account| account.display_name());
                format!("{} ({account})", model.model)
            }
            None => "not set".into(),
        }
    }

    fn toggle_click_through(&mut self, cx: &mut Context<Self>) {
        let result = self.overlay.update(cx, |overlay, window, cx| {
            let enabled = !overlay.click_through();
            overlay.set_click_through(enabled, window, cx)
        });
        self.error = match result {
            Ok(Ok(())) => None,
            Ok(Err(err)) | Err(err) => Some(format!("Couldn't change click-through: {err}")),
        };
        cx.notify();
    }
}

impl Render for ControlPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let click_through = self
            .overlay
            .read(cx)
            .is_ok_and(|overlay| overlay.click_through());

        div()
            .flex()
            .flex_col()
            .gap_2()
            .size_full()
            .p_4()
            .child(div().text_xl().child("Cue"))
            .child(format!("Fast model: {}", self.model_label(Tier::Fast)))
            .child(format!("Deep model: {}", self.model_label(Tier::Deep)))
            .child(format!("Ask: {}", self.config.hotkeys.ask))
            .child(format!("Ask (deep): {}", self.config.hotkeys.ask_deep))
            .child(
                Button::new("toggle-click-through")
                    .primary()
                    .label(if click_through {
                        "Overlay: click-through (click to make interactive)"
                    } else {
                        "Overlay: interactive (click to pass clicks through)"
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_click_through(cx))),
            )
            .children(self.error.clone())
    }
}

fn load_config() -> (Config, Option<String>) {
    match Config::default_path().and_then(|path| Config::load_from(&path)) {
        Ok(config) => (config, None),
        Err(err) => (Config::default(), Some(format!("Config error: {err}"))),
    }
}

fn main() {
    let (config, error) = load_config();

    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);

        let overlay = overlay::open(cx).expect("failed to open the overlay");

        let bounds = Bounds::centered(None, size(px(460.), px(300.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, move |_, cx| {
            cx.new(|_| ControlPanel {
                config,
                error,
                overlay,
            })
        })
        .expect("failed to open the Cue window");
        cx.activate(true);
    });
}
