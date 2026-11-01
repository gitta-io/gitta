//! Safety-net guard: did a change weaken the checks that are supposed to catch bad changes?
//!
//! Deterministic and cheap (one pass over each changed file, base vs new). Findings:
//!   TEST_DELETED       a test file was removed, or CTest registrations (`add_test` ...) were taken out of CMake files
//!   ASSERTIONS_REMOVED a test file has fewer assertions than before
//!   ASSERTIONS_WEAKENED value checks (assertEqual, toEqual, assert x == y ...) replaced by weaker ones that check only
//!                      truth or presence (assertTrue(x), `is not None`, toBeTruthy ...)
//!   TEST_SKIPPED       new skip / xfail / ignore / .only markers (C and C++: `DISABLED_`, `GTEST_SKIP`, `SKIP`, tests
//!                      put under `#if 0`)
//!   CI_WEAKENED        CI configuration now tolerates failures or lost test/lint/scan steps
//!   THRESHOLD_LOWERED  a coverage or quality threshold went down
//!   CHECK_SILENCED     new suppression comments (nosec, noqa, eslint-disable, @ts-ignore, #[allow] ...)
//!   TEST_EXIT          test code or test setup newly exits or skips the run itself (`sys.exit`, `os._exit`,
//!                      `process.exit`, `os.Exit`, `raise SkipTest` ...), so the tests after it never run
//!   TEST_RUNNER_CHANGED the test runner's configuration changed (conftest.py, pytest.ini, jest/vitest config ...)
//!   ALWAYS_EQUAL       a new equality method that answers true for everything (`__eq__` returning True,
//!                      `equals` returning true, `fn eq` returning true), which makes every comparison pass
//! Agents are known to "fix" failing work this way; Gitta's promise that only tested work lands depends on it not happening silently.

use crate::Snapshot;
use serde::Serialize;

#[derive(Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Finding { pub kind: String, pub path: String, pub detail: String }

fn file(p: &str) -> &str { p.rsplit('/').next().unwrap_or(p) }

/// The optional checks this binary was compiled with. The verdict depends on it: a build WITHOUT an optional
/// language feature cannot raise that feature's findings or parse that language, so a clean verdict from such a build
/// is not the same as a clean verdict from a full one. Reported in the audit/result record so that difference is
/// observable; it never changes what any feature does.
pub fn features() -> Vec<&'static str> {
    let mut v = Vec::new();
    if cfg!(feature = "mainframe") { v.push("mainframe"); }
    if cfg!(feature = "abap") { v.push("abap"); }
    if cfg!(feature = "ibmi") { v.push("ibmi"); }
    if cfg!(feature = "fortran") { v.push("fortran"); }
    if cfg!(feature = "sql") { v.push("sql"); }
    if cfg!(feature = "iac") { v.push("iac"); }
    if cfg!(feature = "network") { v.push("network"); }
    if cfg!(feature = "iso20022") { v.push("iso20022"); }
    v
}

pub fn is_test(p: &str) -> bool {
    let f = file(p);
    p.starts_with("tests/") || p.contains("/tests/") || p.starts_with("test/") || p.contains("/test/") || p.contains("__tests__/")
        || f.starts_with("test_") || f.contains("_test.") || f.contains(".test.") || f.contains(".spec.") || ["Test.java", "Tests.java", "Test.kt", "Tests.kt", "Test.cs", "Tests.cs"].iter().any(|e| f.ends_with(e))
        || mainframe_test(f)
        || is_c_test(f)
        || is_rust_test_module(p, f)
}

/// Rust unit test modules kept in their own file: `src/foo/tests.rs` or `src/foo/test.rs` (the `#[cfg(test)] mod
/// tests;` form), and module files under a `tests/` directory inside src (`src/foo/tests/bar.rs`).
fn is_rust_test_module(p: &str, f: &str) -> bool {
    p.ends_with(".rs") && (f == "tests.rs" || f == "test.rs" || p.split('/').rev().skip(1).any(|d| d == "tests"))
}

fn mainframe_test(_: &str) -> bool { false }

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

pub(crate) fn is_ci(p: &str) -> bool {
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
/// Calls that end the process or skip the run from inside test code or test setup.
const TEST_EXITS: &[&str] = &["sys.exit(", "os._exit(", "raise SystemExit", "raise SkipTest", "raise unittest.SkipTest", "pytest.exit(",
    "process.exit(", "process.kill(process.pid", "os.Exit(", "std::process::exit(", "System.exit(", "Environment.Exit(", "exitProcess(", "_exit("];

/// Settings in runner configuration that decide which tests run or whether a failure counts. A fixture or a
/// helper added to conftest.py is ordinary work; these are not.
const RUNNER_SELECTORS: &[&str] = &["collect_ignore", "pytest_collection_modifyitems", "pytest_ignore_collect", "pytest_runtest_", "pytest_report", "addopts", "testpaths",
    "python_files", "python_classes", "python_functions", "norecursedirs", "--deselect", "--ignore", " -k ", "xfail_strict", "filterwarnings",
    "testMatch", "testRegex", "testPathIgnorePatterns", "modulePathIgnorePatterns", "passWithNoTests", "bail", "exclude", "include", "testNamePattern",
    "grep", "invert", "forbidOnly", "retries", "spec:", "\"spec\"", "<exclude", "<testsuite", "<groups"];

/// Files that configure how the test runner finds, runs or reports tests.
fn is_test_runner_config(p: &str) -> bool {
    let f = file(p);
    matches!(f, "conftest.py" | "pytest.ini" | "tox.ini" | ".mocharc.js" | ".mocharc.json" | ".mocharc.yml" | ".mocharc.yaml" | "karma.conf.js" | "phpunit.xml" | "phpunit.xml.dist" | "jest.setup.js" | "jest.setup.ts" | "vitest.setup.ts")
        || f.starts_with("jest.config.") || f.starts_with("vitest.config.") || f.starts_with("playwright.config.") || f.starts_with("cypress.config.")
}

/// Equality methods whose body answers true whatever they are given: `__eq__` returning True, `equals`
/// returning true, Rust `fn eq` returning true. Counted per text, so only new ones are reported.
fn always_equal(text: &str) -> usize {
    let lines: Vec<&str> = text.lines().collect();
    let mut n = 0;
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim();
        let head = t.starts_with("def __eq__(") || (t.contains(" equals(") && t.contains("boolean")) || t.starts_with("fn eq(&self") || t.contains("bool Equals(");
        if !head { continue; }
        // The first statement of the body (the same line for a one-line body).
        let body = std::iter::once(t.split_once('{').map(|(_, b)| b).unwrap_or("")).chain(lines[i + 1..].iter().map(|x| x.trim()))
            .map(|x| x.trim().trim_end_matches('}').trim()).find(|x| !x.is_empty() && !x.starts_with('#') && !x.starts_with("//") && !x.starts_with("\"\"\""));
        if matches!(body, Some("return True") | Some("return true;") | Some("true") | Some("return true")) { n += 1; }
    }
    n
}

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
    let mut out = vec![];
    let txt = |s: &Snapshot, p: &str| s.get(p).map(|b| String::from_utf8_lossy(b).to_string());
    for p in paths {
        let (b, n) = (txt(base, p), txt(new, p));
        let f = |kind: &str, detail: String| Finding { kind: kind.into(), path: p.clone(), detail };
        match (&b, &n) {
            (Some(_), None) if is_test(p) => { out.push(f("TEST_DELETED", "test file removed".into())); continue; }
            (_, None) => continue,
            _ => {}
        }
        let (b, n) = (b.unwrap_or_default(), n.unwrap_or_default());
        if is_test(p) || n.contains("#[test]") || n.contains("#[cfg(test)]") || (crate::c_cpp::lang_of(p).is_some() && c_test_code(&b)) {
            let (ab, an) = (count(&b, ASSERTS), count(&n, ASSERTS));
            if an < ab { out.push(f("ASSERTIONS_REMOVED", format!("{} fewer assertion(s) ({ab} → {an})", ab - an))); }
            let ((strong_b, weak_b), (strong_n, weak_n)) = (assertion_strength(&b), assertion_strength(&n));
            if strong_n < strong_b && weak_n > weak_b {
                let k = (strong_b - strong_n).min(weak_n - weak_b);
                out.push(f("ASSERTIONS_WEAKENED", format!("{k} value check(s) replaced by weaker ones that only check truth, presence or no error ({strong_b} → {strong_n} value checks)")));
            }
            let (sb, sn) = (count(&b, SKIPS) + tests_under_if0(&b), count(&n, SKIPS) + tests_under_if0(&n));
            if sn > sb { out.push(f("TEST_SKIPPED", format!("{} new skip/ignore/only marker(s)", sn - sb))); }
        }
        let fname = file(p);
        if fname == "CMakeLists.txt" || fname.ends_with(".cmake") {
            let (rb, rn) = (count(&b, CTEST), count(&n, CTEST));
            if rn < rb { out.push(f("TEST_DELETED", format!("{} CTest registration(s) removed ({rb} → {rn})", rb - rn))); }
        }
        if is_test(p) || is_test_runner_config(p) {
            let (xb, xn) = (count(&b, TEST_EXITS), count(&n, TEST_EXITS));
            if xn > xb { out.push(f("TEST_EXIT", format!("{} new call(s) that end or skip the test run from inside the tests", xn - xb))); }
        }
        if is_test_runner_config(p) && !b.is_empty() {
            // Only lines that change which tests run or whether a failure counts, added or taken out.
            let (old, new): (std::collections::BTreeSet<&str>, std::collections::BTreeSet<&str>) = (b.lines().map(str::trim).collect(), n.lines().map(str::trim).collect());
            let hits: Vec<&str> = new.symmetric_difference(&old).copied().filter(|l| RUNNER_SELECTORS.iter().any(|k| l.contains(k))).collect();
            if !hits.is_empty() { out.push(f("TEST_RUNNER_CHANGED", format!("changed which tests the runner collects or counts ({})", hits.iter().take(2).cloned().collect::<Vec<_>>().join("; ")))); }
        }
        let (eb, en) = (always_equal(&b), always_equal(&n));
        if en > eb { out.push(f("ALWAYS_EQUAL", format!("{} new equality method(s) that answer true for everything", en - eb))); }
        let (qb, qn) = (count(&b, SILENCERS), count(&n, SILENCERS));
        if qn > qb { out.push(f("CHECK_SILENCED", format!("{} new suppression comment(s)", qn - qb))); }
        if is_ci(p) {
            let (tb, tn) = (count(&b, CI_TOLERATE), count(&n, CI_TOLERATE));
            if tn > tb { out.push(f("CI_WEAKENED", "CI now tolerates failures (continue-on-error, || true, allow_failure ...)".into())); }
            let checks = |t: &str| t.lines().filter(|l| { let l = l.to_lowercase(); (l.contains("run:") || l.contains("script:") || l.contains("- ") || l.contains("uses:")) && CI_CHECKS.iter().any(|c| l.contains(c)) }).count();
            let (cb, cn) = (checks(&b), checks(&n));
            if cn < cb { out.push(f("CI_WEAKENED", format!("{} test/lint/scan step(s) removed", cb - cn))); }
            for d in ci_structure(p, &b, &n) { out.push(f("CI_WEAKENED", d)); }
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

/// `if: false` and the forms that mean the same (`if: ${{ false }}`, `if: 0`): a job or step that never runs.
fn if_false(t: &str) -> usize {
    t.lines().filter(|l| {
        let s: String = l.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase().replace('\'', "\"");
        let s = s.trim_start_matches('-');
        ["if:false", "if:${{false}}", "if:0", "if:\"false\"", "if:${{0}}"].contains(&s)
    }).count()
}

fn indent(l: &str) -> usize { l.len() - l.trim_start().len() }

/// The jobs of a GitHub Actions workflow (keys under `jobs:`) or a GitLab CI file (top-level keys with a script).
pub(crate) fn ci_jobs(p: &str, t: &str) -> Vec<String> {
    let lines: Vec<&str> = t.lines().filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#')).collect();
    let key = |l: &str| l.trim().strip_suffix(':').map(|k| k.trim_matches(['"', '\'']).to_string()).filter(|k| !k.contains(' ') || k.starts_with('.'));
    let mut out = vec![];
    if p.starts_with(".github/workflows/") {
        let Some(j) = lines.iter().position(|l| indent(l) == 0 && l.trim() == "jobs:") else { return out };
        let child = lines.get(j + 1).map(|l| indent(l)).unwrap_or(2);
        for l in &lines[j + 1..] {
            if indent(l) == 0 { break; }
            if indent(l) == child { if let Some(k) = key(l) { out.push(k); } }
        }
    } else if p.ends_with(".gitlab-ci.yml") {
        const RESERVED: &[&str] = &["stages", "variables", "default", "include", "workflow", "image", "services", "before_script", "after_script", "cache"];
        for (i, l) in lines.iter().enumerate() {
            if indent(l) != 0 { continue; }
            let Some(k) = key(l) else { continue };
            if RESERVED.contains(&k.as_str()) || k.starts_with('.') { continue; }
            if lines[i + 1..].iter().take_while(|x| indent(x) > 0).any(|x| { let t = x.trim(); t.starts_with("script:") || t.starts_with("trigger:") || t.starts_with("extends:") }) { out.push(k); }
        }
    }
    out
}

/// What a CI change takes away beyond failures tolerated and checks removed: a job or step switched off with
/// `if: false`, or a job removed.
pub fn ci_structure(p: &str, old: &str, new: &str) -> Vec<String> {
    let mut out = vec![];
    let (fo, fn_) = (if_false(old), if_false(new));
    if fn_ > fo { out.push(format!("{} job(s) or step(s) switched off with `if: false`", fn_ - fo)); }
    let now = ci_jobs(p, new);
    let gone: Vec<String> = ci_jobs(p, old).into_iter().filter(|j| !now.contains(j)).collect();
    if !gone.is_empty() && !new.is_empty() { out.push(format!("removed CI job(s) {}", gone.join(", "))); }
    out
}

/// The trailer gitta's commit-msg hook writes. `git commit --no-verify` skips the commit-msg hook together with the
/// pre-commit hook, so a commit without it did not run the hooks.
pub const HOOK_TRAILER: &str = "Gitta-Hooks: ran";

/// The repository configures pre-commit hooks of its own: the committed configuration a server or CI can see.
pub fn hooks_configured(paths: &[String]) -> bool {
    paths.iter().any(|p| matches!(p.as_str(), ".pre-commit-config.yaml" | ".pre-commit-config.yml" | ".husky/pre-commit" | "lefthook.yml" | "lefthook.yaml" | ".lefthook.yml" | ".githooks/pre-commit" | ".overcommit.yml"))
}

/// Commits that reached the server without the trace the hooks write, while the repository configures hooks and
/// its history shows the trace is in use. `commits`: (sha, message, number of parents). Merge commits and
/// commits gitta made are left out.
pub fn hook_bypass(commits: &[(String, String, usize)], configured: bool, trace_in_use: bool) -> Vec<Finding> {
    if !configured || !(trace_in_use || commits.iter().any(|c| c.1.contains(HOOK_TRAILER))) { return vec![]; }
    let missed: Vec<&str> = commits.iter().filter(|(_, m, parents)| *parents <= 1 && !m.contains(HOOK_TRAILER) && !m.starts_with("gitta:")).map(|(s, _, _)| &s[..s.len().min(10)]).collect();
    if missed.is_empty() { return vec![]; }
    vec![Finding { kind: "CI_WEAKENED".into(), path: "(commits)".into(), detail: format!("{} commit(s) skipped the repository's pre-commit hooks (no {HOOK_TRAILER} trailer, as after git commit --no-verify): {}", missed.len(), missed.join(", ")) }]
}

/// Strong and weak assertions in test text: (strong, weak). A strong one compares a value (assertEqual, toEqual,
/// assert x == y, EXPECT_EQ, assert.Equal ...); a weak one checks only truth, presence or that nothing failed
/// (assertTrue(x), `is not None`, toBeTruthy, toBeDefined, assertNotNull, assert!(x), require.NoError ...).
pub fn assertion_strength(t: &str) -> (usize, usize) {
    const STRONG: &[&str] = &["assertEqual(", "assertEquals(", "assertNotEqual(", "assertListEqual(", "assertDictEqual(", "assertCountEqual(", "assertAlmostEqual(", "assertIn(", "assertRaises(", "assert_eq!(", "assert_ne!(", "assert_matches!(",
        "isEqualTo(", "containsExactly(", "toEqual(", "toStrictEqual(", "toBe(", "toHaveLength(", "toContain(", "toThrow(", "toMatchObject(", "toHaveBeenCalledWith(", "EXPECT_EQ(", "ASSERT_EQ(", "EXPECT_STREQ(", "ASSERT_STREQ(", "REQUIRE(", "CHECK_EQ(",
        "assert.Equal(", "require.Equal(", "assert.EqualValues(", "assert.ElementsMatch(", "assert.equal(", "assert.strictEqual(", "assert.deepEqual(", "assert.deepStrictEqual(", "assert.throws(", "Assert.AreEqual(", "Assert.Equal(", "Assert.Throws", "assertThrows(", "pytest.raises(", "assertArrayEquals(", "should.equal(", ".should.eql(", "expect(err).To(Equal", "Equal("];
    const WEAK: &[&str] = &["assertTrue(", "assertFalse(", "assertIsNotNone(", "assertIsNone(", "assertNotNull(", "assertNull(", "is not None", "toBeTruthy()", "toBeFalsy()", "toBeDefined()", "not.toBeNull()", "not.toBeUndefined()", "assert.ok(",
        "assert.NotNil(", "require.NotNil(", "assert.NoError(", "require.NoError(", "assert.True(", "require.True(", "Assert.IsNotNull(", "Assert.NotNull(", "Assert.True(", "Assert.IsTrue(", "EXPECT_TRUE(", "ASSERT_TRUE(", ".should.exist", "assertNotNull("];
    let (mut s, mut w) = (0, 0);
    for l in t.lines() {
        let x = l.trim();
        if x.starts_with("//") || x.starts_with('#') && !x.starts_with("#[") { continue; }
        if STRONG.iter().any(|m| x.contains(m)) || (x.starts_with("assert ") && ["==", "!=", " in ", "<", ">"].iter().any(|o| x.contains(o)) && !x.contains("is not None")) || (x.starts_with("assert!(") && (x.contains("==") || x.contains("!="))) { s += 1; }
        else if WEAK.iter().any(|m| x.contains(m)) || x.starts_with("assert ") || x.starts_with("assert!(") || (x.starts_with("expect(") && x.ends_with(");") && x.matches('(').count() == 1) { w += 1; }
    }
    (s, w)
}

#[cfg(test)]
mod feature_visibility_tests {
    use super::*;

    // The compiled-in feature set must be observable so a build without an optional feature is not mistaken for a
    // clean verdict from a full build. `features()` reflects exactly what was compiled in, nothing more.
    #[test]
    fn features_reflects_the_build() {
        let f = features();
        assert_eq!(f.contains(&"mainframe"), cfg!(feature = "mainframe"), "mainframe presence must match the build");
        assert_eq!(f.contains(&"abap"), cfg!(feature = "abap"), "abap presence must match the build");
        for (name, on) in [("ibmi", cfg!(feature = "ibmi")), ("fortran", cfg!(feature = "fortran")), ("sql", cfg!(feature = "sql")),
                           ("iac", cfg!(feature = "iac")), ("network", cfg!(feature = "network")), ("iso20022", cfg!(feature = "iso20022"))] {
            assert_eq!(f.contains(&name), on, "{name} presence must match the build");
        }
        // Only the known optional features ever appear.
        assert!(f.iter().all(|x| ["mainframe", "abap", "ibmi", "fortran", "sql", "iac", "network", "iso20022"].contains(x)));
    }
}

/// How many tests a gate run executed, read from the summary lines test runners print: pytest, unittest, Jest,
/// Vitest, Mocha, cargo test, `go test -v`, Maven/Gradle (JUnit) and .NET. Skipped tests do not count: they did not
/// run. None when no summary was recognised (an unknown runner is never reported as a drop).
pub fn tests_ran(output: &str) -> Option<u64> {
    let num = |s: &str, after: &str| -> Option<u64> {
        let i = s.find(after)?;
        s[..i].trim_end().rsplit(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
    };
    let field = |s: &str, key: &str| -> Option<u64> {
        let i = s.find(key)? + key.len();
        s[i..].trim_start().chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().ok()
    };
    let (mut total, mut seen) = (0u64, false);
    let mut maven: Option<u64> = None;
    let mut go = 0u64;
    let mut junit: Option<u64> = None;
    for l in output.lines() {
        let t = l.trim();
        // go test -json: one event per finished test (package events carry no "Test").
        if t.starts_with('{') && t.contains("\"Test\":") && (t.contains("\"Action\":\"pass\"") || t.contains("\"Action\":\"fail\"")) { go += 1; continue; }
        // JUnit XML: the <testsuite> elements; <testsuites> totals are left out so nothing is counted twice.
        for el in t.split('<').skip(1).filter(|e| e.starts_with("testsuite ")) {
            let attr = |k: &str| -> u64 { el.split(&format!(" {k}=\"")).nth(1).and_then(|r| r.split('"').next()).and_then(|v| v.parse().ok()).unwrap_or(0) };
            *junit.get_or_insert(0) += attr("tests").saturating_sub(attr("skipped") + attr("disabled"));
        }
        // cargo test: one summary per test binary, summed.
        if t.starts_with("test result:") {
            total += num(t, " passed").unwrap_or(0) + num(t, " failed").unwrap_or(0); seen = true; continue;
        }
        // pytest when nothing was collected or the run ended before the tests (an exit at import): zero ran.
        if t.contains("no tests ran") { seen = true; continue; }
        // pytest: "=== 12 passed, 1 failed, 2 skipped in 0.3s ===".
        if t.starts_with('=') && t.contains(" in ") && (t.contains(" passed") || t.contains(" failed") || t.contains(" error")) {
            total += num(t, " passed").unwrap_or(0) + num(t, " failed").unwrap_or(0) + num(t, " error").unwrap_or(0) + num(t, " xpassed").unwrap_or(0);
            seen = true; continue;
        }
        if t.starts_with("Ran ") && t.contains(" test") { if let Some(n) = field(t, "Ran ") { total += n; seen = true; } continue; }
        // Jest: "Tests:       1 failed, 12 passed, 13 total"; Vitest: "Tests  12 passed (12)".
        if t.starts_with("Tests:") || (t.starts_with("Tests ") && t.contains(" passed")) {
            total += num(t, " passed").unwrap_or(0) + num(t, " failed").unwrap_or(0); seen = true; continue;
        }
        // Mocha: "12 passing", "1 failing".
        if t.ends_with(" passing") || t.contains(" passing (") { total += num(t, " passing").unwrap_or(0); seen = true; continue; }
        if t.ends_with(" failing") { total += num(t, " failing").unwrap_or(0); seen = true; continue; }
        // Maven / Gradle (JUnit): the last "Tests run:" line is the total.
        if t.contains("Tests run:") && t.contains("Failures:") {
            let run = field(t, "Tests run:").unwrap_or(0);
            maven = Some(run.saturating_sub(field(t, "Skipped:").unwrap_or(0)));
            continue;
        }
        // .NET: "Passed!  - Failed: 0, Passed: 12, Skipped: 0, Total: 12".
        if t.contains("Total:") && t.contains("Passed:") && t.contains("Failed:") {
            total += field(t, "Passed:").unwrap_or(0) + field(t, "Failed:").unwrap_or(0); seen = true; continue;
        }
        // go test -v: one line per finished test.
        if t.starts_with("--- PASS:") || t.starts_with("--- FAIL:") { go += 1; }
    }
    if let Some(m) = maven { total += m; seen = true; }
    if go > 0 { total += go; seen = true; }
    if let Some(j) = junit { total += j; seen = true; }
    seen.then_some(total)
}

/// Each test's name and result ("passed", "failed" or "skipped") as the runner printed it: cargo test, pytest -v,
/// go test -v and -json, JUnit XML. Kept as evidence of what the gate ran; empty when the runner prints no names.
pub fn test_results(output: &str) -> Vec<(String, String)> {
    let mut r = vec![];
    let status = |s: &str| match s.to_ascii_lowercase().as_str() { "ok" | "pass" | "passed" => Some("passed"), "failed" | "fail" | "error" => Some("failed"), "ignored" | "skip" | "skipped" | "xfail" => Some("skipped"), _ => None };
    let json_str = |t: &str, k: &str| t.split(&format!("\"{k}\":\"")).nth(1).and_then(|x| x.split('"').next()).map(String::from);
    let mut in_case: Option<String> = None;
    for l in output.lines() {
        let t = l.trim();
        if let Some(x) = t.strip_prefix("test ").and_then(|x| x.rsplit_once(" ... ")) {
            if let Some(s) = status(x.1.split_whitespace().next().unwrap_or("")) { r.push((x.0.to_string(), s.to_string())); }
        } else if let Some(x) = t.strip_prefix("--- PASS: ").or(t.strip_prefix("--- FAIL: ")).or(t.strip_prefix("--- SKIP: ")) {
            r.push((x.split_whitespace().next().unwrap_or("").to_string(), status(&t[4..8]).unwrap_or("failed").to_string()));
        } else if t.starts_with('{') && t.contains("\"Test\":") {
            if let (Some(a), Some(n)) = (json_str(t, "Action"), json_str(t, "Test")) { if let Some(s) = status(&a) { r.push((n, s.to_string())); } }
        } else if t.contains("::") && !t.starts_with('=') && t.split_whitespace().count() >= 2 {
            let mut w = t.split_whitespace();
            let (n, s) = (w.next().unwrap_or(""), w.next().unwrap_or(""));
            if let Some(s) = status(s) { r.push((n.to_string(), s.to_string())); }
        }
        for el in t.split('<').skip(1) {
            if let Some(c) = el.strip_prefix("testcase ") {
                let attr = |k: &str| format!(" {c}").split(&format!(" {k}=\"")).nth(1).and_then(|r| r.split('"').next()).unwrap_or("").to_string();
                let n = if attr("classname").is_empty() { attr("name") } else { format!("{}.{}", attr("classname"), attr("name")) };
                if c.trim_end().ends_with("/>") { r.push((n, "passed".into())); } else { in_case = Some(n); }
            } else if let Some(n) = in_case.as_ref().filter(|_| el.starts_with("failure") || el.starts_with("error") || el.starts_with("skipped")) {
                r.push((n.clone(), if el.starts_with("skipped") { "skipped" } else { "failed" }.into())); in_case = None;
            } else if el.starts_with("/testcase") {
                if let Some(n) = in_case.take() { r.push((n, "passed".into())); }
            }
        }
    }
    r
}

/// Whether a test run passed, read from the same summaries as `tests_ran`: Some(true) when tests ran and none
/// failed, Some(false) when any failed, None when no summary was recognised or nothing ran.
pub fn tests_passed(output: &str) -> Option<bool> {
    let n = tests_ran(output)?;
    let failed = output.lines().map(str::trim).any(|t| {
        let count = |after: &str| t.find(after).and_then(|i| t[..i].trim_end().rsplit(|c: char| !c.is_ascii_digit()).next()?.parse::<u64>().ok()).unwrap_or(0);
        t.starts_with("test result: FAILED") || t.starts_with("FAILED (") || t.starts_with("--- FAIL:") || t.starts_with("FAIL\t") || t.starts_with("Failed!")
            || (t.starts_with('=') && (count(" failed") > 0 || count(" error") > 0))
            || (t.starts_with("Tests:") && count(" failed") > 0)
            || (t.ends_with(" failing") && count(" failing") > 0)
            || (t.contains("Tests run:") && t.contains("Failures:") && (t.contains("Failures: ") && !t.contains("Failures: 0") || t.contains("Errors: ") && !t.contains("Errors: 0")))
    });
    if failed { Some(false) } else if n > 0 { Some(true) } else { None }
}
