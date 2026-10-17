use cue_core::{Config, Tier};
use gpui_kit::*;

struct CueWindow {
    config: Config,
    config_error: Option<String>,
}

impl CueWindow {
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
}

impl Render for CueWindow {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
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
            .children(self.config_error.clone())
    }
}

fn load_config() -> (Config, Option<String>) {
    match Config::default_path().and_then(|path| Config::load_from(&path)) {
        Ok(config) => (config, None),
        Err(err) => (Config::default(), Some(format!("Config error: {err}"))),
    }
}

fn main() {
    let (config, config_error) = load_config();

    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);

        let bounds = Bounds::centered(None, size(px(420.), px(260.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, move |_, cx| {
            cx.new(|_| CueWindow {
                config,
                config_error,
            })
        })
        .expect("failed to open the Cue window");
        cx.activate(true);
    });
}
