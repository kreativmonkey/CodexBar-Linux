#!/usr/bin/env bash
# Install build dependencies for CI and local release builds on Debian/Ubuntu.
set -euo pipefail

export DEBIAN_FRONTEND=noninteractive

sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  curl \
  git \
  pkg-config \
  libgtk-4-dev \
  libglib2.0-dev \
  libcairo2-dev \
  libpango1.0-dev \
  libgdk-pixbuf-2.0-dev \
  libgraphene-1.0-dev \
  libwayland-dev \
  wayland-protocols \
  meson \
  ninja-build \
  libgirepository-2.0-dev \
  libssl-dev

if pkg-config --exists gtk4-layer-shell-0; then
  echo "gtk4-layer-shell already available via pkg-config"
else
  echo "Building gtk4-layer-shell from source (not in default Ubuntu 24.04 repos)…"
  tmpdir="$(mktemp -d)"
  trap 'rm -rf "$tmpdir"' EXIT
  git clone --depth 1 https://github.com/wmww/gtk4-layer-shell "$tmpdir/gtk4-layer-shell"
  meson setup "$tmpdir/build" "$tmpdir/gtk4-layer-shell" \
    --prefix=/usr \
    -Dexamples=false \
    -Dtests=false \
    -Dvapi=false \
    -Dintrospection=false
  sudo ninja -C "$tmpdir/build" install
  sudo ldconfig
fi
