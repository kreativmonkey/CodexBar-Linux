# CodexBar-Linux

Linux rewrite of [CodexBar](https://github.com/steipete/CodexBar) (macOS): a
system-tray monitor for AI coding provider usage and rate limits. Written in
Rust; tray via StatusNotifierItem (ksni), popover via GTK4 + layer-shell —
built for Wayland compositors like niri with an SNI-capable bar (Quickshell,
Waybar, …).

## Features

- Tray icon with live ring gauge (highest utilization across providers)
- Click → styled popover: per-provider usage bars, reset countdowns, plan badge
- Providers: Claude (Claude Code OAuth), Codex (ChatGPT OAuth) — pluggable
  architecture, more to come
- Auto-detects providers from existing CLI logins; no separate login needed
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
```

## Credentials

Read-only reuse of existing CLI sessions:

- Claude: `~/.claude/.credentials.json` (written by `claude` login; token is
  refreshed in place when expired)
- Codex: `~/.codex/auth.json` (written by `codex` login)

No credentials are stored elsewhere; requests go only to the providers' own
usage endpoints.
