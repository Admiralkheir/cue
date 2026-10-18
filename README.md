# Cue

[![CI](https://github.com/Admiralkheir/cue/actions/workflows/ci.yml/badge.svg)](https://github.com/Admiralkheir/cue/actions/workflows/ci.yml)

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

Windows and macOS are both first-class targets; so far Cue has been run on macOS and
built and tested in CI on Windows. Linux builds are experimental: hotkeys only work on
X11, and the overlay can't pass clicks through or hide itself yet.

## Download

Get the latest build from [Releases](https://github.com/Admiralkheir/cue/releases):
a universal `Cue.app` for macOS 13+, a zip with `cue.exe` for Windows 10/11, and a
tarball for Linux (x86_64, experimental). The builds aren't code-signed yet, so macOS
and Windows ask before the first launch; the release notes say how to get past that.

## Building

Cue is written in Rust on top of [GPUI](https://gpui.rs) (Zed's UI framework) through
[GPUI Kit](https://gpui-kit.com).

```sh
cargo run -p cue-app
```

On macOS the Xcode Command Line Tools are enough, since Metal shaders are compiled at
startup. On Linux, `scripts/install-linux-deps.sh` installs the system libraries
(Debian/Ubuntu). `scripts/package-macos.sh` builds the universal `Cue.app` locally.

When something goes wrong, Cue writes logs to `~/Library/Logs/Cue` (macOS),
`%LOCALAPPDATA%\Cue\logs` (Windows) or `~/.local/state/cue` (Linux); Settings →
Troubleshooting opens the folder.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for the ground rules, the checks CI runs and some
development shortcuts.

## License

[MIT](LICENSE). Cue builds on many open-source crates; their licenses are collected with
[cargo-about](https://github.com/EmbarkStudios/cargo-about) (`cargo about generate
about.hbs --output-file THIRD_PARTY_LICENSES.html`), and CI publishes the result as an
artifact.
