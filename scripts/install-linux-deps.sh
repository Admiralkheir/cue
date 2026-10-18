#!/usr/bin/env bash
# System libraries Cue needs to build on Debian or Ubuntu: GPUI (X11, Wayland, fonts,
# Vulkan) and xcap's screen capture (XCB, PipeWire, D-Bus).
set -euo pipefail

sudo apt-get update
sudo apt-get install -y --no-install-recommends \
    pkg-config clang libclang-dev \
    libx11-dev libx11-xcb-dev libxcb1-dev libxcb-randr0-dev libxcb-shm0-dev \
    libxcb-xfixes0-dev libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev \
    libfontconfig-dev libfreetype-dev libvulkan-dev \
    libpipewire-0.3-dev libspa-0.2-dev libdbus-1-dev libgbm-dev libegl-dev \
    libssl-dev libzstd-dev
