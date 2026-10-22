#!/usr/bin/env bash
# Linux test of copy-on-write Claude Code worktrees. Usage: linux-cow-test.sh <gitta-bin-dir> <mode: overlay|fuse>
set -euo pipefail
BIN=$1; MODE=$2; G=$BIN/gitta; export GITTA_FS=$BIN/gitta-fs
T=$(mktemp -d); cd "$T"
echo "== small repo, mode $MODE"
git init -q -b main repo && cd repo
mkdir shop; printf 'def price(x):\n    return x * 2\n' > shop/p.py; printf 'def name(x):\n    return x\n' > shop/n.py
git add -A && git -c user.name=t -c user.email=t@t commit -qm base
$G init --no-claude | sed 's/^/  /'
test -f .claude/settings.local.json && grep -q "hook worktree-create" .claude/settings.local.json && echo "  ok: worktree hooks installed for this repo"
mk() { echo "{\"cwd\":\"$PWD\",\"name\":\"$1\",\"hook_event_name\":\"WorktreeCreate\"}" | $G hook worktree-create | tail -1; }
A=$(mk a1); B=$(mk a2)
echo "  created: $A"; cat .gitta/cow/a1/backend; echo
mount | grep -q " $A " && echo "  ok: $A is mounted"
[ "$(git -C "$A" rev-parse --abbrev-ref HEAD)" = worktree-a1 ] && echo "  ok: own branch"
[ -z "$(git -C "$A" status --porcelain)" ] && echo "  ok: clean status"
printf 'def price(x):\n    return x * 3\n' > "$A/shop/p.py"
printf '\nfrom shop.p import price\n\n\ndef total(x):\n    return price(x)\n' >> "$B/shop/n.py"
[ "$(git -C "$A" status --porcelain)" = " M shop/p.py" ] && echo "  ok: a1 sees only its own change"
grep -q "x \* 2" "$B/shop/p.py" && echo "  ok: a2 does not see a1's change"
grep -q "x \* 2" .gitta/shared/*/shop/p.py && echo "  ok: shared checkout untouched"
[ "$(find .gitta/cow/a1/upper -type f | grep -v '\.git$' | wc -l)" = 1 ] && echo "  ok: a1's layer holds only the changed file"
$G status | sed 's/^/  /'
echo "{\"cwd\":\"$B\"}" | $G hook claude | grep -q "price" && echo "  ok: a2 warned through its hook"
git -C "$A" -c user.name=t -c user.email=t@t commit -qam "a1 work" && echo "  ok: commit inside the view"
echo "{\"cwd\":\"$PWD\",\"worktree_path\":\"$B\"}" | $G hook worktree-remove; [ ! -d "$B" ] || [ -z "$(ls -A "$B" 2>/dev/null)" ] && echo "  ok: worktree-remove unmounted and removed a2"
$G uninstall | sed 's/^/  /'
mount | grep -q " $A " && echo "  FAIL: still mounted" || echo "  ok: unmounted"
grep -q "x \* 3" "$A/shop/p.py" && [ "$(git -C "$A" log -1 --format=%s)" = "a1 work" ] && echo "  ok: a1's work kept as an ordinary worktree"
echo "== large repo (VS Code), mode $MODE"
cd "$T" && git clone -q --depth 1 https://github.com/microsoft/vscode.git big && cd big
$G init --no-claude >/dev/null
before=$(df -k --output=used . | tail -1)
s=$(date +%s.%N); for n in 1 2 3 4 5; do mk v$n >/dev/null; done; e=$(date +%s.%N)
after=$(df -k --output=used . | tail -1)
echo "  5 worktrees: $(echo "$e - $s" | bc) s, disk used $(( (after-before)/1024 )) MB (full checkout is $(du -sm .gitta/shared | cut -f1) MB)"
s=$(date +%s.%N); git worktree add -q -b plain ../plain; e=$(date +%s.%N); echo "  one ordinary worktree for comparison: $(echo "$e - $s" | bc) s"
V=$(pwd)/.claude/worktrees/v1; s=$(date +%s.%N); git -C "$V" status --porcelain | wc -l; e=$(date +%s.%N); echo "  git status in a view: $(echo "$e - $s" | bc) s"
for n in 1 2 3 4 5; do echo "{\"cwd\":\"$PWD\",\"worktree_path\":\"$PWD/.claude/worktrees/v$n\"}" | $G hook worktree-remove; done
$G uninstall >/dev/null; git worktree remove --force ../plain
echo "== big done"
echo "== build workload (Django: compile every module, then read every file), mode $MODE"
cd "$T" && rm -rf dj && git clone -q --depth 1 https://github.com/django/django.git dj && cd dj && $G init --no-claude >/dev/null
git worktree add -q --detach "$T/dj-native"
work() { local d=$1; local s=$(date +%s.%N); python3 -m compileall -q "$d/django" >/dev/null 2>&1; local m=$(date +%s.%N); find "$d/django" -name '*.py' -exec cat {} + > /dev/null; local e=$(date +%s.%N); echo "compile $(echo "$m - $s" | bc | cut -c1-5) s, read $(echo "$e - $m" | bc | cut -c1-5) s"; }
echo "  native worktree: $(work "$T/dj-native")"
V=$(echo "{\"cwd\":\"$PWD\",\"name\":\"bench\"}" | $G hook worktree-create | tail -1); echo "  view ($(cat .gitta/cow/bench/backend)): $(work "$V")"
echo "{\"cwd\":\"$PWD\",\"worktree_path\":\"$V\"}" | $G hook worktree-remove
$G uninstall >/dev/null 2>&1 || true; git worktree remove --force "$T/dj-native" || true

echo "== Cursor/Codex-style worktrees (converted by the setup step), mode $MODE"
cd "$T" && rm -rf cur && git init -q -b main cur && cd cur
mkdir shop; printf 'def price(x):\n    return x * 2\n' > shop/p.py; printf 'node_modules/\n' > .gitignore
git add -A && git -c user.name=t -c user.email=t@t commit -qm base
$G init --no-claude >/dev/null
grep -q "hook worktree-setup" .cursor/worktrees.json && echo "  ok: Cursor setup step installed"
W=$T/cursor-wt/c1; mkdir -p "$T/cursor-wt"; git worktree add -q -b c1 "$W"
mkdir -p "$W/node_modules/dep" && echo installed > "$W/node_modules/dep/x.js" && echo notes > "$W/notes.txt"
(cd "$W" && $G hook worktree-setup)
grep -q " $W " /proc/mounts && echo "  ok: converted to a copy-on-write view" || echo "  FAIL: not converted"
grep -q "x \* 2" "$W/shop/p.py" && echo "  ok: files visible"
[ -f "$W/node_modules/dep/x.js" ] && [ -f "$W/notes.txt" ] && echo "  ok: files the setup made are kept"
[ "$(git -C "$W" status --porcelain)" = "?? notes.txt" ] && echo "  ok: git status shows only the untracked file"
W2=$T/cursor-wt/c2; git worktree add -q -b c2 "$W2"; printf 'def price(x):\n    return x\n' > "$W2/shop/p.py"
(cd "$W2" && $G hook worktree-setup)
grep -q " $W2 " /proc/mounts && echo "  FAIL: converted a worktree with edits" || echo "  ok: a worktree with edits is left alone"
printf 'def price(x):\n    return x * 9\n' > "$W/shop/p.py"; grep -q "x \* 2" .gitta/shared/*/shop/p.py && echo "  ok: shared checkout untouched by edits"
$G status | sed 's/^/  /'
$G uninstall >/dev/null; grep -q " $W " /proc/mounts && echo "  FAIL: still mounted" || true
grep -q "x \* 9" "$W/shop/p.py" && [ -f "$W/node_modules/dep/x.js" ] && echo "  ok: uninstall kept the work and the setup's files"
[ -f .cursor/worktrees.json ] && echo "  FAIL: Cursor setup left behind" || echo "  ok: Cursor setup step removed"
echo "== Cursor-style on VS Code, mode $MODE"
cd "$T" && rm -rf bigc && git clone -q --depth 1 https://github.com/microsoft/vscode.git bigc && cd bigc && $G init --no-claude >/dev/null
before=$(df -k --output=used . | tail -1)
for n in 1 2 3; do git worktree add -q -b c$n "$T/bigc-wt/c$n"; done
mid=$(df -k --output=used . | tail -1)
s=$(date +%s.%N); for n in 1 2 3; do (cd "$T/bigc-wt/c$n" && $G hook worktree-setup); done; e=$(date +%s.%N)
after=$(df -k --output=used . | tail -1)
echo "  3 ordinary worktrees used $(( (mid-before)/1024 )) MB; after conversion $(( (after-before)/1024 )) MB (includes one shared checkout); conversion took $(echo "$e - $s" | bc) s"
echo "  git status in a converted worktree: $( { /usr/bin/time -f %e git -C "$T/bigc-wt/c1" status --porcelain >/dev/null; } 2>&1 ) s"
$G uninstall >/dev/null 2>&1 || true
for n in 1 2 3; do grep -q " $T/bigc-wt/c$n " /proc/mounts && { fusermount3 -u "$T/bigc-wt/c$n" 2>/dev/null || umount "$T/bigc-wt/c$n" 2>/dev/null; } ; done
cd / && rm -rf "${T:?}"; echo "== done"

