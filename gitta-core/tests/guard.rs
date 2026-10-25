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

#[test]
fn credential_names_inside_strings_are_not_credentials() {
    use gitta_core::guard::find_secret;
    // flynt test/test_edits.py at the scenario's base commit: a format string that names a password field.
    let flynt = "def test_110(state: State):\n    \"\"\"Test for issue #110 on github\"\"\"\n    s_in = \"'{conn.login}:{conn.password}@'.format(conn=x)\"\n    expected_out = \"f'{x.login}:{x.password}@'\"\n";
    assert_eq!(find_secret(flynt), None);
    for ok in ["if password == 'correct horse battery':", "url = f\"{user}:{password}@host\"", "PASSWORD = os.environ['DB_PASSWORD_VALUE']",
               "password: \"${DB_PASSWORD_FROM_VAULT}\"", "api_key = \"<your-api-key-here>\"", "log(\"password_reset_template_sent\")"] {
        assert_eq!(find_secret(ok), None, "{ok}");
    }
}

#[test]
fn real_credentials_are_still_found() {
    use gitta_core::guard::{find_secret, find_secret_at};
    for bad in ["API_KEY = \"sk-live-4f9a8b7c6d5e4f3a2b1c0d9e8f7a6b5c\"", "db_password = 'Tr0ub4dor&3xyz'", "{\"password\": \"s3cr3tPassw0rd9\"}",
                "  auth_token: 'abcd1234efgh5678'", "$config['secret_key'] => 'q8w7e6r5t4y3u2i1';", "connect(password=\"hunter2hunter2\")",
                "token = \"ghp_0123456789abcdefghijABCDEFGHIJ0123\"", "-----BEGIN OPENSSH PRIVATE KEY-----"] {
        assert!(find_secret(bad).is_some(), "{bad}");
    }
    assert_eq!(find_secret_at("x = 1\n\nPASSWORD = \"Zx9q-Lm2w-Pp7r\"\n"), Some((3, "hard-coded credential".into())));
}

#[test]
fn letters_that_grow_when_lowercased_do_not_crash_the_check() {
    use gitta_core::guard::find_secret;
    // `İ` is 2 bytes, and 3 once lowercased: a position in the lowercased line is not one in the line.
    for ok in ["pİ = 1  # password", "İİİ_password", "# İ password"] { assert_eq!(find_secret(ok), None, "{ok}"); }
    assert!(find_secret("İpassword = 'Tr0ub4dor&3xyz'").is_some());
}
