#!/usr/bin/env python3
"""Metrics, graphs and metrics.json from results-<run>.jsonl (runs on the workstation; reads results only, runs no repo code).
  python3 analyze.py [bench dir]      needs matplotlib; DM Sans is used when installed or at $DMSANS (path to the .ttf)"""
import json, os, sys, math, collections, datetime as dt

HERE = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else os.path.dirname(__file__))
PRIMARY = os.environ.get("PRIMARY", "gcp8")
RUN_ORDER = ["gcp8", "aws8", "azure8", "gcp4", "aws4", "azure4"]
# On-demand Linux prices, USD, looked up 2026-10-20 (sources in REPORT.md). (type, instance $/h, 200 GB disk $/h = monthly / 730)
PRICE = {
    "gcp8": ("e2-standard-8", 8 * 0.02401614 + 32 * 0.00321816, 200 * 0.187 / 730),
    "gcp4": ("e2-standard-4", 4 * 0.02401614 + 16 * 0.00321816, 200 * 0.187 / 730),
    "aws8": ("m6i.2xlarge", 0.408, 200 * 0.0836 / 730),
    "aws4": ("m6i.xlarge", 0.204, 200 * 0.0836 / 730),
    "azure8": ("Standard_D8s_v5", 0.408, 41.811428 / 730),
    "azure4": ("Standard_D4s_v5", 0.204, 41.811428 / 730),
}
CLOUD = {"gcp": "Google Cloud", "aws": "AWS", "azure": "Azure"}
FETCH_EXCL = ["repo_unavailable", "pr_ref_missing", "fetch_failed", "no_merge_base", "merge_tree_error", "runner_error"]
TEST_EXCL = ["no_runnable_tests", "unsupported_bun", "side_fails"]


def wilson(k, n, z=1.96):
    if n == 0: return (float("nan"),) * 3
    p = k / n; d = 1 + z * z / n; c = (p + z * z / (2 * n)) / d; h = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return p, max(0, c - h), min(1, c + h)


def pct(k, n):
    p, lo, hi = wilson(k, n)
    return f"{100*p:.1f}% ({k}/{n}; 95% CI {100*lo:.1f}–{100*hi:.1f}%)" if n else "n/a (0/0)"


def load(run):
    f = os.path.join(HERE, f"results-{run}.jsonl")
    return {r["idx"]: r for r in map(json.loads, open(f))} if os.path.exists(f) else None


def live_flag(r):
    g = r.get("gitta_live")
    return bool(g) and (not g.get("merges", True) or bool(g.get("warnings")))


def gate_flag(r):
    g = r.get("gitta_gate")
    return bool(g) and (not g.get("merges", True) or g.get("tests") == "FAIL")


def classify(r):
    if r.get("excluded") in FETCH_EXCL: return r["excluded"]
    if r.get("conflict"): return "conflict"
    if r.get("excluded"): return r["excluded"]
    return r.get("outcome") or "incomplete"


def metrics(R):
    m = {}
    c = collections.Counter(classify(r) for r in R.values())
    m["counts"] = dict(c); m["n"] = len(R)
    tex = [r for r in R.values() if classify(r) not in FETCH_EXCL]
    m["textual"] = len(tex)
    for st in ("intra", "cross"):
        s = [r for r in tex if r["stratum"] == st]
        m[f"conf_{st}"] = (sum(r["conflict"] for r in s), len(s))
    m["conf_all"] = (sum(r["conflict"] for r in tex), len(tex))
    ev = [r for r in R.values() if r.get("outcome") in ("fine", "flaky", "break")]
    br = [r for r in ev if r["outcome"] == "break"]; fine = [r for r in ev if r["outcome"] != "break"]
    m["evaluated"], m["breaks"], m["fine"], m["flaky"] = len(ev), len(br), len(fine), sum(r["outcome"] == "flaky" for r in ev)
    m["live_recall"] = (sum(map(live_flag, br)), len(br)); m["live_fa"] = (sum(map(live_flag, fine)), len(fine))
    gb = [r for r in br if r.get("gitta_gate")]; gf = [r for r in fine if r.get("gitta_gate")]
    m["gate_recall"] = (sum(map(gate_flag, gb)), len(gb)); m["gate_fa"] = (sum(map(gate_flag, gf)), len(gf))
    cf = [r for r in tex if r["conflict"] and r.get("gitta_live")]
    m["gitta_conflict_agree"] = (sum(not r["gitta_live"]["merges"] for r in cf), len(cf))
    cl = [r for r in tex if not r["conflict"] and r.get("gitta_live")]
    m["gitta_clean_says_nomerge"] = (sum(not r["gitta_live"]["merges"] for r in cl), len(cl))
    m["gitta_clean_warns"] = (sum(bool(r["gitta_live"].get("warnings")) for r in cl), len(cl))
    m["gitta_timeouts"] = sorted(r["idx"] for r in tex if r.get("gitta_live_rc") == 124)
    m["gitta_timeouts_evaluated"] = sorted(r["idx"] for r in ev if not r.get("gitta_live"))
    NONCODE = (".md", ".txt", ".rst", ".yml", ".yaml", ".json", ".lock", ".toml", ".cfg", ".ini")
    def noncode(r):
        ws = r["gitta_live"]["warnings"]
        return bool(ws) and all(w.split(":", 1)[1].strip().split()[0].lower().endswith(NONCODE) or "CHANGELOG" in w for w in ws if ":" in w)
    m["live_fa_noncode_only"] = sum(1 for r in fine if live_flag(r) and noncode(r))
    m["eco"] = dict(collections.Counter("+".join((r.get("test_a") or {}).get("eco") or ["none"]) for r in ev))
    m["break_ids"] = sorted(r["idx"] for r in br); m["fa_ids"] = sorted(r["idx"] for r in fine if live_flag(r))
    tk = collections.defaultdict(list)
    for r in R.values():
        for k, v in r.get("timings", {}).items(): tk[k].append(v)
    m["timing_sum_h"] = {k: round(sum(v) / 3600, 2) for k, v in tk.items()}
    return m


def run_meta(run):
    d = os.path.join(HERE, "results", run); meta = {"run": run}
    def rd(f):
        p = os.path.join(d, f); return open(p).read().strip() if os.path.exists(p) else None
    for k, f in (("machine", "machine.json"), ("env", "env.json")):
        try: meta[k] = json.loads(rd(f))
        except Exception: meta[k] = {}
    tl = [l.split(" ", 2) for l in (rd("timeline.log") or "").splitlines() if l.count(" ") >= 2]
    ts = lambda s: dt.datetime.strptime(s, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=dt.timezone.utc).timestamp()
    apply_t = next((ts(t) for t, _, x in tl if x.startswith("terraform apply")), None)
    setup_t = [ts(t) for t, _, x in tl if x.startswith("setup")]
    down_t = next((ts(t) for t, _, x in tl if x.startswith("destroyed")), None)  # first destroy after apply
    s, e = rd("t_start"), rd("t_end")
    meta["runner_h"] = (int(e) - int(s)) / 3600 if s and e else None
    meta["setup_min"] = (setup_t[1] - setup_t[0]) / 60 if len(setup_t) >= 2 else None
    meta["billed_h"] = (down_t - apply_t) / 3600 if apply_t and down_t else None
    name, ph, dh = PRICE[run]; meta["type"], meta["price_h"], meta["disk_h"] = name, ph, dh
    meta["cost"] = meta["billed_h"] * (ph + dh) if meta["billed_h"] else None
    meta["verify"] = rd("verify.txt")
    return meta


INK, MUTED, BG, RED, GREEN, GOLD = "#242B28", "#626B64", "#F7F7F2", "#E8492D", "#4E8A57", "#E7AB4A"


def setup_mpl():
    import matplotlib; matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    from matplotlib import font_manager
    f = os.environ.get("DMSANS", os.path.join(HERE, "fonts", "DMSans.ttf"))
    if f and os.path.exists(f): font_manager.fontManager.addfont(f)
    plt.rcParams.update({"font.family": "DM Sans", "font.size": 12, "text.color": INK, "axes.labelcolor": MUTED,
                         "axes.edgecolor": MUTED, "xtick.color": MUTED, "ytick.color": MUTED, "figure.facecolor": BG,
                         "axes.facecolor": BG, "savefig.facecolor": BG, "svg.fonttype": "none", "axes.spines.top": False,
                         "axes.spines.right": False, "svg.hashsalt": "gitta"})
    return plt


def save(plt, fig, name):
    g = os.path.join(HERE, "graphs"); os.makedirs(g, exist_ok=True)
    fig.savefig(os.path.join(g, name + ".svg"), bbox_inches="tight", metadata={"Date": None})
    fig.savefig(os.path.join(g, name + ".png"), dpi=200, bbox_inches="tight")
    plt.close(fig)


def bare(ax, x=True):
    ax.spines["left"].set_visible(False); ax.tick_params(left=False); ax.set_yticks([])
    if x: ax.spines["bottom"].set_visible(False); ax.tick_params(bottom=False); ax.set_xticks([])


def hbars(plt, name, rows, xmax, width=10, colors=None):
    """rows: (label, value, text, color)"""
    fig, ax = plt.subplots(figsize=(width, 0.62 * len(rows) + 0.6))
    for i, (lab, v, txt, col) in enumerate(rows):
        y = len(rows) - 1 - i
        ax.barh(y, v, color=col, height=0.6)
        ax.text(-xmax * 0.012, y, lab, ha="right", va="center")
        ax.text(v + xmax * 0.012, y, txt, va="center", color=INK)
    ax.set_xlim(0, xmax); bare(ax); save(plt, fig, name)


def graphs(m, metas):
    plt = setup_mpl(); c = m["counts"]
    fx = ", ".join(f"{c[k]} {k.replace('_', ' ')}" for k in FETCH_EXCL if c.get(k))
    tx = ", ".join(f"{c[k]} {k.replace('_', ' ').replace('side fails', 'a PR fails its own tests')}" for k in TEST_EXCL if c.get(k))
    clean = m["textual"] - c.get("conflict", 0)
    hbars(plt, "funnel", [
        ("Pairs sampled, one per repository", m["n"], f"{m['n']}", INK),
        ("Replayed with git merge-tree", m["textual"], f"{m['textual']}    out: {fx}", INK),
        ("Git merges them cleanly", clean, f"{clean}    out: {c.get('conflict', 0)} textual conflicts", INK),
        ("Tests run on both PRs and the merge", m["evaluated"], f"{m['evaluated']}    out: {tx}", INK),
        ("Clean merge, merged code breaks", m["breaks"], f"{m['breaks']}", RED)], m["n"] * 1.9)
    # outcome split
    fig, ax = plt.subplots(figsize=(10, 1.4))
    ok, br = m["fine"], m["breaks"]
    ax.barh(0, ok, color=GREEN, height=0.6); ax.barh(0, br, left=ok, color=RED, height=0.6)
    ax.text(ok / 2, 0, f"Merge passes the tests: {ok}", ha="center", va="center", color=BG)
    ax.text(ok + br + m["evaluated"] * 0.01, 0, f"Merge fails the tests: {br} of {m['evaluated']}", va="center", color=RED)
    ax.set_xlim(0, m["evaluated"] * 1.5); bare(ax); save(plt, fig, "outcomes")
    # flagged vs missed
    rows = []
    for lab, (k, n) in (("gitta live warnings, no gate", m["live_recall"]), ("gitta with the test gate", m["gate_recall"])):
        if n: rows.append((lab, k, n))
    if rows:
        fig, ax = plt.subplots(figsize=(9, 0.7 * len(rows) + 0.6)); N = max(n for _, _, n in rows)
        for i, (lab, k, n) in enumerate(rows):
            y = len(rows) - 1 - i
            ax.barh(y, k, color=GREEN, height=0.6); ax.barh(y, n - k, left=k, color="#D9DBD3", height=0.6)
            ax.text(-N * 0.015, y, lab, ha="right", va="center")
            ax.text(n + N * 0.015, y, f"{k} flagged, {n - k} missed", va="center")
        ax.set_xlim(0, N * 1.5); bare(ax); save(plt, fig, "recall")
    # false alarms
    rows = [(lab, k, n) for lab, (k, n) in (("gitta live warnings, no gate", m["live_fa"]), ("gitta with the test gate", m["gate_fa"])) if n]
    if rows:
        fig, ax = plt.subplots(figsize=(9, 0.7 * len(rows) + 0.9))
        for i, (lab, k, n) in enumerate(rows):
            y = len(rows) - 1 - i; p, lo, hi = wilson(k, n)
            ax.barh(y, 100 * p, color=GOLD, height=0.6); ax.plot([100 * lo, 100 * hi], [y, y], color=INK, lw=1.2)
            ax.text(-1.2, y, lab, ha="right", va="center")
            ax.text(100 * hi + 1.5, y, f"{100 * p:.1f}%, {k} of {n} pairs that work together", va="center")
        ax.set_xlim(0, 100); bare(ax, x=False); ax.set_xlabel("False alarms (%), line shows the 95% interval")
        save(plt, fig, "false_alarms")
    # conflicts vs paper
    fig, ax = plt.subplots(figsize=(8, 3.6)); top = 0
    for i, (lab, ours, paper) in enumerate((("Cross-agent pairs", m["conf_cross"], (48, 115)), ("Intra-agent pairs", m["conf_intra"], (119, 601)))):
        for j, ((k, n), col, who) in enumerate(((paper, MUTED, "Xu et al. 2026"), (ours, INK, "This replay"))):
            x = i * 3 + j; p, lo, hi = wilson(k, n)
            ax.bar(x, 100 * p, color=col, width=0.8); ax.plot([x, x], [100 * lo, 100 * hi], color=GOLD, lw=1.5); top = max(top, 100 * hi)
            ax.text(x, 100 * hi + 1.5, f"{100 * p:.1f}%\n{k} of {n}", ha="center", va="bottom", fontsize=11)
            ax.text(x, -2.5, who, ha="center", va="top", fontsize=10, color=MUTED)
        ax.text(i * 3 + 0.5, -9, lab, ha="center", va="top")
    ax.set_ylim(0, top + 18); ax.set_xlim(-0.8, 4.8); bare(ax); ax.set_ylabel("")
    ax.set_title("Textual conflict rate, line shows the 95% interval", fontsize=11, color=MUTED, loc="left")
    save(plt, fig, "conflict_vs_paper")
    # clouds
    ms = [x for x in metas if x.get("runner_h")]
    if ms:
        fig, axs = plt.subplots(1, 2, figsize=(11, 0.62 * len(ms) + 1.0), gridspec_kw={"wspace": 0.05})
        for ax, key, fmt, title in ((axs[0], "runner_h", "{:.1f} h", "Benchmark wall-clock"), (axs[1], "cost", "${:.2f}", "Total cost, VM and disk, apply to destroy")):
            top = max((x.get(key) or 0) for x in ms)
            for i, x in enumerate(ms):
                y = len(ms) - 1 - i; v = x.get(key) or 0
                ax.barh(y, v, color=INK if x["run"].endswith("8") else MUTED, height=0.6)
                ax.text(v + top * 0.02, y, fmt.format(v) if x.get(key) is not None else "pending", va="center")
                if ax is axs[0]: ax.text(-top * 0.02, y, f"{CLOUD[x['run'][:-1]]}, {x['type']}", ha="right", va="center")
            ax.set_xlim(0, top * 1.35); bare(ax); ax.set_title(title, fontsize=11, color=MUTED, loc="left")
        save(plt, fig, "clouds")


def main():
    runs = [r for r in RUN_ORDER if load(r)]
    if not runs: print("no results"); return
    data = {r: load(r) for r in runs}
    prim = PRIMARY if PRIMARY in data else runs[0]
    m = metrics(data[prim]); metas = [run_meta(r) for r in runs]
    graphs(m, metas)
    diffs = []
    for r in runs:
        if r == prim: continue
        for i, x in sorted(data[prim].items()):
            y = data[r].get(i)
            if y is None: diffs.append({"run": r, "idx": i, prim: classify(x), r: "missing"}); continue
            if classify(x) != classify(y) or live_flag(x) != live_flag(y) or x.get("head_a") != y.get("head_a") or x.get("head_b") != y.get("head_b") or x.get("merge_tree") != y.get("merge_tree"):
                diffs.append({"run": r, "idx": i, "repo": x["repo"], prim: classify(x) + (" +gitta" if live_flag(x) else ""), r: classify(y) + (" +gitta" if live_flag(y) else ""),
                              "same_shas": x.get("head_a") == y.get("head_a") and x.get("head_b") == y.get("head_b")})
    out = {"primary": prim, "metrics": m, "runs": {r: metrics(data[r]) for r in runs}, "meta": metas, "diffs": diffs}
    json.dump(out, open(os.path.join(HERE, "metrics.json"), "w"), indent=1, default=str)
    json.dump(diffs, open(os.path.join(HERE, "cross_cloud_diffs.json"), "w"), indent=1)
    # cloud comparison table (markdown), pasted into REPORT.md
    rows = ["| Run | Instance | vCPU / RAM | $/h VM + 200 GB disk | Benchmark wall-clock | Billed (apply to destroy) | Cost | Replayed / conflicts / evaluated / breaks | Live recall | Live false alarms | Gated false alarms |",
            "|---|---|---|---|---|---|---|---|---|---|---|"]
    for x in metas:
        rm = metrics(data[x["run"]]); zone = (x.get("machine") or {}).get("zone", "")
        f = lambda v, fmt: fmt.format(v) if v is not None else "n/a"
        rows.append(f"| {CLOUD[x['run'][:-1]]} {x['run'][-1]} | {x['type']}, {zone} | {'8 / 32 GB' if x['run'].endswith('8') else '4 / 16 GB'} | {x['price_h']:.4f} + {x['disk_h']:.4f} | "
                    f"{f(x['runner_h'], '{:.1f} h')} | {f(x['billed_h'], '{:.1f} h')} | {f(x['cost'], '${:.2f}')} | {rm['textual']} / {rm['counts'].get('conflict', 0)} / {rm['evaluated']} / {rm['breaks']} | "
                    f"{rm['live_recall'][0]}/{rm['live_recall'][1]} | {rm['live_fa'][0]}/{rm['live_fa'][1]} | {rm['gate_fa'][0]}/{rm['gate_fa'][1]} |")
    open(os.path.join(HERE, "cloud_table.md"), "w").write("\n".join(rows) + "\n")
    print(json.dumps({"primary": prim, "counts": m["counts"], "conf_intra": m["conf_intra"], "conf_cross": m["conf_cross"], "evaluated": m["evaluated"],
                      "breaks": m["breaks"], "flaky": m["flaky"], "live_recall": m["live_recall"], "live_fa": m["live_fa"], "gate_recall": m["gate_recall"],
                      "gate_fa": m["gate_fa"], "conflict_agree": m["gitta_conflict_agree"], "clean_nomerge": m["gitta_clean_says_nomerge"],
                      "eco": m["eco"], "diffs": len(diffs),
                      "metas": [{k: v for k, v in x.items() if k in ("run", "type", "runner_h", "billed_h", "cost", "setup_min")} for x in metas]}, indent=1, default=str))


if __name__ == "__main__":
    main()
