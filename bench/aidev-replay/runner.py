#!/usr/bin/env python3
"""AIDev pair replay runner (runs on the benchmark VM, never on a workstation).

  python3 runner.py run pairs.jsonl results.jsonl      # resumable: skips pairs already in results.jsonl
  python3 runner.py gate <dir>                         # used as `gitta pr-check --gate`; tests <dir> in Docker

Per pair: fetch refs/pull/N/head of both PRs, merge-base, `git merge-tree --write-tree` (textual conflict),
`gitta pr-check --json` without a gate (live warnings). For textually clean pairs: run the project's own tests in
Docker on PR A, PR B and the merge (deps installed with network, tests run with --network none). If A and B pass and
the merge fails, the merge (and base) is rerun once; only consistent failures count as a break. Then
`gitta pr-check --gate` on the same pair. Repo code only ever runs inside the containers.
"""
import json, os, subprocess, sys, time, uuid, shutil, gzip, threading, concurrent.futures as cf

HERE = os.path.dirname(os.path.abspath(__file__))
IMAGES = json.load(open(os.path.join(HERE, "images.json")))  # name -> image@sha256 digest
WORK = os.environ.get("BENCH_WORK", "/data/work")
LOGS = os.environ.get("BENCH_LOGS", "/data/results/logs")
GITTA = os.environ.get("GITTA", "/usr/local/bin/gitta")
WORKERS = int(os.environ.get("BENCH_WORKERS", "3"))
CPUS, MEM = "2", "5g"
T_FETCH, T_INSTALL, T_TEST, T_GITTA = 900, 900, 900, 600
TAIL = 150
lock = threading.Lock()
os.environ["GIT_TERMINAL_PROMPT"] = "0"


def sh(cmd, cwd=None, timeout=None, env=None):
    t = time.time()
    try:
        p = subprocess.run(cmd, cwd=cwd, timeout=timeout, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        return p.returncode, p.stdout.decode("utf-8", "replace"), time.time() - t
    except subprocess.TimeoutExpired as e:
        return 124, (e.stdout or b"").decode("utf-8", "replace") + "\n[timeout]", time.time() - t


def git(repo, *a, timeout=600):
    return sh(["git", "-C", repo, *a], timeout=timeout)


def tail(s, n=TAIL):
    return "\n".join(s.splitlines()[-n:])


# ---------------------------------------------------------------- tests in Docker
def detect(d):
    eco = []
    pj = os.path.join(d, "package.json")
    if os.path.exists(pj):
        try:
            t = (json.load(open(pj)).get("scripts") or {}).get("test", "")
        except Exception:
            t = ""
        has_lock = {f for f in ("bun.lockb", "bun.lock", "pnpm-lock.yaml", "yarn.lock", "package-lock.json") if os.path.exists(os.path.join(d, f))}
        if t and "no test specified" not in t:
            eco.append(("node", "bun" if has_lock & {"bun.lockb", "bun.lock"} else "pnpm" if "pnpm-lock.yaml" in has_lock else "yarn" if "yarn.lock" in has_lock else "npm" if "package-lock.json" in has_lock else "npm-nolock"))
    if any(os.path.exists(os.path.join(d, f)) for f in ("pyproject.toml", "setup.py", "requirements.txt")):
        found = os.path.isdir(os.path.join(d, "tests")) or os.path.isdir(os.path.join(d, "test"))
        if not found:
            for root, dirs, files in os.walk(d):
                dirs[:] = [x for x in dirs if not x.startswith(".") and x not in ("node_modules", "venv", ".venv")]
                if root.count(os.sep) - d.count(os.sep) > 3: dirs[:] = []
                if any(f.startswith("test_") and f.endswith(".py") or f.endswith("_test.py") for f in files): found = True; break
        if found: eco.append(("python", "pip"))
    if os.path.exists(os.path.join(d, "go.mod")):
        eco.append(("go", "go"))
    return eco


GO_ENV = "export GOMODCACHE=/w/.bench/gomod GOCACHE=/w/.bench/gocache GOFLAGS=-mod=mod GOTOOLCHAIN=auto GOPATH=/w/.bench/gopath;"
# yarn and pnpm through corepack (the image's own yarn 1 would ignore the project's packageManager field)
NODE_ENV = ("export CI=true COREPACK_HOME=/w/.bench/corepack COREPACK_ENABLE_DOWNLOAD_PROMPT=0 COREPACK_ENABLE_STRICT=0 npm_config_cache=/w/.bench/npm "
            "PATH=/w/.bench/bin:$PATH; mkdir -p /w/.bench/bin; for t in yarn pnpm; do printf '#!/bin/sh\\nexec corepack %s \"$@\"\\n' $t > /w/.bench/bin/$t; "
            "chmod +x /w/.bench/bin/$t; done;")
STEPS = {
    "python": ("python",
               "python -m venv /w/.bench/venv && . /w/.bench/venv/bin/activate && pip install -q -U pip && "
               "for f in *requirements*.txt requirements/*.txt; do [ -f \"$f\" ] && pip install -q -r \"$f\" || true; done; "
               "if [ -f pyproject.toml ] || [ -f setup.py ]; then pip install -q -e . || pip install -q . || exit 3; "
               "for e in test tests testing dev; do pip install -q -e \".[$e]\" 2>/dev/null || true; done; "
               "for g in test tests dev; do pip install -q --group \"$g\" 2>/dev/null || true; done; fi; "
               "pip install -q setuptools pytest pytest-mock pytest-asyncio",
               ". /w/.bench/venv/bin/activate && python -m pytest -q -p no:cacheprovider -o addopts='' --timeout=300 2>/dev/null; r=$?; "
               "[ $r -eq 4 ] && { python -m pytest -q -p no:cacheprovider -o addopts=''; r=$?; }; exit $r"),
    "node": ("node", None, None),
    "go": ("go", GO_ENV + "go mod download", GO_ENV + "export GOPROXY=off; go build ./... && go test ./..."),
}
NODE_INSTALL = {"npm": "npm ci --no-audit --no-fund", "npm-nolock": "npm install --no-audit --no-fund",
                "pnpm": "pnpm install --frozen-lockfile",
                "yarn": "yarn install --immutable || yarn install --frozen-lockfile"}
NODE_TEST = {"npm": "npm test", "npm-nolock": "npm test", "pnpm": "pnpm test", "yarn": "yarn test"}
TSC = "if [ -f tsconfig.json ] && [ -x node_modules/.bin/tsc ]; then node_modules/.bin/tsc --noEmit -p . || exit 7; fi"


def docker(d, image, script, network, timeout):
    name = "bench-" + uuid.uuid4().hex[:12]
    cmd = ["docker", "run", "--rm", "--name", name, "--cpus", CPUS, "--memory", MEM, "--memory-swap", MEM, "--pids-limit", "4096",
           "--security-opt", "no-new-privileges", "-v", f"{d}:/w", "-w", "/w", "-e", "HOME=/w/.bench/home"]
    if not network: cmd += ["--network", "none"]
    cmd += [IMAGES[image], "bash", "-c", "mkdir -p /w/.bench/home; " + script]
    rc, out, dt = sh(cmd, timeout=timeout)
    if rc == 124: sh(["docker", "kill", name], timeout=60)
    return rc, out, dt


def test_dir(d):
    """Install deps (network on) and run tests (network off) for every detected ecosystem. Returns a dict."""
    eco = detect(d)
    res = {"eco": [f"{a}:{b}" for a, b in eco], "steps": [], "ok": None, "status": None}
    if not eco: res["status"] = "no_tests"; return res, ""
    full = []
    ok = True
    for kind, pm in eco:
        if kind == "node":
            if pm == "bun": res["steps"].append({"eco": "node:bun", "status": "unsupported"}); res["status"] = "unsupported_bun"; return res, ""
            image, inst, test = "node", NODE_ENV + NODE_INSTALL[pm], NODE_ENV + NODE_TEST[pm] + " && " + TSC
        else:
            image, inst, test = STEPS[kind]
        if kind == "python":  # pytest-timeout keeps one hung test from eating the budget
            inst += " pytest-timeout"
        rc, out, dt = docker(d, image, inst, True, T_INSTALL)
        st = {"eco": f"{kind}:{pm}", "install_rc": rc, "install_s": round(dt, 1)}
        full.append(f"### {kind} install rc={rc}\n{out}")
        if rc != 0:
            st["status"] = "install_timeout" if rc == 124 else "install_failed"; res["steps"].append(st); ok = False; break
        rc, out, dt = docker(d, image, test, False, T_TEST)
        full.append(f"### {kind} test rc={rc}\n{out}")
        st.update(test_rc=rc, test_s=round(dt, 1), test_tail=tail(out, 40))
        if kind == "python" and rc == 5: st["status"] = "no_tests_collected"; res["steps"].append(st); res["status"] = "no_tests"; return res, "\n".join(full)
        st["status"] = "pass" if rc == 0 else "timeout" if rc == 124 else "fail"
        res["steps"].append(st)
        if rc != 0: ok = False
    res["ok"] = ok
    res["status"] = "pass" if ok else next(s["status"] for s in res["steps"] if s["status"] != "pass")
    return res, "\n".join(full)


def clean(d):
    # files written by containers are owned by root; remove them from a container
    if os.path.exists(d):
        sh(["docker", "run", "--rm", "-v", f"{os.path.dirname(d)}:/p", IMAGES["python"], "rm", "-rf", "/p/" + os.path.basename(d)], timeout=600)
        shutil.rmtree(d, ignore_errors=True)


# ---------------------------------------------------------------- one pair
def run_pair(pr):
    idx = pr["idx"]; t0 = time.time()
    r = dict(pr); r["timings"] = {}; r["stage"] = "start"
    base = os.path.join(WORK, str(idx)); clean(base); os.makedirs(base)
    repo = os.path.join(base, "repo"); logs = []
    def T(k, t): r["timings"][k] = round(time.time() - t, 1)
    try:
        t = time.time()
        sh(["git", "init", "-q", repo]); git(repo, "remote", "add", "origin", pr["repo"])
        git(repo, "config", "user.email", "bench@example.invalid"); git(repo, "config", "user.name", "bench")
        rc, out, _ = git(repo, "fetch", "-q", "--filter=blob:none", "origin", f"+refs/pull/{pr['pr_a']}/head:refs/heads/pra",
                         f"+refs/pull/{pr['pr_b']}/head:refs/heads/prb", timeout=T_FETCH)
        T("fetch", t)
        if rc != 0:
            lo = out.lower()
            r["excluded"] = "repo_unavailable" if ("not found" in lo and "repository" in lo) or "could not read username" in lo or "access denied" in lo or "dmca" in lo else \
                            "pr_ref_missing" if "couldn't find remote ref" in lo else "fetch_failed"
            r["fetch_error"] = tail(out, 10); return r
        r["head_a"] = git(repo, "rev-parse", "pra")[1].strip(); r["head_b"] = git(repo, "rev-parse", "prb")[1].strip()
        rc, out, _ = git(repo, "merge-base", "pra", "prb")
        if rc != 0: r["excluded"] = "no_merge_base"; return r
        r["base"] = out.strip()
        t = time.time()
        rc, out, _ = git(repo, "merge-tree", "--write-tree", "--name-only", "pra", "prb", timeout=1200)
        T("merge_tree", t)
        if rc not in (0, 1): r["excluded"] = "merge_tree_error"; r["merge_tree_error"] = tail(out, 10); return r
        lines = out.splitlines()
        r["conflict"] = rc == 1; r["merge_tree"] = lines[0] if lines else None
        if rc == 1:
            files = []
            for l in lines[1:]:
                if not l.strip(): break
                files.append(l)
            r["conflict_files"] = files
        r["stage"] = "textual"
        # Gitta, live warnings only (no gate). Needs a checkout as its root.
        git(repo, "-c", "advice.detachedHead=false", "checkout", "-q", "--detach", r["base"], timeout=1200)
        t = time.time()
        rc, out, _ = sh([GITTA, "pr-check", "--pr", "pra", "--name", f"#{pr['pr_a']}", "--other", f"prb=#{pr['pr_b']}", "--base", r["base"], "--json"], cwd=repo, timeout=T_GITTA)
        T("gitta_live", t)
        r["gitta_live_rc"] = rc
        try: r["gitta_live"] = json.loads(out[out.index("["):])[0]
        except Exception: r["gitta_live"] = None; r["gitta_live_raw"] = tail(out, 40)
        logs.append(f"## gitta pr-check (live) rc={rc}\n{out}")
        if r["conflict"]: return r
        # ---------------- tests
        r["stage"] = "tests"
        git(repo, "commit-tree", "-h")  # no-op, keeps git warm
        merge_commit = git(repo, "commit-tree", r["merge_tree"], "-p", "pra", "-p", "prb", "-m", "bench merge")[1].strip()
        r["merge_commit"] = merge_commit
        def variant(label, commit):
            d = os.path.join(base, label)
            rc, out, _ = git(repo, "worktree", "add", "-q", "--detach", d, commit, timeout=1800)
            if rc != 0: return {"status": "checkout_failed", "ok": None, "err": tail(out, 10)}, out
            tt = time.time(); res, full = test_dir(d); res["s"] = round(time.time() - tt, 1)
            clean(d); git(repo, "worktree", "prune")
            return res, full
        t = time.time()
        with cf.ThreadPoolExecutor(2) as ex:
            fa, fb = ex.submit(variant, "a", "pra"), ex.submit(variant, "b", "prb")
            (ra, la), (rb, lb) = fa.result(), fb.result()
        r["test_a"], r["test_b"] = ra, rb; logs += [f"## tests PR A\n{la}", f"## tests PR B\n{lb}"]
        T("tests_sides", t)
        if ra["status"] in ("no_tests", "unsupported_bun") and rb["status"] in ("no_tests", "unsupported_bun"):
            r["excluded"] = "no_runnable_tests" if ra["status"] == "no_tests" else "unsupported_bun"; return r
        if not (ra["ok"] and rb["ok"]):
            r["excluded"] = "side_fails"; return r
        t = time.time()
        rm, lm = variant("m", merge_commit); r["test_merge"] = rm; logs.append(f"## tests merge\n{lm}")
        if not rm["ok"]:
            with cf.ThreadPoolExecutor(2) as ex:
                f1, f2 = ex.submit(variant, "m2", merge_commit), ex.submit(variant, "base", r["base"])
                (rm2, lm2), (rbase, lbase) = f1.result(), f2.result()
            r["test_merge_rerun"], r["test_base"] = rm2, rbase; logs += [f"## tests merge (rerun)\n{lm2}", f"## tests base\n{lbase}"]
            r["outcome"] = "break" if not rm2["ok"] else "flaky"
        else:
            r["outcome"] = "fine"
        T("tests_merge", t)
        r["stage"] = "evaluated"
        # Gitta with the gate: tests (same Docker runner) on the combination on top of the base.
        t = time.time()
        rc, out, _ = sh([GITTA, "pr-check", "--pr", "pra", "--name", f"#{pr['pr_a']}", "--other", f"prb=#{pr['pr_b']}", "--base", r["base"],
                         "--gate", f"python3 {os.path.abspath(__file__)} gate \"$PWD\"", "--json"], cwd=repo, timeout=T_INSTALL + T_TEST + 600)
        T("gitta_gate", t)
        try: r["gitta_gate"] = json.loads(out[out.index("["):])[0]
        except Exception: r["gitta_gate"] = None; r["gitta_gate_raw"] = tail(out, 40)
        logs.append(f"## gitta pr-check (gate) rc={rc}\n{out}")
        return r
    except Exception as e:
        r["excluded"] = "runner_error"; r["error"] = repr(e); return r
    finally:
        r["timings"]["total"] = round(time.time() - t0, 1)
        os.makedirs(LOGS, exist_ok=True)
        with gzip.open(os.path.join(LOGS, f"{idx:04d}.log.gz"), "wt") as f: f.write("\n\n".join(logs))
        git(repo, "worktree", "prune"); clean(base)


def main():
    if sys.argv[1] == "gate":
        d = sys.argv[2]; res, full = test_dir(d); print(full[-20000:]); print(json.dumps({k: v for k, v in res.items() if k != "steps"}))
        clean_bench = os.path.join(d, ".bench")
        sh(["docker", "run", "--rm", "-v", f"{d}:/w", IMAGES["python"], "rm", "-rf", "/w/.bench", "/w/node_modules"], timeout=600)
        sys.exit(0 if res["ok"] else 1)
    pairs = [json.loads(l) for l in open(sys.argv[2])]
    out = sys.argv[3]
    done = set()
    if os.path.exists(out):
        done = {json.loads(l)["idx"] for l in open(out) if l.strip()}
    todo = [p for p in pairs if p["idx"] not in done]
    print(f"{len(done)} done, {len(todo)} to go", flush=True)
    with cf.ThreadPoolExecutor(WORKERS) as ex:
        for r in ex.map(run_pair, todo):
            with lock, open(out, "a") as f: f.write(json.dumps(r) + "\n")
            print(time.strftime("%H:%M:%S"), r["idx"], r.get("excluded") or r.get("outcome") or ("conflict" if r.get("conflict") else r["stage"]), r["timings"].get("total"), flush=True)


if __name__ == "__main__":
    main()
