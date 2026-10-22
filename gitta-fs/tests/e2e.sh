#!/bin/bash
# End-to-end test for gitta-fs: mounts a real FUSE view and checks copy-on-write behaviour.
# Needs Linux with /dev/fuse and fusermount3. Run: tests/e2e.sh [path/to/gitta-fs]
# Runs the suite twice: with FUSE passthrough (default) and with GITTA_NO_PASSTHROUGH=1.
set -u
BIN=${1:-gitta-fs}
FAIL=0; PASS=0
ok()   { PASS=$((PASS+1)); echo "ok   $MODE: $1"; }
bad()  { FAIL=$((FAIL+1)); echo "FAIL $MODE: $1"; }
check() { local d=$1; shift; if "$@" >/dev/null 2>&1; then ok "$d"; else bad "$d"; fi; }
nocheck() { local d=$1; shift; if "$@" >/dev/null 2>&1; then bad "$d"; else ok "$d"; fi; }

mount_fs() {
  "$BIN" --lower $L --upper $U --mount $M --events $E --agent a1 \
    --deny '.env*' --deny 'locked/**' --deny-read 'secrets/**' 2>$W/fs.log &
  PID=$!
  for _ in $(seq 100); do mountpoint -q $M && return 0; sleep 0.05; done
  echo "mount failed"; cat $W/fs.log; exit 1
}
umount_fs() { cd /; fusermount3 -u $M 2>/dev/null || umount $M; wait $PID 2>/dev/null; }

run_suite() {
  W=$(mktemp -d); L=$W/lower U=$W/upper M=$W/mnt E=$W/events.jsonl
  mkdir -p $L/src/deep $L/olddir $L/gone $L/locked $L/secrets $M
  echo base > $L/a.txt; echo keep > $L/b.txt; echo del > $L/c.txt
  echo deep > $L/src/deep/x.txt; echo o > $L/olddir/f; echo g > $L/gone/f
  echo L > $L/locked/l; echo topsecret > $L/secrets/key; echo X=1 > $L/.env
  printf 'abcdefghij' > $L/mm.bin; ln -s a.txt $L/link
  chmod -R a-w $L    # lower must never be written; prove it by making it read-only
  ( cd $L && find . -exec stat -c '%n %s %Y %a' {} + | sort ) > $W/lower.before
  mount_fs; cd $M

  check "read through lower" grep -qx base a.txt
  check "nested read" grep -qx deep src/deep/x.txt
  check "symlink from lower" test "$(readlink link)" = a.txt
  check "listing shows lower files" test "$(ls | tr '\n' ' ')" = "a.txt b.txt c.txt gone link locked mm.bin olddir secrets src "
  echo edit >> a.txt
  check "append copies up" test "$(cat a.txt)" = "$(printf 'base\nedit')"
  check "upper has the copy" grep -q edit $U/a.txt
  echo n > src/deep/x.txt
  check "nested copy-up" test "$(cat $U/src/deep/x.txt)" = n
  echo new > new.txt; check "create" grep -qx new $U/new.txt
  mkdir -p d1/d2 && echo z > d1/d2/z; check "mkdir -p + file" test -f $U/d1/d2/z
  truncate -s 3 b.txt; check "truncate" test "$(cat b.txt)" = kee
  chmod 600 b.txt; check "chmod" test "$(stat -c %a b.txt)" = 600
  rm c.txt; nocheck "unlink hides lower" test -e c.txt
  check "whiteout recorded" grep -qx c.txt $U/.gitta-whiteouts
  nocheck "whiteout file hidden" test -e .gitta-whiteouts
  echo again > c.txt; check "recreate after unlink" test "$(cat c.txt)" = again
  rm -r gone; nocheck "rmdir lower dir" test -e gone
  mkdir gone; check "recreated dir is opaque" test -z "$(ls gone)"
  mv olddir newdir; nocheck "rename dir: old gone" test -e olddir
  check "rename dir: content moved" grep -qx o newdir/f
  mv new.txt renamed.txt; check "rename upper file" test -f renamed.txt -a ! -e new.txt
  mv a.txt a2.txt; check "rename lower file" test -f a2.txt -a ! -e a.txt
  ln -s a2.txt l2; check "new symlink" test "$(cat l2)" = "$(cat a2.txt)"
  python3 - <<'PY' && ok "mmap write" || bad "mmap write"
import mmap
with open("mm.bin","r+b") as f:
    m=mmap.mmap(f.fileno(),0); m[0:3]=b"XYZ"; m.flush(); m.close()
assert open("mm.bin","rb").read()==b"XYZdefghij"
PY
  check "mmap reached upper" test "$(cat $U/mm.bin)" = XYZdefghij
  # write policy
  nocheck "deny: write .env refused" sh -c 'echo Y=2 > .env'
  check "deny: .env still readable" grep -qx X=1 .env
  nocheck "deny: create in locked/ refused" sh -c 'echo q > locked/new'
  nocheck "deny: unlink in locked/ refused" rm -f locked/l
  check "deny: errno is EACCES" sh -c 'echo q 2>&1 > locked/new | grep -q "Permission denied"'
  nocheck "deny-read: read refused" cat secrets/key
  nocheck "deny-read: write refused" sh -c 'echo x > secrets/key'
  check "BLOCKED events logged" test "$(grep -c '"op":"BLOCKED"' $E)" -ge 4
  check "events are valid JSON" python3 -c "import json,sys;[json.loads(l) for l in open('$E')]"
  check "write event for a2/a.txt" grep -q '"op":"write"' $E
  check "events carry agent" sh -c "! grep -v '\"agent\":\"a1\"' $E"
  # git inside the mount
  check "git init+commit in view" sh -c 'git init -q g && cd g && echo 1 > f && git add f && git -c user.email=t@t -c user.name=t commit -qm m && git log --oneline | grep -q m'
  # concurrent writers
  PIDS=""; for i in $(seq 20); do (for j in $(seq 50); do echo $i-$j >> par$i.txt; done) & PIDS="$PIDS $!"; done; wait $PIDS
  check "parallel appends" test "$(cat par*.txt | wc -l)" = 1000
  sync
  umount_fs
  ( cd $L && find . -exec stat -c '%n %s %Y %a' {} + | sort ) > $W/lower.after
  check "lower untouched" cmp $W/lower.before $W/lower.after
  # remount: state survives
  mount_fs; cd $M
  check "remount: edit kept" grep -q edit a2.txt
  check "remount: whiteout kept" test ! -e a.txt -a ! -e olddir
  check "remount: opaque dir kept" test -z "$(ls gone)"
  check "remount: recreated file kept" grep -qx again c.txt
  umount_fs
  chmod -R u+w $L; rm -rf $W
}

MODE=passthrough; unset GITTA_NO_PASSTHROUGH; run_suite
MODE=userspace; export GITTA_NO_PASSTHROUGH=1; run_suite
echo "passed $PASS, failed $FAIL"
[ $FAIL = 0 ]
