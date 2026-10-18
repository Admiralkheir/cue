# Cue

An open-source, AI-powered game assistant. Press a hotkey while you play, Cue grabs the
game window, and you ask your question: "how do I solve this puzzle?", "what's the best
way to deal with this boss?". The answer streams into a translucent overlay on top of
the game.

> **Status:** early development. It works on macOS with your own API key, with plenty of rough edges.

## Principles

- **Screen capture only.** Cue never reads game memory, injects code or automates input,
  so it stays clear of anti-cheat systems. It sees exactly what you see.
- **Bring your own key.** Add keys for one or more providers (Anthropic, OpenAI,
  Google Gemini, OpenCode Zen or OpenCode Go; local Ollama is planned). Cue lists the
  models each key can reach and suggests a pick per tier, but you choose.
- **Two tiers.** A *fast* model for quick in-game questions and a *deep* model for
  strategy, each with its own hotkey.
- **You decide what leaves your machine.** Cue captures only when you press the hotkey.
  Keys are stored in the OS keychain. No telemetry.

## Planned for the first release

- [x] Translucent, always-on-top, click-through overlay (borderless/windowed games)
- [x] Global hotkeys for the fast and deep tiers
- [x] Capture of the game window only
- [x] Streaming answers with follow-up questions on the same capture
- [x] Game detection from the active window, plus per-game profiles (notes and models)
- [x] Providers: Anthropic, OpenAI, Google Gemini, OpenCode Zen and OpenCode Go
- [ ] Local models through Ollama

Windows and macOS are both first-class targets, but so far Cue has only been built and
run on macOS. The Windows code paths haven't been compiled yet.

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
| `crates/cue-core` | UI-independent core: config, capture, provider clients, key storage, model recommendations |
| `crates/cue-app` | The GPUI desktop app (`cue` binary) |

## License

[MIT](LICENSE)
