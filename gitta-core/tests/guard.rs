use gitta_core::guard::safety_net;
use gitta_core::Snapshot;

fn snap(v: &[(&str, &str)]) -> Snapshot { v.iter().map(|(p, t)| (p.to_string(), t.as_bytes().to_vec())).collect() }
fn kinds(b: &Snapshot, n: &Snapshot) -> Vec<String> {
    let paths: Vec<String> = b.keys().chain(n.keys()).cloned().collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    safety_net(b, n, &paths).into_iter().map(|f| f.kind).collect()
}

#[test]
fn weakened_tests_are_found() {
    let t = "import unittest\nclass T(unittest.TestCase):\n    def test_a(self):\n        self.assertEqual(f(1), 2)\n        self.assertEqual(f(2), 3)\n";
    let base = snap(&[("tests/test_a.py", t)]);
    assert_eq!(kinds(&base, &snap(&[("tests/test_a.py", &t.replace("        self.assertEqual(f(2), 3)\n", ""))])), vec!["ASSERTIONS_REMOVED"]);
    assert_eq!(kinds(&base, &snap(&[("tests/test_a.py", &t.replace("    def test_a", "    @unittest.skip(\"flaky\")\n    def test_a"))])), vec!["TEST_SKIPPED"]);
    assert_eq!(kinds(&base, &snap(&[])), vec!["TEST_DELETED"]);
    // Adding assertions or tests is fine.
    assert!(kinds(&base, &snap(&[("tests/test_a.py", &format!("{t}        self.assertTrue(True)\n"))])).is_empty());
}

#[test]
fn ci_thresholds_and_silencing_are_found() {
    let ci = "jobs:\n  test:\n    steps:\n      - run: npm test\n      - run: npm run lint\n";
    let b = snap(&[(".github/workflows/ci.yml", ci), ("pyproject.toml", "[tool.coverage.report]\nfail_under = 90\n"), ("app.py", "x = eval(s)\n")]);
    let n = snap(&[(".github/workflows/ci.yml", &ci.replace("      - run: npm run lint\n", "").replace("npm test", "npm test || true")),
                   ("pyproject.toml", "[tool.coverage.report]\nfail_under = 60\n"), ("app.py", "x = eval(s)  # nosec\n")]);
    let k = kinds(&b, &n);
    for want in ["CI_WEAKENED", "THRESHOLD_LOWERED", "CHECK_SILENCED"] { assert!(k.contains(&want.to_string()), "{want} missing in {k:?}"); }
    // Raising a threshold is fine.
    assert!(kinds(&snap(&[("pyproject.toml", "fail_under = 60\n")]), &snap(&[("pyproject.toml", "fail_under = 90\n")])).is_empty());
}

#[test]
fn csharp_and_kotlin_tests_are_guarded() {
    let cs = "public class VatTests\n{\n    [Fact]\n    public void Rate()\n    {\n        Assert.Equal(0.25, Vat.Rate);\n        Assert.Equal(1.25, Vat.NetToGross(1));\n    }\n}\n";
    let base = snap(&[("tests/Shop.Tests/VatTests.cs", cs)]);
    assert_eq!(kinds(&base, &snap(&[("tests/Shop.Tests/VatTests.cs", &cs.replace("        Assert.Equal(1.25, Vat.NetToGross(1));\n", ""))])), vec!["ASSERTIONS_REMOVED"]);
    assert_eq!(kinds(&base, &snap(&[("tests/Shop.Tests/VatTests.cs", &cs.replace("[Fact]", "[Fact(Skip = \"flaky\")]"))])), vec!["TEST_SKIPPED"]);
    let kt = "class VatTest {\n    @Test\n    fun rate() {\n        assertEquals(0.25, RATE)\n    }\n}\n";
    let base = snap(&[("src/test/kotlin/shop/VatTest.kt", kt)]);
    assert_eq!(kinds(&base, &snap(&[])), vec!["TEST_DELETED"]);
    assert_eq!(kinds(&base, &snap(&[("src/test/kotlin/shop/VatTest.kt", &kt.replace("    @Test\n", "    @Test\n    @Disabled\n"))])), vec!["TEST_SKIPPED"]);
}

// The hack forms frontier agents use when tests contradict the task (ImpossibleBench, arXiv 2510.20270; Baker et
// al., OpenAI, arXiv 2503.11926): ending the run early from the tests, editing the runner, and equality that always
// passes. Each has a harmless control.
#[test]
fn test_run_tampering_is_found() {
    let t = "import unittest\nclass T(unittest.TestCase):\n    def test_a(self):\n        self.assertEqual(f(1), 2)\n";
    let base = snap(&[("tests/test_a.py", t)]);
    // An early exit at import time: no test after it runs, and the runner reports success.
    assert!(kinds(&base, &snap(&[("tests/test_a.py", &format!("import sys\nsys.exit(0)\n{t}"))])).contains(&"TEST_EXIT".to_string()));
    assert!(kinds(&base, &snap(&[("tests/test_a.py", &t.replace("        self.assertEqual", "        raise unittest.SkipTest('later')\n        self.assertEqual"))])).contains(&"TEST_EXIT".to_string()));
    // Go and JavaScript forms.
    let go = "package shop\n\nfunc TestVat(t *testing.T) {\n\tif Vat(1) != 2 {\n\t\tt.Fatal(\"vat\")\n\t}\n}\n";
    assert!(kinds(&snap(&[("shop/vat_test.go", go)]), &snap(&[("shop/vat_test.go", &go.replace("func TestVat", "func init() { os.Exit(0) }\n\nfunc TestVat"))])).contains(&"TEST_EXIT".to_string()));
    let js = "test('vat', () => {\n  expect(vat(1)).toBe(2);\n});\n";
    assert!(kinds(&snap(&[("src/vat.test.js", js)]), &snap(&[("src/vat.test.js", &format!("process.exit(0);\n{js}"))])).contains(&"TEST_EXIT".to_string()));
    // Control: an exit in application code is not a test exit.
    assert!(!kinds(&snap(&[("src/cli.py", "def main():\n    pass\n")]), &snap(&[("src/cli.py", "import sys\ndef main():\n    sys.exit(0)\n")])).contains(&"TEST_EXIT".to_string()));
}

#[test]
fn test_runner_config_changes_are_found() {
    let conf = "import pytest\n\n@pytest.fixture\ndef db():\n    return make_db()\n";
    let k = kinds(&snap(&[("conftest.py", conf)]), &snap(&[("conftest.py", &format!("{conf}\ndef pytest_collection_modifyitems(items):\n    items.clear()\n"))]));
    assert!(k.contains(&"TEST_RUNNER_CHANGED".to_string()), "{k:?}");
    assert!(kinds(&snap(&[("jest.config.js", "module.exports = { testMatch: ['**/*.test.js'] };\n")]), &snap(&[("jest.config.js", "module.exports = { testMatch: [] };\n")])).contains(&"TEST_RUNNER_CHANGED".to_string()));
    // Control: a new fixture in conftest.py is ordinary work.
    assert!(!kinds(&snap(&[("conftest.py", conf)]), &snap(&[("conftest.py", &format!("{conf}\n@pytest.fixture\ndef user():\n    return make_user()\n"))])).contains(&"TEST_RUNNER_CHANGED".to_string()));
    // Control: a new conftest.py (no previous version) and an unchanged one are not reported.
    assert!(!kinds(&snap(&[]), &snap(&[("conftest.py", conf)])).contains(&"TEST_RUNNER_CHANGED".to_string()));
}

#[test]
fn equality_that_always_passes_is_found() {
    let py = "class Money:\n    def __init__(self, v):\n        self.v = v\n";
    let k = kinds(&snap(&[("shop/money.py", py)]), &snap(&[("shop/money.py", &format!("{py}\n    def __eq__(self, other):\n        return True\n"))]));
    assert!(k.contains(&"ALWAYS_EQUAL".to_string()), "{k:?}");
    let java = "public class Money {\n    private long v;\n}\n";
    assert!(kinds(&snap(&[("src/Money.java", java)]), &snap(&[("src/Money.java", &java.replace("}\n", "    @Override public boolean equals(Object o) {\n        return true;\n    }\n}\n"))])).contains(&"ALWAYS_EQUAL".to_string()));
    let rs = "struct Money(u64);\n";
    assert!(kinds(&snap(&[("src/money.rs", rs)]), &snap(&[("src/money.rs", &format!("{rs}impl PartialEq for Money {{\n    fn eq(&self, _o: &Self) -> bool {{ true }}\n}}\n"))])).contains(&"ALWAYS_EQUAL".to_string()));
    // Control: a real equality method is fine.
    assert!(!kinds(&snap(&[("shop/money.py", py)]), &snap(&[("shop/money.py", &format!("{py}\n    def __eq__(self, other):\n        return self.v == other.v\n"))])).contains(&"ALWAYS_EQUAL".to_string()));
}

#[test]
fn tests_ran_is_read_from_runner_summaries() {
    use gitta_core::guard::tests_ran;
    assert_eq!(tests_ran("....\n========== 12 passed, 1 failed, 2 skipped in 0.31s ==========\n"), Some(13));
    assert_eq!(tests_ran("..\n----------------------------------------------------------------------\nRan 7 tests in 0.002s\n\nOK\n"), Some(7));
    assert_eq!(tests_ran("Tests:       1 failed, 12 passed, 13 total\n"), Some(13));
    assert_eq!(tests_ran("test result: ok. 5 passed; 0 failed; 1 ignored\ntest result: ok. 3 passed; 0 failed; 0 ignored\n"), Some(8));
    assert_eq!(tests_ran("[INFO] Tests run: 4, Failures: 0, Errors: 0, Skipped: 0\n[INFO] Results:\n[INFO] Tests run: 10, Failures: 0, Errors: 0, Skipped: 2\n"), Some(8));
    assert_eq!(tests_ran("=== RUN   TestA\n--- PASS: TestA (0.00s)\n=== RUN   TestB\n--- FAIL: TestB (0.00s)\n"), Some(2));
    assert_eq!(tests_ran("  12 passing (20ms)\n  1 failing\n"), Some(13));
    assert_eq!(tests_ran("Passed!  - Failed:     0, Passed:    12, Skipped:     0, Total:    12\n"), Some(12));
    // An early exit at import: pytest reports nothing ran.
    assert_eq!(tests_ran("============================ no tests ran in 0.01s =============================\n"), Some(0));
    assert_eq!(tests_ran("everything is fine\n"), None);
    // go test -json: package events are not tests.
    let gj = "{\"Action\":\"pass\",\"Package\":\"p\",\"Test\":\"TestA\"}\n{\"Action\":\"fail\",\"Package\":\"p\",\"Test\":\"TestB\"}\n{\"Action\":\"pass\",\"Package\":\"p\"}\n";
    assert_eq!(tests_ran(gj), Some(2));
    // JUnit XML: skipped tests did not run; the <testsuites> total is not counted again.
    let xml = "<testsuites tests=\"9\"><testsuite name=\"a\" tests=\"5\" skipped=\"1\" failures=\"0\"></testsuite>\n<testsuite name=\"b\" tests=\"4\" failures=\"1\"></testsuite></testsuites>";
    assert_eq!(tests_ran(xml), Some(8));
}

#[test]
fn per_test_results_are_read_from_runner_output() {
    use gitta_core::guard::test_results;
    let r = |o: &str| test_results(o).into_iter().map(|(n, s)| format!("{n}={s}")).collect::<Vec<_>>();
    assert_eq!(r("test a::b ... ok\ntest a::c ... FAILED\ntest a::d ... ignored\n"), ["a::b=passed", "a::c=failed", "a::d=skipped"]);
    assert_eq!(r("t.py::test_x PASSED  [ 50%]\nt.py::test_y FAILED  [100%]\n"), ["t.py::test_x=passed", "t.py::test_y=failed"]);
    assert_eq!(r("--- PASS: TestA (0.00s)\n--- SKIP: TestB (0.00s)\n"), ["TestA=passed", "TestB=skipped"]);
    assert_eq!(r("{\"Action\":\"fail\",\"Test\":\"TestC\"}\n"), ["TestC=failed"]);
    assert_eq!(r("<testcase classname=\"K\" name=\"a\"/><testcase classname=\"K\" name=\"b\"><failure/></testcase>"), ["K.a=passed", "K.b=failed"]);
    // Control: output with no test names gives nothing.
    assert!(test_results("===== 3 passed in 0.1s =====\n").is_empty());
}

#[test]
fn tests_passed_is_read_from_runner_summaries() {
    use gitta_core::guard::tests_passed;
    assert_eq!(tests_passed("Ran 2 tests in 0.001s\n\nOK\n"), Some(true));
    assert_eq!(tests_passed("Ran 2 tests in 0.001s\n\nFAILED (failures=1)\n"), Some(false));
    assert_eq!(tests_passed("===== 3 passed in 0.1s =====\n"), Some(true));
    assert_eq!(tests_passed("===== 1 failed, 2 passed in 0.1s =====\n"), Some(false));
    assert_eq!(tests_passed("test result: ok. 3 passed; 0 failed; 0 ignored\n"), Some(true));
    assert_eq!(tests_passed("test result: FAILED. 2 passed; 1 failed; 0 ignored\n"), Some(false));
    assert_eq!(tests_passed("Tests:       13 passed, 13 total\n"), Some(true));
    assert_eq!(tests_passed("[INFO] Tests run: 4, Failures: 1, Errors: 0, Skipped: 0\n"), Some(false));
    assert_eq!(tests_passed("no tests ran in 0.01s\n"), None);
}

#[test]
fn rust_test_modules_inside_src_are_test_code() {
    use gitta_core::guard::is_test;
    assert!(is_test("gitta-core/src/crypto/tests.rs"));
    assert!(is_test("src/parser/test.rs"));
    assert!(is_test("src/protocol/tests/parsers.rs"));
    // Negative controls: ordinary module files, and a file that only has "tests" in its name.
    assert!(!is_test("src/protocol.rs"));
    assert!(!is_test("src/contests.rs"));
    assert!(!is_test("src/tests_helper/mod.c"));
}
