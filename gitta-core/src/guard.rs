//! Safety-net guard: did a change weaken the checks that are supposed to catch bad changes?
//!
//! Deterministic and cheap (one pass over each changed file, base vs new). Findings:
//!   TEST_DELETED       a test file was removed, or CTest registrations (`add_test` ...) were taken out of CMake files
//!   ASSERTIONS_REMOVED a test file has fewer assertions than before
//!   TEST_SKIPPED       new skip / xfail / ignore / .only markers (C and C++: `DISABLED_`, `GTEST_SKIP`, `SKIP`, tests
//!                      put under `#if 0`)
//!   CI_WEAKENED        CI configuration now tolerates failures or lost test/lint/scan steps
//!   THRESHOLD_LOWERED  a coverage or quality threshold went down
//!   CHECK_SILENCED     new suppression comments (nosec, noqa, eslint-disable, @ts-ignore, #[allow] ...)
//!   SECURITY_WEAKENED  a security control was removed or switched off in code (see security.rs)
//! Agents are known to "fix" failing work this way; Gitta's promise that only tested work lands depends on it not happening silently.

use crate::Snapshot;
use serde::Serialize;

#[derive(Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Finding { pub kind: String, pub path: String, pub detail: String }

fn file(p: &str) -> &str { p.rsplit('/').next().unwrap_or(p) }

pub fn is_test(p: &str) -> bool {
    let f = file(p);
    p.starts_with("tests/") || p.contains("/tests/") || p.starts_with("test/") || p.contains("/test/") || p.contains("__tests__/")
        || f.starts_with("test_") || f.contains("_test.") || f.contains(".test.") || f.contains(".spec.") || ["Test.java", "Tests.java", "Test.kt", "Tests.kt", "Test.cs", "Tests.cs"].iter().any(|e| f.ends_with(e))
        || is_c_test(f)
}

/// C and C++ test files by name: `foo_test.cc`, `foo_unittest.cpp`, `test_foo.c`, `FooTest.cpp`, `foo.test.cpp`.
fn is_c_test(f: &str) -> bool {
    let Some((stem, ext)) = f.rsplit_once('.') else { return false };
    if !matches!(ext, "c" | "cc" | "cpp" | "cxx" | "c++") { return false; }
    let lo = stem.to_ascii_lowercase();
    lo.ends_with("_test") || lo.ends_with("_tests") || lo.ends_with("_unittest") || lo.ends_with("-test") || lo.starts_with("test_") || lo.starts_with("tests_")
        || stem.ends_with("Test") || stem.ends_with("Tests") || lo.ends_with("_spec")
}

/// C and C++ test code by content: GoogleTest, Catch2, doctest, Boost.Test, CppUnit, Unity, CMocka.
fn c_test_code(t: &str) -> bool {
    ["TEST(", "TEST_F(", "TEST_P(", "TYPED_TEST(", "TEST_CASE(", "SCENARIO(", "BOOST_AUTO_TEST_CASE(", "CPPUNIT_TEST(", "RUN_TEST(", "cmocka_unit_test("]
        .iter().any(|m| t.lines().any(|l| l.trim_start().starts_with(m)))
}

/// Test registrations in CMake files (CTest): `add_test(`, `gtest_discover_tests(` ...
const CTEST: &[&str] = &["add_test(", "gtest_discover_tests(", "gtest_add_tests(", "catch_discover_tests(", "doctest_discover_tests("];

/// Lines between `#if 0` and its `#endif` that hold test cases: tests switched off by the preprocessor.
fn tests_under_if0(t: &str) -> usize {
    let (mut depth, mut off, mut n) = (0usize, None::<usize>, 0usize);
    for l in t.lines() {
        let d = l.trim_start();
        if let Some(r) = d.strip_prefix('#') {
            let r = r.trim_start();
            if r.starts_with("if") { depth += 1; if off.is_none() && r.strip_prefix("if").map(|c| c.trim() == "0" || c.trim().starts_with("0 ")).unwrap_or(false) { off = Some(depth); } continue; }
            if r.starts_with("endif") { if off == Some(depth) { off = None; } depth = depth.saturating_sub(1); continue; }
            if (r.starts_with("else") || r.starts_with("elif")) && off == Some(depth) { off = None; continue; }
        }
        if off.is_some() && c_test_code(l) { n += 1; }
    }
    n
}

fn is_ci(p: &str) -> bool {
    p.starts_with(".github/workflows/") || p == ".gitlab-ci.yml" || p.ends_with("Jenkinsfile") || p.starts_with(".circleci/") || p == "azure-pipelines.yml"
        || p.starts_with(".buildkite/") || p == ".travis.yml" || p == "bitbucket-pipelines.yml" || p.ends_with(".pre-commit-config.yaml")
}

fn is_threshold_config(p: &str) -> bool {
    let f = file(p);
    ["package.json", "pyproject.toml", "setup.cfg", "tox.ini", ".coveragerc", "codecov.yml", ".codecov.yml", ".nycrc", ".nycrc.json", "Cargo.toml", "pom.xml", "build.gradle", "build.gradle.kts", "sonar-project.properties"].contains(&f)
        || f.starts_with("jest.config") || f.starts_with("vitest.config") || f.starts_with("karma.conf") || is_ci(p)
}

/// Lines holding at least one of the markers: overlapping markers (`@unittest.skip(` matches three) count once.
fn count(text: &str, needles: &[&str]) -> usize { text.lines().filter(|l| needles.iter().any(|n| l.contains(n))).count() }

const ASSERTS: &[&str] = &["assert", "expect(", "expect!(", "t.Error", "t.Fatal", "t.Fail", "require.", "should.", ".should(", "verify(", "assertThat(", "Assert.", "EXPECT ",
    // C and C++: GoogleTest, Catch2, doctest, Boost.Test, CppUnit, Unity, CMocka
    "ASSERT_", "EXPECT_", "REQUIRE(", "REQUIRE_", "CHECK(", "CHECK_", "BOOST_CHECK", "BOOST_REQUIRE", "CPPUNIT_ASSERT", "TEST_ASSERT", "assert_int_equal(", "assert_true("];
const SKIPS: &[&str] = &["@pytest.mark.skip", "@pytest.mark.xfail", "@unittest.skip", "pytest.skip(", "unittest.skip(", ".skip(", "it.skip", "describe.skip", "test.skip",
    "xit(", "xdescribe(", "xtest(", ".only(", "fit(", "fdescribe(", "t.Skip(", "#[ignore]", "@Disabled", "@Ignore", "skipIf(", "todo(", "(Skip =", "[Ignore",
    // C and C++: GoogleTest DISABLED_ tests and GTEST_SKIP, Catch2 SKIP and hidden tags, doctest skip, Boost disabled
    "DISABLED_", "GTEST_SKIP", "SKIP(", "\"[.]", "[!mayfail]", "doctest::skip(", "boost::unit_test::disabled(", "TEST_IGNORE("];
const SILENCERS: &[&str] = &["nosec", "noqa", "type: ignore", "eslint-disable", "@ts-ignore", "@ts-nocheck", "@ts-expect-error", "@SuppressWarnings", "#[allow(",
    "#![allow(", "//nolint", "// nolint", "pylint: disable", "NOLINT", "rubocop:disable", "nolint:", "istanbul ignore", "c8 ignore", "pragma: no cover", "noinspection", "#pragma warning disable", "[SuppressMessage", "@Suppress("];
const CI_TOLERATE: &[&str] = &["continue-on-error: true", "|| true", "allow_failure: true", "allow-failure: true", "set +e", "--no-verify", "exit 0"];
const CI_CHECKS: &[&str] = &["test", "lint", "audit", "scan", "check", "codeql", "semgrep", "coverage", "typecheck", "tsc", "clippy", "mypy", "pytest", "vitest", "jest"];

/// Numbers on lines that look like thresholds (coverage, fail_under, minimum ...), keyed by the line's text before the number.
fn thresholds(text: &str) -> Vec<(String, f64)> {
    let keys = ["threshold", "fail_under", "fail-under", "failunder", "minimum", "min_coverage", "coverage", "branches", "lines", "statements", "functions", "jacoco"];
    let mut out = vec![];
    for line in text.lines() {
        let l = line.to_lowercase();
        if !keys.iter().any(|k| l.contains(k)) { continue; }
        let digits: String = l.chars().map(|c| if c.is_ascii_digit() || c == '.' { c } else { ' ' }).collect();
        if let Some(v) = digits.split_whitespace().filter_map(|x| x.parse::<f64>().ok()).last() {
            let label: String = l.split(|c: char| c.is_ascii_digit()).next().unwrap_or("").trim().trim_end_matches(['=', ':', '"', ' ']).to_string();
            out.push((label, v));
        }
    }
    out
}

/// Weakening of the safety net between `base` and `new` for the given changed paths.
pub fn safety_net(base: &Snapshot, new: &Snapshot, paths: &[String]) -> Vec<Finding> {
    let mut out = crate::security::weakened(base, new, paths);
    let txt = |s: &Snapshot, p: &str| s.get(p).map(|b| String::from_utf8_lossy(b).to_string());
    for p in paths {
        let (b, n) = (txt(base, p), txt(new, p));
        let f = |kind: &str, detail: String| Finding { kind: kind.into(), path: p.clone(), detail };
        if p == ".gitta-security" {
            // The repository's own list of security checks: an entry taken off it stops protecting that code.
            // `[allow]` entries are layer 1 allows: taking one off makes the checks stricter.
            let marks = |t: &Option<String>| -> Vec<String> { crate::security::entries(t.as_deref().unwrap_or("")).into_iter().filter(|e| !e.starts_with("[allow]")).collect() };
            let now = marks(&n);
            let gone: Vec<String> = marks(&b).into_iter().filter(|m| !now.contains(m)).collect();
            if !gone.is_empty() { out.push(f("SECURITY_WEAKENED", format!("took {} off the list of security checks", gone.join(", ")))); }
        }
        match (&b, &n) {
            (Some(_), None) if is_test(p) => { out.push(f("TEST_DELETED", "test file removed".into())); continue; }
            (_, None) => continue,
            _ => {}
        }
        let (b, n) = (b.unwrap_or_default(), n.unwrap_or_default());
        if is_test(p) || n.contains("#[test]") || n.contains("#[cfg(test)]") || (crate::c_cpp::lang_of(p).is_some() && c_test_code(&b)) {
            let (ab, an) = (count(&b, ASSERTS), count(&n, ASSERTS));
            if an < ab { out.push(f("ASSERTIONS_REMOVED", format!("{} fewer assertion(s) ({ab} → {an})", ab - an))); }
            let (sb, sn) = (count(&b, SKIPS) + tests_under_if0(&b), count(&n, SKIPS) + tests_under_if0(&n));
            if sn > sb { out.push(f("TEST_SKIPPED", format!("{} new skip/ignore/only marker(s)", sn - sb))); }
        }
        let fname = file(p);
        if fname == "CMakeLists.txt" || fname.ends_with(".cmake") {
            let (rb, rn) = (count(&b, CTEST), count(&n, CTEST));
            if rn < rb { out.push(f("TEST_DELETED", format!("{} CTest registration(s) removed ({rb} → {rn})", rb - rn))); }
        }
        let (qb, qn) = (count(&b, SILENCERS), count(&n, SILENCERS));
        if qn > qb { out.push(f("CHECK_SILENCED", format!("{} new suppression comment(s)", qn - qb))); }
        if is_ci(p) {
            let (tb, tn) = (count(&b, CI_TOLERATE), count(&n, CI_TOLERATE));
            if tn > tb { out.push(f("CI_WEAKENED", "CI now tolerates failures (continue-on-error, || true, allow_failure ...)".into())); }
            let checks = |t: &str| t.lines().filter(|l| { let l = l.to_lowercase(); (l.contains("run:") || l.contains("script:") || l.contains("- ") || l.contains("uses:")) && CI_CHECKS.iter().any(|c| l.contains(c)) }).count();
            let (cb, cn) = (checks(&b), checks(&n));
            if cn < cb { out.push(f("CI_WEAKENED", format!("{} test/lint/scan step(s) removed", cb - cn))); }
        }
        if is_threshold_config(p) {
            let (tb, tn) = (thresholds(&b), thresholds(&n));
            for (label, v) in &tn {
                if let Some((_, old)) = tb.iter().find(|(l, _)| l == label) { if v < old { out.push(f("THRESHOLD_LOWERED", format!("{label}: {old} → {v}"))); } }
            }
        }
    }
    out
}

/// Credential patterns: (label, prefix-or-marker, minimum tail length of [A-Za-z0-9_-]).
const SECRETS: &[(&str, &str, usize)] = &[("private key", "-----BEGIN ", 0), ("AWS access key", "AKIA", 16), ("GitHub token", "ghp_", 30),
    ("GitHub token", "github_pat_", 30), ("Slack token", "xoxb-", 20), ("Slack token", "xoxp-", 20), ("OpenAI key", "sk-proj-", 30),
    ("Anthropic key", "sk-ant-", 30), ("Stripe key", "sk_live_", 20), ("Google API key", "AIza", 30)];

/// The first credential in `text`, by label: well-known key formats, and secret-named settings with a real-looking value.
pub fn find_secret(text: &str) -> Option<String> { find_secret_at(text).map(|(_, l)| l) }

/// The first credential in `text` with its 1-based line number.
pub fn find_secret_at(text: &str) -> Option<(usize, String)> {
    let line_of = |at: usize| text[..at].matches('\n').count() + 1;
    for (label, marker, min) in SECRETS {
        let mut from = 0;
        while let Some(i) = text[from..].find(marker) {
            let at = from + i + marker.len();
            let tail = text[at..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').count();
            if *marker == "-----BEGIN " { if text[at..].starts_with("RSA PRIVATE KEY") || text[at..].starts_with("PRIVATE KEY") || text[at..].starts_with("OPENSSH PRIVATE KEY") || text[at..].starts_with("EC PRIVATE KEY") { return Some((line_of(from + i), label.to_string())); } }
            else if tail >= *min { return Some((line_of(from + i), label.to_string())); }
            from = at;
        }
    }
    for (n, line) in text.lines().enumerate() {
        if assigned_secret(line) { return Some((n + 1, "hard-coded credential".into())); }
    }
    None
}

/// A credential on a line of `text` that `base` does not have, with its 1-based line number. A credential that was
/// already there does not stop an unrelated edit.
pub fn secret_in_new_lines(base: &str, text: &str) -> Option<(usize, String)> {
    let old: std::collections::BTreeSet<&str> = base.lines().collect();
    // Keep line numbers: lines that were already there are blanked, not dropped.
    let fresh: String = text.lines().map(|l| if old.contains(l) { "" } else { l }).collect::<Vec<_>>().join("\n");
    find_secret_at(&fresh)
}

/// `password = "literal"`, `"api_key": "literal"`, `secret_key => 'literal'`: a secret-named setting that is given
/// a real-looking literal. The name must be assigned the value; a name that only appears inside a string or a
/// template (`"{conn.password}@".format(...)`) is not a setting.
fn assigned_secret(line: &str) -> bool {
    let l = line.to_lowercase();
    for k in ["password", "passwd", "secret_key", "api_key", "apikey", "auth_token"] {
        let mut from = 0;
        while let Some(i) = l[from..].find(k) {
            let start = from + i;
            from = start + k.len();
            // The rest of the setting name (`password_hash`, `api_key2`), then an optional closing quote of a key.
            let rest = &line[from..];
            let name_end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-')).unwrap_or(rest.len());
            let after = rest[name_end..].trim_start_matches(['"', '\'', ']']).trim_start();
            let Some(op) = ["=>", ":=", "=", ":"].iter().find(|o| after.starts_with(**o)) else { continue };
            if after.starts_with("==") { continue; }
            let v = after[op.len()..].trim_start();
            let Some(q) = v.chars().next().filter(|c| *c == '"' || *c == '\'') else { continue };
            let val: String = v[1..].chars().take_while(|c| *c != q).collect();
            let fake = val.contains(' ') || val.contains("${") || val.contains('{') || val.contains('(') || val.contains('<') || val.contains('%')
                || ["example", "xxx", "changeme", "placeholder", "dummy"].iter().any(|w| val.to_lowercase().contains(w));
            if val.len() >= 12 && !fake { return true; }
        }
    }
    false
}
