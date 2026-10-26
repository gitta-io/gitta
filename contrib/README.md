# gitta integrations

One event stream, many frontends — show what gitta is doing wherever you already
look.

gitta writes `.gitta/audit.jsonl` in every repo it manages: one hash-chained JSON
object per event (`landed`, `land_refused`, `agent_started`, …), each carrying the
gate command, its result and seconds. These scripts only **read** that file. They
never run the gitta binary, so they are agnostic to the editor, agent or AI
frontend driving gitta — the same stream feeds a status bar, a terminal, an
editor or a notifier equally, and polling is cheap and safe.

They all show gitta's **impact**, not its plumbing: how many merges it **refused**
(bad code kept off the branch) next to how many passed and **landed**. Refused and
landed are coloured differently — a refusal is an alert, a landing is good — and a
serious refusal (credentials, security, weakened checks, policy) is called out in
red with a ⚠.

![the Waybar module](media/waybar.png)

## What's here

- **`gitta-feed`** — the core. Emits a neutral JSON summary
  (`{on, landed, refused, serious, by_check, last}`) for one repo. Every other
  script formats this, so they never re-parse the trail.
- **`waybar/`** — a [Waybar](https://github.com/Alexays/Waybar) module (its own
  README; colours and glyphs are configurable).
- **`gitta-tmux`** — a tmux `status-right` segment.

  ![the tmux segment](media/tmux.png)
- **`gitta-i3blocks`** — a block for **swaybar** (sway) and **i3bar** (i3), via
  [i3blocks](https://github.com/vivien/i3blocks).
- **`gitta-notify`** — a one-shot status line; `--toast` also calls `notify-send`.
- **`gitta-dashboard`** — generates a self-contained HTML page (tiles, a
  refused-by-reason chart and a timeline) from the trail. No server.

  ![the dashboard](media/dashboard.png)
- **`sample-audit.jsonl`** — a sample trail to try any of them against.

## Use

Each takes a path (an argument, `$GITTA_REPO`, or the current directory) and
needs `jq`. Give it **one repo**, or a **root directory** — if the path is not
itself a gitta repo, it aggregates every repo's trail found under it, so you set
it once and never repoint as you move between repos:

```sh
./gitta-feed ~/src/myrepo          # one repo
./gitta-feed ~/src                 # every gitta repo under ~/src, combined
./gitta-dashboard ~/src > dashboard.html
```

The root scan is cached (`GITTA_SCAN_TTL`, default 60s; `GITTA_SCAN_DEPTH`,
default 6), so a polled bar stays cheap; a new gitta repo shows up within the TTL.

Try them against the sample without a real repo:

```sh
mkdir -p /tmp/demo/.gitta && cp sample-audit.jsonl /tmp/demo/.gitta/audit.jsonl
./gitta-tmux /tmp/demo
```

Writing another frontend is a few lines: call `gitta-feed`, format its JSON.

## A cleaner read, later

Today these read the audit trail directly. A `gitta status --json` with
cumulative counts, or an atomically-rewritten `.gitta/status.json`, would let a
consumer read current state without walking the chain.
