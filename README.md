# Cue

An open-source, AI-powered game assistant. Press a hotkey while you play, Cue grabs the
game window, and you ask your question: "how do I solve this puzzle?", "what's the best
way to deal with this boss?". The answer streams into a translucent overlay on top of
the game.

> **Status:** early development. Nothing is usable yet.

## Principles

- **Screen capture only.** Cue never reads game memory, injects code or automates input,
  so it stays clear of anti-cheat systems. It sees exactly what you see.
- **Bring your own key.** Add keys for one or more providers (Anthropic, OpenAI,
  OpenCode Zen, Gemini, or a local Ollama). Cue lists the models each key can reach and
  suggests a pick per tier, but you choose.
- **Two tiers.** A *fast* model for quick in-game questions and a *deep* model for
  strategy, each with its own hotkey.
- **You decide what leaves your machine.** Cue captures only when you press the hotkey.
  Keys are stored in the OS keychain. No telemetry.

## Planned for the first release

- [ ] Translucent, always-on-top, click-through overlay (borderless/windowed games)
- [ ] Global hotkeys for the fast and deep tiers
- [ ] Capture of the game window only
- [ ] Streaming answers with follow-up questions on the same capture
- [ ] Game detection from the active window, plus per-game profiles
- [ ] Providers: Anthropic, OpenAI, OpenCode Zen, Gemini, Ollama

Windows and macOS are both first-class targets.

## Building

Cue is written in Rust on top of [GPUI](https://gpui.rs) (Zed's UI framework) through
[GPUI Kit](https://gpui-kit.com).

```sh
cargo run -p cue-app
```

On macOS the default `runtime_shaders` feature compiles Metal shaders at startup, so the
Xcode Command Line Tools are enough. With full Xcode installed you can precompile them:

```sh
cargo build -p cue-app --release --no-default-features
```

### Layout

| Crate | Purpose |
| --- | --- |
| `crates/cue-core` | UI-independent core: config, provider accounts, model recommendations |
| `crates/cue-app` | The GPUI desktop app (`cue` binary) |

## License

[MIT](LICENSE)
