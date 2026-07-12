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
