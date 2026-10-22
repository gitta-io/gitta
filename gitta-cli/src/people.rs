//! Telling the person when something only a person can do comes up: approvals of weakened tests or security
//! checks, and problems with the environment. One plain line on the next gitta output they see, and a desktop
//! notification (macOS and Linux), at most one a minute. `gitta init --notify off` or GITTA_NO_NOTIFY turns the
//! desktop notification off.

use super::*;

/// Set while a person runs a gitta command at a terminal: what it says, they read right there.
pub(crate) static AT_TERMINAL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn append(p: &Path, line: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) { let _ = writeln!(f, "{line}"); }
}

/// Something needs a person. `key` keeps it to one telling per finding; `line` is one plain sentence.
pub(crate) fn tell_person(root: &Path, key: &str, line: &str) {
    let dir = root.join(".gitta");
    let told = dir.join("told");
    if std::fs::read_to_string(&told).unwrap_or_default().lines().any(|l| l == key) { return; }
    append(&told, key);
    log(root, "needs-you", "", line);
    // At a terminal the person reads the command's own output; elsewhere the line waits for them and the desktop says it.
    if !AT_TERMINAL.load(std::sync::atomic::Ordering::Relaxed) { append(&dir.join("notices"), line); desktop(root, line); }
}

/// The lines not shown yet, removed as they are taken.
pub(crate) fn take_notices(root: &Path) -> Vec<String> {
    let p = root.join(".gitta/notices");
    let t = std::fs::read_to_string(&p).unwrap_or_default();
    if t.is_empty() { return vec![]; }
    let _ = std::fs::remove_file(&p);
    let mut v: Vec<String> = vec![];
    for l in t.lines().filter(|l| !l.trim().is_empty()) { if !v.iter().any(|x| x == l) { v.push(l.to_string()); } }
    v
}

/// Print waiting notices to the terminal (stderr, so output that scripts read stays clean).
pub(crate) fn show_notices(root: &Path) {
    for l in take_notices(root) { eprintln!("gitta: needs you: {l}"); }
}

fn notify_on(root: &Path) -> bool {
    if std::env::var_os("GITTA_NO_NOTIFY").is_some() { return false; }
    std::fs::read_to_string(cfg_path(root)).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .map(|v| v["notify"].as_bool() != Some(false)).unwrap_or(true)
}

fn desktop(root: &Path, line: &str) {
    if !notify_on(root) { return; }
    let stamp = root.join(".gitta/notified-at");
    if let Ok(t) = std::fs::metadata(&stamp).and_then(|m| m.modified()) {
        if t.elapsed().map(|d| d.as_secs() < 60).unwrap_or(false) { return; }
    }
    let _ = std::fs::write(&stamp, "");
    let msg: String = line.chars().take(220).collect();
    let quiet = |c: &mut Command| { c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().ok(); };
    if cfg!(target_os = "macos") {
        let q = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
        quiet(Command::new("osascript").args(["-e", &format!("display notification {} with title \"gitta\"", q(&msg))]));
    } else if cfg!(target_os = "linux") {
        quiet(Command::new("notify-send").args(["gitta", &msg]));
    }
}
