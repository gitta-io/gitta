#!/bin/sh
# Smoke test for the gitta integrations: run each script against sample-audit.jsonl
# and check the key numbers. No gitta binary, no network. Needs jq.
set -eu
D=$(cd -- "$(dirname -- "$0")" && pwd)
T=$(mktemp -d); ROOT=$(mktemp -d); trap 'rm -rf "$T" "$ROOT"' EXIT
mkdir -p "$T/.gitta"; cp "$D/sample-audit.jsonl" "$T/.gitta/audit.jsonl"

fail() { echo "FAIL: $1" >&2; exit 1; }

# gitta-feed: the sample has 4 landed, 3 blocked, 2 of them serious.
feed=$("$D/gitta-feed" "$T")
[ "$(printf '%s' "$feed" | jq .landed)"  = 4 ] || fail "feed landed"
[ "$(printf '%s' "$feed" | jq .blocked)" = 3 ] || fail "feed blocked"
[ "$(printf '%s' "$feed" | jq .serious)" = 2 ] || fail "feed serious"

# waybar: valid JSON, class "refused" because a serious block happened.
w=$("$D/waybar/gitta-status" "$T")
printf '%s' "$w" | jq -e . >/dev/null || fail "waybar not JSON"
[ "$(printf '%s' "$w" | jq -r .class)" = refused ] || fail "waybar class"

# the other frontends produce the blocked count and exit 0.
"$D/gitta-tmux" "$T"       | grep -q "3 blocked"   || fail "tmux"
"$D/gitta-i3blocks" "$T"   | grep -q "3 blocked"   || fail "i3blocks"
"$D/gitta-notify" "$T"     | grep -q "3 blocked"   || fail "notify"
"$D/gitta-dashboard" "$T"  | grep -q "const DATA=" || fail "dashboard"

# aggregate mode: a root holding two repos sums their trails (repos=2).
mkdir -p "$ROOT/r1/.gitta" "$ROOT/r2/.gitta"
cp "$D/sample-audit.jsonl" "$ROOT/r1/.gitta/audit.jsonl"
cp "$D/sample-audit.jsonl" "$ROOT/r2/.gitta/audit.jsonl"
agg=$(GITTA_SCAN_TTL=0 "$D/gitta-feed" "$ROOT")
[ "$(printf '%s' "$agg" | jq .repos)"   = 2 ] || fail "aggregate repos"
[ "$(printf '%s' "$agg" | jq .blocked)" = 6 ] || fail "aggregate blocked"
[ "$(printf '%s' "$agg" | jq .landed)"  = 8 ] || fail "aggregate landed"

# off state: no .gitta -> feed says on:false, waybar hides (empty text).
"$D/gitta-feed" "$T/nope"  | grep -q '"on":false' || fail "off feed"
[ "$("$D/waybar/gitta-status" "$T/nope" | jq -r .text)" = "" ] || fail "off waybar"

echo "ok: all integrations pass"
