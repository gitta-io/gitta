// Coverage of the lines a change touched, from the report formats teams' test runs write.
use gitta_core::proof::{change_coverage, changed_lines, parse};

const BASE: &str = "def vat(x):\n    return x * 1.25\n\n\ndef label(x):\n    return str(x)\n";
const NEW: &str = "def vat(x):\n    if x < 0:\n        raise ValueError(x)\n    return x * 1.25\n\n\ndef label(x):\n    return str(x)\n";

#[test]
fn changed_lines_are_the_added_and_rewritten_ones() {
    assert_eq!(changed_lines(BASE, NEW).into_iter().collect::<Vec<_>>(), vec![2, 3]);
    assert_eq!(changed_lines(BASE, &BASE.replace("str(x)", "f\"{x}\"")).into_iter().collect::<Vec<_>>(), vec![6]);
    assert!(changed_lines(BASE, BASE).is_empty());
}

fn files() -> Vec<(String, String, String)> { vec![("shop/vat.py".into(), BASE.into(), NEW.into())] }

#[test]
fn lcov_report() {
    let cov = parse("TN:\nSF:/ci/build/shop/vat.py\nDA:1,1\nDA:2,4\nDA:3,0\nDA:4,4\nend_of_record\n").unwrap();
    let c = change_coverage(&cov, &files());
    assert_eq!(c.executable, 2);
    assert_eq!(c.uncovered.get("shop/vat.py"), Some(&vec![3]));
    assert_eq!(c.covered_pct(), 50.0);
}

#[test]
fn cobertura_report() {
    let x = r#"<?xml version="1.0" ?><coverage><packages><package name="shop"><classes><class name="vat.py" filename="shop/vat.py"><lines><line number="2" hits="3"/><line number="3" hits="1"/></lines></class></classes></package></packages></coverage>"#;
    let c = change_coverage(&parse(x).unwrap(), &files());
    assert_eq!((c.executable, c.uncovered.len()), (2, 0));
}

#[test]
fn jacoco_report() {
    let java_base = "class Vat {\n  long vat(long x) {\n    return x * 125 / 100;\n  }\n}\n";
    let java_new = "class Vat {\n  long vat(long x) {\n    if (x < 0) throw new IllegalArgumentException();\n    return x * 125 / 100;\n  }\n}\n";
    let x = r#"<report name="shop"><package name="com/shop"><sourcefile name="Vat.java"><line nr="3" mi="2" ci="0" mb="0" cb="0"/><line nr="4" mi="0" ci="3" mb="0" cb="0"/></sourcefile></package></report>"#;
    let c = change_coverage(&parse(x).unwrap(), &[("src/main/java/com/shop/Vat.java".into(), java_base.into(), java_new.into())]);
    assert_eq!(c.uncovered.get("src/main/java/com/shop/Vat.java"), Some(&vec![3]));
}

#[test]
fn coverage_py_json_report() {
    let x = r#"{"meta": {}, "files": {"shop/vat.py": {"executed_lines": [1, 2, 4], "missing_lines": [3]}}}"#;
    let c = change_coverage(&parse(x).unwrap(), &files());
    assert_eq!(c.uncovered.get("shop/vat.py"), Some(&vec![3]));
}

#[test]
fn go_cover_profile() {
    let base = "package shop\n\nfunc Vat(x int) int {\n\treturn x * 125 / 100\n}\n";
    let new = "package shop\n\nfunc Vat(x int) int {\n\tif x < 0 {\n\t\treturn 0\n\t}\n\treturn x * 125 / 100\n}\n";
    let x = "mode: set\nexample.com/shop/vat.go:3.21,4.11 1 1\nexample.com/shop/vat.go:4.11,6.3 1 0\nexample.com/shop/vat.go:7.2,7.22 1 1\n";
    let c = change_coverage(&parse(x).unwrap(), &[("shop/vat.go".into(), base.into(), new.into())]);
    assert_eq!(c.uncovered.get("shop/vat.go"), Some(&vec![5, 6]));
}

#[test]
fn test_files_and_unknown_formats_are_ignored() {
    let cov = parse("SF:tests/test_vat.py\nDA:1,0\nend_of_record\n").unwrap();
    assert_eq!(change_coverage(&cov, &[("tests/test_vat.py".into(), "".into(), "x = 1\n".into())]).executable, 0);
    assert!(parse("hello").is_none());
}

#[test]
fn sarif_results_on_changed_lines_are_kept() {
    let rep = r#"{"version":"2.1.0","runs":[{"tool":{"driver":{"name":"cppcheck"}},"results":[
      {"ruleId":"nullPointer","level":"error","message":{"text":"Null pointer dereference"},"locations":[{"physicalLocation":{"artifactLocation":{"uri":"file://src/drv.c"},"region":{"startLine":3}}}]},
      {"ruleId":"style","level":"note","message":{"text":"style"},"locations":[{"physicalLocation":{"artifactLocation":{"uri":"src/drv.c"},"region":{"startLine":3}}}]},
      {"ruleId":"old","level":"warning","message":{"text":"old finding"},"locations":[{"physicalLocation":{"artifactLocation":{"uri":"src/drv.c"},"region":{"startLine":1}}}]},
      {"ruleId":"waived","level":"error","suppressions":[{"kind":"inSource"}],"message":{"text":"x"},"locations":[{"physicalLocation":{"artifactLocation":{"uri":"src/drv.c"},"region":{"startLine":3}}}]}]}]}"#;
    let r = gitta_core::proof::sarif(rep).unwrap();
    assert_eq!(r.len(), 3);
    let files = vec![("src/drv.c".to_string(), "int a;\nint b;\n".to_string(), "int a;\nint b;\nint *p = 0; *p = 1;\n".to_string())];
    let hits = gitta_core::proof::sarif_on_change(&r, &files);
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!((hits[0].rule.as_str(), hits[0].tool.as_str(), hits[0].line), ("nullPointer", "cppcheck", 3));
}

#[test]
fn mutants_sit_on_changed_code_lines_only() {
    let base = "def fee(n):\n    return 0\n";
    let new = "def fee(n):\n    # free below the limit\n    if n <= 10 and n > 0:\n        return 0\n    log.info(\"n > 10\")\n    return n - 10\n";
    let files = vec![("app/fee.py".to_string(), base.to_string(), new.to_string()), ("tests/test_fee.py".to_string(), String::new(), "assert fee(3) == 0\n".to_string())];
    let ms = gitta_core::proof::mutants(&files, 10);
    let afters: Vec<&str> = ms.iter().map(|m| m.after.as_str()).collect();
    assert!(afters.contains(&"    if n > 10 and n > 0:"), "{afters:?}");
    assert!(afters.contains(&"    if n <= 10 and n <= 0:"), "{afters:?}");
    assert!(afters.contains(&"    if n <= 10 or n > 0:"), "{afters:?}");
    assert!(afters.contains(&"    return n + 10"), "{afters:?}");
    assert!(ms.iter().all(|m| m.path == "app/fee.py" && m.line != 2 && m.line != 5), "no comments, logging or tests: {ms:?}");
    // The budget spreads over lines: the first two mutants are on different lines.
    let two = gitta_core::proof::mutants(&files, 2);
    assert_eq!(two.len(), 2);
    assert_ne!(two[0].line, two[1].line);
}

#[test]
fn operators_inside_strings_and_generics_are_not_mutated() {
    let files = vec![("src/a.rs".to_string(), String::new(), "let v: Vec<u8> = parse(\"a < b\");\nlet ok = x << 2;\n".to_string())];
    assert!(gitta_core::proof::mutants(&files, 10).is_empty(), "{:?}", gitta_core::proof::mutants(&files, 10));
}

#[test]
fn lines_the_preprocessor_dropped_are_uncompiled() {
    let out = "# 1 \"src/a.c\"\n# 1 \"<built-in>\" 1\n# 1 \"src/a.c\"\nint f(void)\n{\n\n\n\treturn 1;\n\n\n}\n";
    // Lines 1 and 2 kept, 3 to 4 dropped (#ifdef CONFIG_X ... ), 5 kept, then #endif and closing brace.
    let compiled = gitta_core::proof::preprocessed_lines(out, "src/a.c");
    assert!(compiled.contains(&1) && compiled.contains(&5) && compiled.contains(&8));
    let base = "int f(void)\n{\n\treturn 0;\n}\n";
    let new = "int f(void)\n{\n#ifdef CONFIG_X\n\tx_init();\n#endif\n\treturn 1;\n}\n";
    let compiled = gitta_core::proof::preprocessed_lines("# 1 \"src/a.c\"\nint f(void)\n{\n# 6 \"src/a.c\"\n\treturn 1;\n}\n", "src/a.c");
    assert_eq!(gitta_core::proof::uncompiled(base, new, &compiled), [4]);
}
