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
- Auto-detects providers from existing CLI logins and API keys; appears/
  disappears without restart
- Refresh every 5 min (configurable), manual refresh from tray menu/popover

## Supported providers

| Provider | ID | Auth | Status |
|----------|----|------|--------|
| Claude | `claude` | `claude` CLI session (`~/.claude/.credentials.json`) | **Tested** |
| Codex | `codex` | `codex` CLI session (`~/.codex/auth.json`) | **Tested** |
| Gemini | `gemini` | Gemini CLI OAuth (`~/.gemini/oauth_creds.json`) | **Tested** |
| Cursor | `cursor` | Cursor app DB or `CURSOR_SESSION_TOKEN` cookie | **Tested** |
| GitHub Copilot | `copilot` | `COPILOT_API_TOKEN` / `[keys] copilot` | Untested |
| OpenRouter | `openrouter` | `OPENROUTER_API_KEY` / OpenCode or Pi `auth.json` / `[keys] openrouter` | Untested |
| OpenCode Zen | `opencode_zen` | OpenCode `auth.json` / `OPENCODE_ZEN_API_KEY` / `[keys] opencode_zen` | No billing API |
| OpenAI | `openai` | `OPENAI_ADMIN_KEY` or `OPENAI_API_KEY` / OpenCode or Pi `auth.json` | Untested |
| Mistral | `mistral` | `MISTRAL_API_KEY` / OpenCode or Pi `auth.json` / `[keys] mistral` | Untested |
| DeepSeek | `deepseek` | `DEEPSEEK_API_KEY` / OpenCode or Pi `auth.json` / `[keys] deepseek` | Untested |
| Groq | `groq` | `GROQ_API_KEY` / OpenCode or Pi `auth.json` / `[keys] groq` | Untested |
| Grok | `grok` | `XAI_API_KEY` / Pi `auth.json` (`xai`) (inference only) | No billing API |
| Perplexity | `perplexity` | Session cookie (no API-key billing endpoint) | No billing API |

**Tested** — verified against live accounts on Linux (usage bars, resets, token
refresh where applicable).

**Untested** — provider module and unit tests exist, but no live end-to-end
verification yet. Bug reports welcome; response mapping may need adjustment.

**No billing API** — the provider can appear when configured, but xAI and
Perplexity do not expose usage/credits via their public API keys. Grok shows an
actionable hint; Perplexity requires a browser session cookie (same limitation
as the macOS original). OpenCode Zen keys are read from the OpenCode CLI
(`~/.local/share/opencode/auth.json`), but OpenCode does not publish a balance
endpoint for API keys yet.

Each provider lives in one file under `src/providers/`.

## Versioning

Releases use calendar versioning: **`vYY.MM.PATCH`**

| Part | Meaning | Example |
|------|---------|---------|
| `YY` | Year (two digits) | `26` → 2026 |
| `MM` | Month | `07` → July |
| `PATCH` | Release index within that month (starts at `0`) | `0`, `1`, … |

Git tags and GitHub Release assets use this form literally, e.g. `v26.07.0`.
`Cargo.toml` stores the same value as Rust semver without leading zeros in the
month field (`26.7.0` ↔ tag `v26.07.0`).

The early tag `v0.2.0` predates this scheme; treat **`v26.07.0`** as its
calver equivalent.

## Install (Nix)

```sh
nix profile install .
systemctl --user enable --now codexbar   # unit shipped in contrib/
```

## Install (binary)

Pre-built Linux binaries are attached to
[GitHub Releases](https://github.com/kreativmonkey/CodexBar-Linux/releases)
for tagged versions (`v26.07.0`, …).

### Download

| Asset | Architecture |
|-------|--------------|
| `codexbar-x86_64-linux-v*.tar.gz` | Intel/AMD 64-bit |
| `codexbar-aarch64-linux-v*.tar.gz` | ARM64 (e.g. Raspberry Pi, Apple Silicon Linux VMs) |

Optional checksum verification:

```sh
sha256sum -c codexbar-x86_64-linux-v26.07.0.tar.gz.sha256
```

### Install the binary

```sh
tar xzf codexbar-x86_64-linux-v26.07.0.tar.gz
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
# OPENROUTER_API_KEY, OPENCODE_ZEN_API_KEY, OPENAI_ADMIN_KEY/OPENAI_API_KEY, MISTRAL_API_KEY,
# DEEPSEEK_API_KEY, GROQ_API_KEY, CURSOR_SESSION_TOKEN, COPILOT_API_TOKEN.
[keys]
# openrouter = "sk-or-…"
# opencode_zen = "sk-…"
# cursor = "<WorkosCursorSessionToken cookie value>"
```

## Credentials

Read-only reuse of existing CLI sessions:

- Claude: `~/.claude/.credentials.json` (written by `claude` login; token is
  refreshed in place when expired)
- Codex: `~/.codex/auth.json` (written by `codex` login)

No credentials are stored elsewhere; requests go only to the providers' own
usage endpoints.

OpenCode CLI logins (`~/.local/share/opencode/auth.json`) and Pi agent logins
(`~/.pi/agent/auth.json`, or `$PI_CODING_AGENT_DIR/auth.json`) are reused
read-only for OpenRouter, OpenCode Zen, and other API-key providers when no
explicit key is set. OpenCode is checked before Pi.
