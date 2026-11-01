//! Test notes on a change: changed code with no test change, and new tests that will fail at random.
//!
//!   untested-change  code changed and no test changed in the same change (low; medium when the untested lines
//!                    catch or throw, the lines agent pull requests leave untested most often)
//!   flaky-test       an added test line depends on the wall clock, unseeded randomness, sleeps or files outside a
//!                    temporary directory fixture
//!
//! Text rules on the lines a change adds (a line counts as added when the new file has it more often than the base,
//! ignoring indentation). No parse and no repository scan.

use crate::Snapshot;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// One note on an added line.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Note {
    pub path: String,
    pub line: usize,
    pub rule: String,
    pub severity: String,
    /// What the change does: "changes code with no change to any test in the same change."
    pub text: String,
    /// The better pattern: "Add or update a test for the changed behaviour."
    pub safer: String,
    /// Why this line matches.
    #[serde(default, skip_serializing_if = "Option::is_none")] pub detail: Option<String>,
    /// Stable across line shifts: the path, the rule and the line's text.
    pub key: String,
}

const RULES: &[(&str, &str, &str, &str)] = &[
    ("untested-change", "low", "changes code with no change to any test in the same change.", "Add or update a test for the changed behaviour."),
    ("flaky-test", "low", "adds a test that depends on the clock, randomness, sleeps or shared files.", "Inject the clock, seed the random generator, wait on a condition instead of sleeping, and use a temporary directory fixture."),
];

#[derive(Clone, Copy, PartialEq)]
enum L { Py, Js, Go, Java, Kotlin, Cs, Rs, C }

fn lang(path: &str) -> Option<L> {
    let e = path.rsplit_once('.').map(|x| x.1.to_ascii_lowercase()).unwrap_or_default();
    Some(match e.as_str() {
        "py" | "pyw" => L::Py, "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts" => L::Js, "go" => L::Go,
        "java" => L::Java, "kt" | "kts" => L::Kotlin, "cs" => L::Cs, "rs" => L::Rs,
        "c" | "h" | "ino" | "cc" | "cpp" | "cxx" | "c++" | "hpp" | "hh" | "hxx" | "h++" | "ipp" | "inl" | "tpp" => L::C, _ => return None,
    })
}

fn fnv(s: &str) -> String { let mut h: u64 = 0xcbf29ce484222325; for b in s.bytes() { h ^= b as u64; h = h.wrapping_mul(0x100000001b3); } format!("{h:016x}") }
fn squash(s: &str) -> String { s.chars().filter(|c| !c.is_whitespace()).collect() }

fn comment(t: &str, l: L) -> bool {
    if l == L::Py { t.starts_with('#') } else { t.starts_with("//") || t.starts_with("/*") || t.starts_with("* ") || t == "*" }
}

/// For each line of `new`: the new file has it more often than `old` (indentation ignored).
fn added_lines(old: &str, new: &str) -> Vec<bool> {
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for x in old.lines() { *seen.entry(x.trim()).or_default() += 1; }
    new.lines().map(|l| match seen.get_mut(l.trim()) { Some(c) if *c > 0 => { *c -= 1; false } _ => true }).collect()
}

fn note(path: &str, lines: &[&str], n: usize, id: &str, detail: Option<String>) -> Note {
    let t = lines.get(n).copied().unwrap_or("").trim();
    let (_, sev, text, safer) = *RULES.iter().find(|r| r.0 == id).expect("known rule");
    Note { path: path.into(), line: n + 1, rule: id.into(), severity: sev.into(), text: text.into(), safer: safer.into(), detail, key: fnv(&format!("{path}|{id}|{t}")) }
}

/// Flaky idioms on an added test line: the wall clock, unseeded randomness, sleeps, files outside a fixture.
fn flaky(t: &str, l: L, file: &str) -> Option<String> {
    fn has(t: &str, xs: &[&'static str]) -> Option<&'static str> { xs.iter().find(|x| t.contains(**x)).copied() }
    let seeded = ["random.seed(", "np.random.seed(", "Random(", "rand.Seed(", "rand.New(rand.NewSource(", "faker.seed(", "seed(", "StdRng::seed_from_u64(", "new Random("].iter().any(|s| file.contains(s));
    let fixture = ["tmp_path", "tmpdir", "tempfile.", "TemporaryDirectory", "mkdtemp", "t.TempDir()", "@TempDir", "os.tmpdir()", "fs.mkdtemp", "tempdir()", "TempDir::new", "Path.GetTempPath", "Files.createTempDirectory", "mock_open", "pyfakefs", "memfs", "mock-fs"].iter().any(|s| file.contains(s));
    let (clock, rand, sleep, files): (&[&'static str], &[&'static str], &[&'static str], &[&'static str]) = match l {
        L::Py => (&["datetime.now(", "datetime.utcnow(", "time.time()", "date.today()"], &["random.random(", "random.randint(", "random.choice(", "random.shuffle(", "uuid.uuid4("], &["time.sleep("], &["open(", "os.remove(", "shutil.rmtree("]),
        L::Js => (&["Date.now()", "new Date()"], &["Math.random()"], &["setTimeout(", "sleep("], &["fs.writeFileSync(", "fs.writeFile(", "fs.rmSync(", "fs.unlinkSync("]),
        L::Go => (&["time.Now()"], &["rand.Intn(", "rand.Int(", "rand.Float64("], &["time.Sleep("], &["os.Create(", "os.WriteFile(", "os.Remove("]),
        L::Java | L::Kotlin => (&["System.currentTimeMillis()", "LocalDateTime.now()", "Instant.now()", "new Date()"], &["Math.random()", "new Random()"], &["Thread.sleep("], &["new FileWriter(", "Files.write(", "new FileOutputStream("]),
        L::Cs => (&["DateTime.Now", "DateTime.UtcNow"], &["new Random()"], &["Thread.Sleep(", "Task.Delay("], &["File.WriteAllText(", "File.Delete("]),
        L::Rs => (&["SystemTime::now()", "Instant::now()"], &["rand::random", "thread_rng()"], &["thread::sleep("], &["File::create(", "fs::write(", "fs::remove_file("]),
        L::C => (&["time(NULL)", "clock()"], &["rand()"], &["sleep(", "usleep("], &["fopen("]),
    };
    if let Some(x) = has(t, sleep) { return Some(format!("sleeps ({})", x.trim_end_matches('('))); }
    if let Some(x) = has(t, clock) { return Some(format!("reads the wall clock ({})", x.trim_end_matches('('))); }
    if !seeded { if let Some(x) = has(t, rand) { return Some(format!("uses unseeded randomness ({})", x.trim_end_matches('('))); } }
    if !fixture { if let Some(x) = has(t, files) { if !(l == L::Py && x == "open(" && !(t.contains("\"w") || t.contains("'w") || t.contains("\"a") || t.contains("'a"))) { return Some(format!("writes files outside a temporary directory fixture ({})", x.trim_end_matches('('))); } } }
    None
}

/// New test lines that will fail at random: one note per added flaky line of a test file.
pub fn flaky_tests(base: &Snapshot, new: &Snapshot, paths: &[String]) -> Vec<Note> {
    let mut out = vec![];
    for p in paths.iter().filter(|p| crate::guard::is_test(p)) {
        let (Some(l), Some(n)) = (lang(p), new.get(p)) else { continue };
        let os = base.get(p).map(|x| String::from_utf8_lossy(x).to_string()).unwrap_or_default();
        let ns = String::from_utf8_lossy(n).to_string();
        let lines: Vec<&str> = ns.lines().collect();
        for (i, a) in added_lines(&os, &ns).into_iter().enumerate() {
            let t = lines[i].trim();
            if !a || t.is_empty() || comment(t, l) { continue; }
            if let Some(why) = flaky(t, l, &ns) { out.push(note(p, &lines, i, "flaky-test", Some(why))); }
        }
    }
    out
}

/// Changed code with no test change in the same change: one note per changed source file, low, raised to medium
/// when the untested lines catch or throw (agent pull requests leave try/catch and throw lines untested most often).
pub fn untested(base: &Snapshot, new: &Snapshot, paths: &[String]) -> Vec<Note> {
    let tests_changed = paths.iter().any(|p| crate::guard::is_test(p) && base.get(p) != new.get(p));
    if tests_changed { return vec![]; }
    let mut out = vec![];
    for p in paths {
        let Some(l) = lang(p) else { continue };
        if crate::guard::is_test(p) { continue; }
        let Some(n) = new.get(p) else { continue };
        let o = base.get(p);
        if o == Some(n) { continue; }
        let os = o.map(|x| String::from_utf8_lossy(x).to_string()).unwrap_or_default();
        let ns = String::from_utf8_lossy(n);
        let lines: Vec<&str> = ns.lines().collect();
        let added = added_lines(&os, &ns);
        let changed: Vec<usize> = added.iter().enumerate().filter(|(i, a)| **a && lines.get(*i).map(|x| { let t = x.trim(); !t.is_empty() && !comment(t, l) && !t.starts_with("import ") && !t.starts_with("use ") && !t.starts_with("#include") }).unwrap_or(false)).map(|x| x.0).collect();
        let Some(&first) = changed.first() else { continue };
        let error_path: Vec<usize> = changed.iter().copied().filter(|i| { let t = lines[*i].trim(); t.contains("catch") || t.starts_with("except") || t.starts_with("raise") || t.starts_with("throw") || t.contains(" throw ") || squash(t).starts_with("iferr!=nil") || t.contains("Err(") }).collect();
        let (at, detail) = match error_path.first() {
            Some(&e) => (e, format!("{} changed line(s), {} of them catch or throw, and no test changed", changed.len(), error_path.len())),
            None => (first, format!("{} changed line(s), and no test changed", changed.len())),
        };
        let mut nt = note(p, &lines, at, "untested-change", Some(detail));
        if !error_path.is_empty() { nt.severity = "medium".into(); }
        nt.key = fnv(&format!("{p}|untested-change"));
        out.push(nt);
    }
    out
}
