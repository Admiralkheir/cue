## Installing

These builds aren't code-signed yet, so your system asks before running them the first
time. Check a download against `SHA256SUMS.txt` if you like.

**macOS** (13 or later, Apple silicon or Intel): unzip and move `Cue.app` to
Applications. On first launch macOS says it can't verify the developer: open
System Settings → Privacy & Security and click "Open Anyway". Then allow Screen
Recording for Cue when asked, so it can see the game. If Cue still can't see the game
although it's turned on under Screen & System Audio Recording, remove it there with −,
then open Cue and allow it again.

**Windows** (10 or 11, x64): unzip and run `cue.exe`. SmartScreen shows "Windows
protected your PC": click "More info", then "Run anyway".

**Linux** (x86_64, experimental): extract and run `cue/cue`. Hotkeys only work on X11,
and the overlay can't pass clicks through or hide itself yet.

Logs for bug reports: Settings → Troubleshooting → Open log folder.
