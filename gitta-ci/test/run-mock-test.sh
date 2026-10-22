#!/usr/bin/env bash
# End-to-end test of gitta-mr-check.sh against the mock APIs and real Git repositories.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd); G=${GITTA:-$HERE/../../gitta-cli/target/release/gitta}
SRV=; T=$(mktemp -d); trap 'kill $SRV 2>/dev/null; rm -rf "$T"' EXIT
# Origin with three merge requests: 1 lowers VAT, 2 adds a label expecting 25 %, 3 is unrelated.
git init -q --bare "$T/origin.git"; git clone -q "$T/origin.git" "$T/w" 2>/dev/null; cd "$T/w"
git checkout -q -b main 2>/dev/null || true; git config user.name t; git config user.email t@t
mkdir shop tests; touch shop/__init__.py tests/__init__.py
printf 'VAT = 0.25\n\n\ndef gross(x):\n    return round(x * (1 + VAT), 2)\n' > shop/p.py
printf 'import unittest\nfrom shop.p import gross\n\n\nclass T(unittest.TestCase):\n    def test_g(self):\n        self.assertEqual(gross(100), 125.0)\n' > tests/test_p.py
git add -A; git commit -qm base; git push -q origin HEAD:main
branch() { git checkout -q -b "$1" main; eval "$2"; git add -A; git commit -qm "$1"; git push -q origin "$1"; git push -q origin "$1:refs/merge-requests/$3/head"; git checkout -q main; }
branch vat 'sed -i.b "s/0.25/0.12/" shop/p.py; rm shop/p.py.b; sed -i.b "s/125.0/112.0/" tests/test_p.py; rm tests/test_p.py.b' 1
branch label 'printf "from shop.p import gross\n\n\ndef label(x):\n    return f\"{gross(x)} kr incl. 25%% VAT\"\n" > shop/l.py; printf "import unittest\nfrom shop.l import label\n\n\nclass L(unittest.TestCase):\n    def test_l(self):\n        self.assertEqual(label(100), \"125.0 kr incl. 25%% VAT\")\n" > tests/test_l.py' 2
branch readme 'echo "# Shop" > README.md' 3
cat > "$T/mrs.json" <<J
[{"id": 1, "target": "main", "source": "vat", "title": "Lower VAT"}, {"id": 2, "target": "main", "source": "label", "title": "Add price label"}, {"id": 3, "target": "main", "source": "readme", "title": "Readme"}]
J
PORT=$(( 20000 + RANDOM % 20000 )); python3 "$HERE/mock_api.py" $PORT "$T" & SRV=$!; sleep 0.8
run() {  # provider mode
  ( cd "$T/w" && PROVIDER=$1 MODE=$2 GITTA="$G" GATE="python3 -m unittest -q" GITTA_TOKEN=secret SYSTEM_ACCESSTOKEN=secret \
    CI_API_V4_URL="http://127.0.0.1:$PORT/api/v4" CI_PROJECT_ID=7 CI_MERGE_REQUEST_IID=2 \
    SYSTEM_COLLECTIONURI="http://127.0.0.1:$PORT/org/" SYSTEM_TEAMPROJECT=proj BUILD_REPOSITORY_ID=repo1 SYSTEM_PULLREQUEST_PULLREQUESTID=2 \
    bash "$HERE/../gitta-mr-check.sh" ) ; echo "exit=$?"
}
check() { python3 - "$T/comments.json" "$1" <<'P'
import json, sys
c = json.load(open(sys.argv[1])); p = sys.argv[2]
for k in sorted(c):
    if not k.startswith(p): continue
    bodies = [x['body'] for x in c[k]]
    first = bodies[0].splitlines()[1] if bodies else ''
    breaks = any('Breaks together' in b for b in bodies)
    print(f"  {k}: {len(bodies)} comment(s){' · breaks reported' if breaks else ''} · {first}")
P
}
for prov in gitlab azure; do
  echo "== $prov: current merge request"; run $prov current || true
  echo "== $prov: every merge request"; run $prov all || true
  echo "== $prov: again (comments must be updated, not duplicated)"; run $prov all || true
  check $prov
done
