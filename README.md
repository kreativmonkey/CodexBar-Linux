# CodexBar-Linux

Linux rewrite of [CodexBar](https://github.com/steipete/CodexBar) (macOS): a
system-tray monitor for AI coding provider usage and rate limits. Written in
Rust; tray via StatusNotifierItem (ksni), popover via GTK4 + layer-shell —
built for Wayland compositors like niri with an SNI-capable bar (Quickshell,
Waybar, …).

## Features

- One tray icon per active provider: original provider logo inside a live
  progress ring (green/orange/red by utilization)
- Click an icon → styled popover with a provider switcher bar, usage bars,
  reset countdowns, plan badge and credits
- Providers: Claude, Codex, Gemini, GitHub Copilot, Cursor, OpenRouter,
  OpenAI, Mistral, DeepSeek, Groq, Grok*, Perplexity* (pluggable — one file
  per provider). *Grok/Perplexity expose no API-key billing endpoint; they
  show an actionable hint instead of data.
- Auto-detects providers from existing CLI logins and API keys; appears/
  disappears without restart
- Refresh every 5 min (configurable), manual refresh from tray menu/popover

## Install (Nix)

```sh
nix profile install .
systemctl --user enable --now codexbar   # unit shipped in contrib/
```

## Install (binary)

Pre-built Linux binaries are attached to
[GitHub Releases](https://github.com/kreativmonkey/CodexBar-Linux/releases)
for tagged versions (`v0.1.0`, …).

### Download

| Asset | Architecture |
|-------|--------------|
| `codexbar-x86_64-linux-v*.tar.gz` | Intel/AMD 64-bit |
| `codexbar-aarch64-linux-v*.tar.gz` | ARM64 (e.g. Raspberry Pi, Apple Silicon Linux VMs) |

Optional checksum verification:

```sh
sha256sum -c codexbar-x86_64-linux-v0.1.0.tar.gz.sha256
```

### Install the binary

```sh
tar xzf codexbar-x86_64-linux-v0.1.0.tar.gz
install -Dm755 codexbar-x86_64-linux ~/.local/bin/codexbar
```

Ensure `~/.local/bin` is on your `PATH`.

### Runtime dependencies

The release binary is dynamically linked against GTK 4 and layer-shell. Install
the matching packages for your distro:

**Arch Linux**

```sh
sudo pacman -S gtk4 gtk4-layer-shell
```

**Fedora**

```sh
sudo dnf install gtk4 gtk4-layer-shell
```

**Ubuntu / Debian**

```sh
sudo apt install libgtk-4-1 libgraphene-1.0-0 libgdk-pixbuf-2.0-0 \
  libpango-1.0-0 libcairo2 libwayland-client0
```

`gtk4-layer-shell` is not packaged on all Ubuntu/Debian releases yet. If the
app fails to start with `libgtk4-layer-shell.so` missing, build and install it
from source — see `contrib/ci-install-deps.sh` for the exact steps.

You also need a Wayland session with a StatusNotifierItem (system tray) host
such as Quickshell or Waybar.

### Autostart

```sh
mkdir -p ~/.config/systemd/user
sed 's|\.nix-profile/bin/codexbar|.local/bin/codexbar|' contrib/codexbar.service \
  > ~/.config/systemd/user/codexbar.service
systemctl --user daemon-reload
systemctl --user enable --now codexbar
```

## Develop

```sh
nix develop   # or direnv
just          # list recipes: build, run, test, lint, check
```

## Configuration

`~/.config/codexbar/config.toml` (all optional):

```toml
refresh_secs = 300
providers = []            # empty = auto-detect; e.g. ["claude", "codex"]
popover_margin_top = 8
popover_margin_right = 8

# API keys for key-based providers. Environment variables take precedence:
# OPENROUTER_API_KEY, OPENAI_ADMIN_KEY/OPENAI_API_KEY, MISTRAL_API_KEY,
# DEEPSEEK_API_KEY, GROQ_API_KEY, CURSOR_SESSION_TOKEN, COPILOT_API_TOKEN.
[keys]
# openrouter = "sk-or-…"
# cursor = "<WorkosCursorSessionToken cookie value>"
```

## Credentials

Read-only reuse of existing CLI sessions:

- Claude: `~/.claude/.credentials.json` (written by `claude` login; token is
  refreshed in place when expired)
- Codex: `~/.codex/auth.json` (written by `codex` login)

No credentials are stored elsewhere; requests go only to the providers' own
usage endpoints.
