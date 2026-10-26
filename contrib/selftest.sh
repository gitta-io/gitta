#!/bin/sh
# Smoke test for the gitta integrations: run each script against sample-audit.jsonl
# and check the key numbers. No gitta binary, no network. Needs jq.
set -eu
D=$(cd -- "$(dirname -- "$0")" && pwd)
T=$(mktemp -d); ROOT=$(mktemp -d); trap 'rm -rf "$T" "$ROOT"' EXIT
mkdir -p "$T/.gitta"; cp "$D/sample-audit.jsonl" "$T/.gitta/audit.jsonl"

fail() { echo "FAIL: $1" >&2; exit 1; }

# gitta-feed: the sample has 4 landed, 3 refused, 2 of them serious.
feed=$("$D/gitta-feed" "$T")
[ "$(printf '%s' "$feed" | jq .landed)"  = 4 ] || fail "feed landed"
[ "$(printf '%s' "$feed" | jq .refused)" = 3 ] || fail "feed refused"
[ "$(printf '%s' "$feed" | jq .serious)" = 2 ] || fail "feed serious"

# waybar: valid JSON, class "refused" because a serious refusal happened.
w=$("$D/waybar/gitta-status" "$T")
printf '%s' "$w" | jq -e . >/dev/null || fail "waybar not JSON"
[ "$(printf '%s' "$w" | jq -r .class)" = refused ] || fail "waybar class"

# the other frontends produce the refused count and exit 0.
"$D/gitta-tmux" "$T"       | grep -q "3 refused"   || fail "tmux"
"$D/gitta-i3blocks" "$T"   | grep -q "3 refused"   || fail "i3blocks"
"$D/gitta-notify" "$T"     | grep -q "3 refused"   || fail "notify"
"$D/gitta-dashboard" "$T"  | grep -q "const DATA=" || fail "dashboard"

# dashboard: a "</script>" in an agent name (a legal branch name) must not end the
# script element early.
mkdir -p "$T/evil/.gitta"
printf '%s\n' '{"seq":1,"event":"land_refused","agent":"x</script><b>","data":{"check":"tests"}}' > "$T/evil/.gitta/audit.jsonl"
[ "$("$D/gitta-dashboard" "$T/evil" | grep -o '</script>' | wc -l)" -eq 1 ] || fail "dashboard escaping"

# keep the root-scan cache inside the test's temp dir
export XDG_RUNTIME_DIR="$T/run"; mkdir -p "$XDG_RUNTIME_DIR"

# aggregate mode: a root holding two repos sums their trails (repos=2).
mkdir -p "$ROOT/r1/.gitta" "$ROOT/r2/.gitta"
cp "$D/sample-audit.jsonl" "$ROOT/r1/.gitta/audit.jsonl"
cp "$D/sample-audit.jsonl" "$ROOT/r2/.gitta/audit.jsonl"
agg=$(GITTA_SCAN_TTL=0 "$D/gitta-feed" "$ROOT")
[ "$(printf '%s' "$agg" | jq .repos)"   = 2 ] || fail "aggregate repos"
[ "$(printf '%s' "$agg" | jq .refused)" = 6 ] || fail "aggregate refused"
[ "$(printf '%s' "$agg" | jq .landed)"  = 8 ] || fail "aggregate landed"

# off state: no .gitta -> feed says on:false, waybar hides (empty text).
"$D/gitta-feed" "$T/nope"  | grep -q '"on":false' || fail "off feed"
[ "$("$D/waybar/gitta-status" "$T/nope" | jq -r .text)" = "" ] || fail "off waybar"

echo "ok: all integrations pass"
