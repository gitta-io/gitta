// Static interference: one agent newly writes shared state that the other agent's new code reads or also writes.
use gitta_core::{analyze, Snapshot};

fn snap(files: &[(&str, &str)]) -> Snapshot { files.iter().map(|(p, t)| (p.to_string(), t.as_bytes().to_vec())).collect() }
fn notes(base: &Snapshot, a: &Snapshot, b: &Snapshot) -> Vec<String> {
    analyze(base, ("a", a), ("b", b)).into_iter().filter(|w| w.kind == "INTERFERENCE").map(|w| format!("{} {}", w.path, w.symbol.unwrap_or_default())).collect()
}

const CACHE: &str = "RETRY_LIMIT = 3\n\n\ndef fetch(url):\n    return url\n\n\ndef reset():\n    pass\n";

#[test]
fn a_new_write_to_a_global_that_new_code_reads_is_noted() {
    let base = snap(&[("app/net.py", CACHE), ("app/jobs.py", "def run():\n    return 1\n")]);
    let mut a = base.clone();
    a.insert("app/net.py".into(), CACHE.replace("def reset():\n    pass", "def reset():\n    global RETRY_LIMIT\n    RETRY_LIMIT = 0").into_bytes());
    let mut b = base.clone();
    b.insert("app/net.py".into(), format!("{CACHE}\n\ndef retries_left(done):\n    return RETRY_LIMIT - done\n").into_bytes());
    assert_eq!(notes(&base, &a, &b), ["app/net.py RETRY_LIMIT"]);
}

#[test]
fn both_writing_the_same_attribute_is_noted_and_locals_are_not() {
    let cls = "class Session:\n    def __init__(self):\n        self.token = None\n\n    def login(self):\n        return 1\n\n    def logout(self):\n        return 0\n";
    let base = snap(&[("app/s.py", cls)]);
    let mut a = base.clone(); a.insert("app/s.py".into(), cls.replace("    def login(self):\n        return 1", "    def login(self):\n        self.token = 'x'\n        return 1").into_bytes());
    let mut b = base.clone(); b.insert("app/s.py".into(), cls.replace("    def logout(self):\n        return 0", "    def logout(self):\n        self.token = None\n        return 0").into_bytes());
    assert_eq!(notes(&base, &a, &b), ["app/s.py self.token"]);
    // A local with the same name as nothing shared is not shared state.
    let mut c = base.clone(); c.insert("app/s.py".into(), cls.replace("        return 1", "        count = 2\n        return count").into_bytes());
    let mut d = base.clone(); d.insert("app/s.py".into(), cls.replace("        return 0", "        count = 3\n        return count").into_bytes());
    assert!(notes(&base, &c, &d).is_empty());
}
