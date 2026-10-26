# gitta on Waybar

A small [Waybar](https://github.com/Alexays/Waybar) module that shows gitta's
impact: how many merges it has **refused** — bad code kept off your branch — next
to how many passed and **landed**. Refused and landed are coloured differently (a
refusal is an alert, a landing is good), and a serious refusal — credentials,
security, weakened checks or policy — turns the count red and adds a ⚠. The
tooltip breaks the refusals down by reason.

![the module on a bar](../media/waybar.png)

## Why it works everywhere

gitta already writes an event stream to `.gitta/audit.jsonl` in every repo it
manages — one hash-chained JSON object per event (`landed`, `land_refused`,
`agent_started`), each carrying the gate command, its result and seconds. This
module only *reads* that file. It never runs gitta, so it is agnostic to the
editor, agent or AI frontend that drives gitta, and the same stream feeds a tmux
statusline, a swaybar/i3bar block, an editor plugin or a one-shot `notify-send`
just as well (see `../`).

## Install

1. Copy `gitta-status` to `~/.config/waybar/` and `chmod +x` it. Needs `jq` and a
   Nerd Font in your bar (for the git and warning glyphs).
2. Add `"custom/gitta"` to a modules list in `~/.config/waybar/config` and paste
   the block from `waybar-config.jsonc`. Point `exec` at your projects root (e.g.
   `~/src`) to aggregate every gitta repo under it, or at a single repo.
3. Append `style.css` to `~/.config/waybar/style.css`.

## Colours

The module colours its text with Pango markup and reads the colours from
`GITTA_COLOR_*` env vars, set in the `exec` line (see `waybar-config.jsonc`) — so
you match your bar's palette without editing the script. `waybar-config.jsonc`
shows Tokyo Night Storm values; the script's defaults are Catppuccin. The `.css`
only gives the module the same background and padding as your other modules.

## One repo or many

Point `exec` at a single repo, or at a root directory (e.g. `~/src`): if the path
is not itself a gitta repo, the module aggregates every repo's `.gitta/audit.jsonl`
found under it, so you set it once and new gitta repos appear on their own — no
need to repoint or run several instances. The directory scan is cached
(`GITTA_SCAN_TTL`, default 60s; `GITTA_SCAN_DEPTH`, default 6), so polling stays cheap.

## A cleaner read, upstream

Today the module reads the audit trail to infer current state. A small addition
to gitta itself — `gitta status --json` with cumulative counts, or an
atomically-rewritten `.gitta/status.json` — would let any consumer read current
state without walking the chain, and make integrations like this one a one-liner.
