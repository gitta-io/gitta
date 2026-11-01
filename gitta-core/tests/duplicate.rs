// Duplicate work: two agents add the same new function or class in different files. Common short names, test
// files and the same file (an OVERLAP) are not reported as duplicates.
use gitta_core::{analyze, analyze_summaries, summarize, Snapshot};

fn snap(files: &[(&str, &str)]) -> Snapshot { files.iter().map(|(p, t)| (p.to_string(), t.as_bytes().to_vec())).collect() }

fn dups(base: &Snapshot, a: &Snapshot, b: &Snapshot) -> Vec<String> {
    analyze(base, ("a", a), ("b", b)).into_iter().filter(|w| w.kind == "DUPLICATE").map(|w| format!("{} {}", w.path, w.symbol.unwrap_or_default())).collect()
}

#[test]
fn the_same_new_function_in_two_files_is_a_duplicate() {
    let base = snap(&[("app/core.py", "def run():\n    pass\n")]);
    let mut a = base.clone();
    a.insert("app/dates.py".into(), b"def parse_iso_date(s):\n    return s\n".to_vec());
    let mut b = base.clone();
    b.insert("app/util/time.py".into(), b"def parse_iso_date(text):\n    return text.strip()\n".to_vec());
    assert_eq!(dups(&base, &a, &b), ["app/util/time.py parse_iso_date (also new in app/dates.py)"]);
    // The team server sees the same from the summaries alone.
    let w = analyze_summaries(("a", &summarize(&base, &a)), ("b", &summarize(&base, &b)));
    assert!(w.iter().any(|x| x.kind == "DUPLICATE" && x.referenced_by.as_deref() == Some("b")), "{w:?}");
}

#[test]
fn common_names_tests_and_the_same_file_are_not_duplicates() {
    let base = snap(&[("app/core.py", "def run():\n    pass\n")]);
    let mut a = base.clone();
    a.insert("app/one.py".into(), b"def main():\n    pass\n\ndef helper():\n    pass\n".to_vec());
    a.insert("tests/test_one.py".into(), b"def make_fixture_order():\n    pass\n".to_vec());
    let mut b = base.clone();
    b.insert("app/two.py".into(), b"def main():\n    pass\n\ndef helper():\n    pass\n".to_vec());
    b.insert("tests/test_two.py".into(), b"def make_fixture_order():\n    pass\n".to_vec());
    assert!(dups(&base, &a, &b).is_empty());
}
