# Contributing to Cue

Thanks for helping out! Issues and pull requests are welcome.

## Ground rules

- **Screen capture only.** Cue must never read game memory, inject code, hook a game's
  renderer or automate input. Changes in that direction will be declined, because they
  put players' accounts at risk with anti-cheat systems.
- **Bring your own key.** Don't add a hosted backend, telemetry or analytics. Keys stay in
  the OS keychain and screenshots only go to the provider the user picked.
- **macOS and Windows** are both first-class. If you can only test one, say so in the PR.

## Building

You need a recent stable Rust toolchain.

```sh
cargo run -p cue-app
```

- **macOS:** the Xcode Command Line Tools are enough; Metal shaders are compiled at
  startup. Grant the terminal (or the built app) Screen Recording permission in System
  Settings so Cue can capture windows. `scripts/package-macos.sh` builds `Cue.app`.
- **Windows:** use the MSVC toolchain. Cue captures with Windows Graphics Capture.
- **Linux (experimental):** run `scripts/install-linux-deps.sh` on Debian/Ubuntu first.

Every push to `main` also builds downloadable packages for all three platforms (the
`cue-macos`, `cue-windows` and `cue-linux` artifacts of the CI run), and tagging `vX.Y.Z`
drafts a GitHub release with them.

Each rebuilt development binary has a new code signature, so macOS asks for your login
keychain password the first time it reads a stored API key.

## Checks

CI runs these on macOS and Windows; please run them before opening a PR:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Provider clients are tested against a local mock server (`providers/mock.rs`), so the
tests need no API keys or network access.

## Logs

Cue logs to `~/Library/Logs/Cue` (macOS), `%LOCALAPPDATA%\Cue\logs` (Windows) or
`~/.local/state/cue` (Linux), one file per day, kept for a week. `CUE_LOG=debug` turns up
the detail. Never log API keys, screenshots, questions, answers or window titles.

## Handy for development

- `CUE_ASK_AFTER=3 cargo run -p cue-app` acts as if the fast hotkey was pressed three
  seconds after launch.
- `cargo run -p cue-core --example capture -- out.jpg 3` captures the frontmost window
  after three seconds, the same way the hotkey does, and saves the JPEG.
- `cargo run -p cue-core --example ask -- --model claude-haiku-5-5 --effort medium --search searxng "question"`
  asks a real model with a key from your config, the way the overlay does, and prints the
  answer, each web search and the usage. Every run is billed to that key.

## Layout

| Crate | Purpose |
| --- | --- |
| `crates/cue-core` | UI-independent core: config, game profiles, capture, provider clients, key storage |
| `crates/cue-app` | The GPUI desktop app: overlay, settings, profile editor, hotkeys, native window tweaks |

## Licensing

Cue is MIT licensed, and by contributing you agree that your contributions are too.
New dependencies must use one of the licenses allowed in `deny.toml`; CI checks this
with `cargo-deny`.
