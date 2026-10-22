"""Build the pair list once (Xu et al. 2026 method): AIDev tag v3 (= AIDev-pop), k=0 co-activity,
one pair per repository: all 122 cross-agent repos (stratum B) + 625 intra-agent repos (stratum A)
drawn from the repos with intra-agent co-active pairs and no cross-agent pair. Seed fixed.
usage: python build_pairs.py pull_request.parquet pairs.jsonl"""
import pandas as pd, random, json, sys
SEED = 20261020
p = pd.read_parquet(sys.argv[1])
p["o"] = pd.to_datetime(p.created_at, utc=True); p["c"] = pd.to_datetime(p.closed_at, utc=True)
cut = max(p.o.max(), p.c.max()); p["c"] = p.c.fillna(cut)
rng = random.Random(SEED)
cross, intra = {}, {}
for rid, g in sorted(p.groupby("repo_id"), key=lambda x: x[0]):
    g = g.sort_values(["o", "number"]); rows = list(zip(g.number, g.agent, g.o, g.c, g.repo_url))
    for i in range(len(rows)):
        for j in range(i + 1, len(rows)):
            if rows[j][2] > rows[i][3]: continue
            d = cross if rows[i][1] != rows[j][1] else intra
            d.setdefault(rid, []).append((rows[i], rows[j]))
pairs = []
for rid in sorted(cross): pairs.append(("cross", rid, rng.choice(cross[rid])))
pool = sorted(r for r in intra if r not in cross)
for rid in sorted(rng.sample(pool, 625)): pairs.append(("intra", rid, rng.choice(intra[rid])))
rng.shuffle(pairs)
with open(sys.argv[2], "w") as f:
    for k, (st, rid, (a, b)) in enumerate(pairs):
        url = a[4].replace("https://api.github.com/repos/", "https://github.com/")
        f.write(json.dumps({"idx": k, "stratum": st, "repo_id": int(rid), "repo": url, "pr_a": int(a[0]), "agent_a": a[1],
                            "pr_b": int(b[0]), "agent_b": b[1]}) + "\n")
print(len(pairs), "pairs; intra pool", len(pool), "seed", SEED)
