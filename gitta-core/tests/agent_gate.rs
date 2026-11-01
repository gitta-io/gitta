//! The agent diff gate: CI and hook weakening, and the test notes on agent work.
use gitta_core::guard::{ci_structure, hook_bypass, hooks_configured, safety_net, HOOK_TRAILER};
use gitta_core::Snapshot;

fn snap(v: &[(&str, &str)]) -> Snapshot { v.iter().map(|(p, t)| (p.to_string(), t.as_bytes().to_vec())).collect() }
fn details(b: &Snapshot, n: &Snapshot) -> Vec<String> {
    let paths: Vec<String> = b.keys().chain(n.keys()).cloned().collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    safety_net(b, n, &paths).into_iter().map(|f| format!("{} {}", f.kind, f.detail)).collect()
}

const WF: &str = "name: ci\non:\n  pull_request:\npermissions:\n  contents: read\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n      - run: cargo build\n  lint:\n    runs-on: ubuntu-latest\n    steps:\n      - run: cargo fmt --check\n";

#[test]
fn ci_if_false_and_removed_job() {
    let p = ".github/workflows/ci.yml";
    let off = WF.replace("  test:\n    runs-on", "  test:\n    if: false\n    runs-on");
    let d = details(&snap(&[(p, WF)]), &snap(&[(p, &off)]));
    assert!(d.iter().any(|x| x.starts_with("CI_WEAKENED") && x.contains("if: false")), "{d:?}");
    let gone = WF.replace("  lint:\n    runs-on: ubuntu-latest\n    steps:\n      - run: cargo fmt --check\n", "");
    let d = details(&snap(&[(p, WF)]), &snap(&[(p, &gone)]));
    assert!(d.iter().any(|x| x.contains("removed CI job(s) lint")), "{d:?}");
    // Negative control: a job renamed in place and a new step are not weakening.
    assert!(ci_structure(p, WF, &WF.replace("      - run: cargo build\n", "      - run: cargo build\n      - run: cargo doc\n")).is_empty());
    assert!(ci_structure(p, WF, &WF.replace("if: github.ref", "")).is_empty());
}

#[test]
fn gitlab_job_removed() {
    let p = ".gitlab-ci.yml";
    let ci = "stages: [test]\nunit:\n  stage: test\n  script:\n    - make test\nsast:\n  stage: test\n  script:\n    - semgrep ci\n";
    let d = ci_structure(p, ci, "stages: [test]\nunit:\n  stage: test\n  script:\n    - make test\n");
    assert_eq!(d, vec!["removed CI job(s) sast".to_string()]);
}

#[test]
fn hook_bypass_trace() {
    let c = |s: &str, m: &str| (s.to_string(), m.to_string(), 1usize);
    let ok = c("aaaaaaaaaaaa", &format!("Fix parser\n\n{HOOK_TRAILER}\n"));
    let skipped = c("bbbbbbbbbbbb", "Fix parser again\n");
    let f = hook_bypass(&[ok.clone(), skipped.clone()], true, false);
    assert_eq!(f.len(), 1);
    assert!(f[0].detail.contains("bbbbbbbbbb") && !f[0].detail.contains("aaaaaaaaaa"), "{}", f[0].detail);
    // Negative controls: no hooks configured, or the trace not in use in this repository.
    assert!(hook_bypass(&[skipped.clone()], false, true).is_empty());
    assert!(hook_bypass(&[skipped.clone()], true, false).is_empty());
    // A merge commit is left out.
    assert!(hook_bypass(&[("cccccccccc".into(), "Merge branch x".into(), 2)], true, true).is_empty());
    assert!(hooks_configured(&[".pre-commit-config.yaml".into()]) && !hooks_configured(&["README.md".into()]));
}

use gitta_core::test_notes::{flaky_tests, untested};

fn untested_notes(b: &Snapshot, n: &Snapshot) -> Vec<(String, String, usize)> {
    let paths: Vec<String> = b.keys().chain(n.keys()).cloned().collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    untested(b, n, &paths).into_iter().map(|x| (x.rule, x.severity, x.line)).collect()
}

fn notes(b: &Snapshot, n: &Snapshot) -> Vec<(String, String, usize)> {
    let paths: Vec<String> = b.keys().chain(n.keys()).cloned().collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    flaky_tests(b, n, &paths).into_iter().map(|x| (x.rule, x.severity, x.line)).collect()
}
fn has(v: &[(String, String, usize)], rule: &str) -> bool { v.iter().any(|x| x.0 == rule) }

#[test]
fn untested_change_and_error_paths() {
    let p = "app/svc.py";
    let base = "def f(x):\n    return x\n";
    let plain = "def f(x):\n    return x + 1\n";
    let n = untested_notes(&snap(&[(p, base)]), &snap(&[(p, plain)]));
    let u: Vec<_> = n.iter().filter(|x| x.0 == "untested-change").collect();
    assert_eq!(u.len(), 1, "{n:?}");
    assert_eq!(u[0].1, "low");
    let catchy = "def f(x):\n    try:\n        return g(x)\n    except KeyError:\n        return 0\n";
    let n = untested_notes(&snap(&[(p, base)]), &snap(&[(p, catchy)]));
    assert!(n.iter().any(|x| x.0 == "untested-change" && x.1 == "medium" && x.2 == 4), "{n:?}");
    // Negative control: the same change with a test change.
    let n = untested_notes(&snap(&[(p, base), ("tests/test_svc.py", "def test_f():\n    assert f(1) == 1\n")]), &snap(&[(p, plain), ("tests/test_svc.py", "def test_f():\n    assert f(1) == 2\n")]));
    assert!(!has(&n, "untested-change"), "{n:?}");
}

#[test]
fn assertion_strength_drop() {
    let p = "tests/test_calc.py";
    let base = "def test_add():\n    assert add(1, 2) == 3\n    self.assertEqual(add(2, 2), 4)\n";
    let weak = "def test_add():\n    assert add(1, 2) is not None\n    self.assertTrue(add(2, 2))\n";
    let d = details(&snap(&[(p, base)]), &snap(&[(p, weak)]));
    assert!(d.iter().any(|x| x.starts_with("ASSERTIONS_WEAKENED")), "{d:?}");
    // Negative controls: a stronger test, and an added weak check next to the strong ones.
    assert!(details(&snap(&[(p, weak)]), &snap(&[(p, base)])).iter().all(|x| !x.starts_with("ASSERTIONS_WEAKENED")));
    let more = format!("{base}    assert result\n");
    assert!(details(&snap(&[(p, base)]), &snap(&[(p, &more)])).iter().all(|x| !x.starts_with("ASSERTIONS_WEAKENED")));
    let js = "test('a', () => {\n  expect(sum(1, 2)).toBe(3);\n});\n";
    let d = details(&snap(&[("src/sum.test.js", js)]), &snap(&[("src/sum.test.js", &js.replace("toBe(3)", "toBeDefined()"))]));
    assert!(d.iter().any(|x| x.starts_with("ASSERTIONS_WEAKENED")), "{d:?}");
}

#[test]
fn flaky_test_idioms() {
    for (p, line) in [("tests/test_a.py", "    time.sleep(2)\n"), ("tests/test_a.py", "    now = datetime.now()\n"), ("tests/test_a.py", "    n = random.randint(1, 9)\n"),
                      ("tests/test_a.py", "    with open(\"out.txt\", \"w\") as f:\n"), ("src/a.test.ts", "  const t = Date.now();\n"), ("pkg/a_test.go", "\ttime.Sleep(100 * time.Millisecond)\n"),
                      ("src/test/java/ATest.java", "        Thread.sleep(500);\n")] {
        let n = notes(&snap(&[(p, "x = 1\n")]), &snap(&[(p, &format!("x = 1\n{line}"))]));
        assert!(n.iter().any(|x| x.0 == "flaky-test" && x.1 == "low"), "{p}: {line}: {n:?}");
    }
    // Negative controls: a seeded generator, a temporary directory fixture, and the same calls outside tests.
    let n = notes(&snap(&[("tests/test_a.py", "x = 1\n")]), &snap(&[("tests/test_a.py", "x = 1\nrandom.seed(4)\nn = random.randint(1, 9)\ndef test_w(tmp_path):\n    with open(tmp_path / \"o.txt\", \"w\") as f:\n        f.write(\"x\")\n")]));
    assert!(!has(&n, "flaky-test"), "{n:?}");
    let n = notes(&snap(&[("app/a.py", "x = 1\n")]), &snap(&[("app/a.py", "x = 1\ntime.sleep(2)\n")]));
    assert!(!has(&n, "flaky-test"), "{n:?}");
}
