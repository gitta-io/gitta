//! Audit trail: who started which agent, what it changed, what was checked, what was refused, what landed and who
//! approved what. One JSON event per line in `.gitta/audit.jsonl`, each carrying the SHA-256 of the one before, so
//! a changed, removed or reordered line is found by `gitta audit verify`. Events are also sent to the team servers,
//! which keep a copy for the whole team (a lost or wiped laptop does not lose the record).
//!
//!   gitta audit [--since 2026-10-01] [--agent NAME] [--event landed] [--json|--csv] [--team]
//!   gitta audit --follow [--json] [--agent NAME] [--event landed]   new events as they are written, until stopped
//!   gitta audit verify

use serde_json::{json, Value};
use std::io::{BufRead, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::Command;

pub fn file(root: &Path) -> std::path::PathBuf { root.join(".gitta/audit.jsonl") }

fn sha256_hex(b: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, b).as_ref().iter().map(|x| format!("{x:02x}")).collect()
}

/// The hash of an event: SHA-256 over its JSON without the `hash` field (keys sorted, as serde_json's map is).
fn event_hash(ev: &Value) -> String {
    let mut e = ev.clone();
    // `source`, `received` and `sent_by` are added by the team server, after the event was written; `repo` and
    // `severity` by its live feed to log systems
    if let Some(m) = e.as_object_mut() { for k in ["hash", "source", "received", "sent_by", "repo", "severity"] { m.remove(k); } }
    sha256_hex(e.to_string().as_bytes())
}

fn now_rfc3339() -> String {
    let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let (days, rem) = (s.div_euclid(86400), s.rem_euclid(86400));
    // civil date from days since 1970-01-01 (Howard Hinnant's algorithm)
    let z = days + 719468; let era = z.div_euclid(146097); let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153; let d = doy - (153 * mp + 2) / 5 + 1; let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Who is acting: the Git identity, the operating system user and the machine.
pub fn actor(dir: &Path) -> Value {
    let cfg = |k: &str| Command::new("git").args(["-C", &dir.to_string_lossy(), "config", k]).output().ok()
        .filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    let machine = std::env::var("GITTA_MACHINE").ok().or_else(|| Command::new("hostname").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())).unwrap_or_default();
    json!({"name": cfg("user.name"), "email": cfg("user.email"), "os_user": std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_default(), "machine": machine})
}

/// The last event's sequence number and hash, read from the end of the file. The last 64 KiB are read as bytes,
/// since they can start inside a character; the window doubles until it holds a whole event.
fn tail(f: &mut std::fs::File) -> (u64, String) {
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len == 0 { return (0, String::new()); }
    let mut window = 64 * 1024;
    loop {
        let start = len.saturating_sub(window);
        let mut buf = vec![];
        if f.seek(SeekFrom::Start(start)).is_err() || f.read_to_end(&mut buf).is_err() { return (0, String::new()); }
        let lines: Vec<&[u8]> = buf.split(|&b| b == b'\n').collect();
        // The first line is cut off unless the window starts at the top of the file.
        let last = lines[usize::from(start > 0)..].iter().rev().find_map(|l| serde_json::from_slice::<Value>(l).ok());
        if let Some(v) = last { return (v["seq"].as_u64().unwrap_or(0), v["hash"].as_str().unwrap_or("").to_string()); }
        if start == 0 { return (0, String::new()); }
        window *= 2;
    }
}

/// Append one event. `data` holds what happened; never source code, only paths, symbols, commits and results.
pub fn record(root: &Path, event: &str, agent: &str, actor: &Value, data: Value) {
    let path = file(root);
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).read(true).append(true).open(&path) else { return };
    // One writer at a time, so hooks of agents running in parallel never fork the chain.
    if f.lock().is_err() { return; }
    let (seq, prev) = tail(&mut f);
    let mut ev = json!({"seq": seq + 1, "time": now_rfc3339(), "event": event, "agent": agent, "actor": actor, "data": data, "prev": prev});
    let h = event_hash(&ev);
    ev["hash"] = Value::String(h);
    let _ = writeln!(f, "{ev}");
    let _ = f.unlock();
}

pub fn read(root: &Path) -> Vec<Value> {
    let Ok(f) = std::fs::File::open(file(root)) else { return vec![] };
    std::io::BufReader::new(f).lines().map_while(Result::ok).filter_map(|l| serde_json::from_str(&l).ok()).collect()
}

/// Checks the chain. Ok(number of events) or Err(where it breaks and how).
pub fn verify(events: &[Value]) -> Result<usize, String> {
    let mut prev = String::new();
    for (i, e) in events.iter().enumerate() {
        let seq = e["seq"].as_u64().unwrap_or(0);
        if seq != i as u64 + 1 { return Err(format!("event {} has sequence number {seq}: events were removed or reordered", i + 1)); }
        if e["prev"].as_str().unwrap_or("") != prev { return Err(format!("event {seq} does not follow event {}: an event before it was changed or removed", seq - 1)); }
        let h = e["hash"].as_str().unwrap_or("");
        if event_hash(e) != h { return Err(format!("event {seq} ({}, {}) was changed after it was written", e["event"].as_str().unwrap_or(""), e["time"].as_str().unwrap_or(""))); }
        prev = h.to_string();
    }
    Ok(events.len())
}

/// Which copy of the repository on which machine an event comes from: each has its own chain.
pub fn source(root: &Path) -> String {
    let m = actor(root)["machine"].as_str().unwrap_or("").to_string();
    sha256_hex(format!("{m}:{}", root.display()).as_bytes())[..16].to_string()
}

/// Events not yet sent to a team server go there in one batch; the server keeps them per repository and machine.
pub fn send_pending(root: &Path, urls: &str, token: &str, repo: &str) {
    // Cheap check first: the last event's number against what each server already has.
    let last = std::fs::File::open(file(root)).map(|mut f| tail(&mut f).0).unwrap_or(0);
    if last == 0 { return; }
    let behind = urls.split(',').map(|u| u.trim().trim_end_matches('/')).filter(|u| !u.is_empty()).any(|url| {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new(); url.hash(&mut h);
        let sent: u64 = std::fs::read_to_string(root.join(".gitta").join(format!("audit-sent-{:016x}", h.finish()))).ok().and_then(|t| t.trim().parse().ok()).unwrap_or(0);
        sent < last
    });
    if !behind { return; }
    let events = read(root);
    if events.is_empty() { return; }
    std::thread::scope(|sc| {
        for url in urls.split(',').map(|u| u.trim().trim_end_matches('/')).filter(|u| !u.is_empty()) {
            let events = &events;
            sc.spawn(move || {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new(); url.hash(&mut h);
                let cursor = root.join(".gitta").join(format!("audit-sent-{:016x}", h.finish()));
                let sent: u64 = std::fs::read_to_string(&cursor).ok().and_then(|t| t.trim().parse().ok()).unwrap_or(0);
                let new: Vec<&Value> = events.iter().filter(|e| e["seq"].as_u64().unwrap_or(0) > sent).take(500).collect();
                let Some(last) = new.last().and_then(|e| e["seq"].as_u64()) else { return };
                let body = json!({"repo": repo, "source": source(root), "events": new}).to_string();
                let mut curl = Command::new("curl");
                if let Ok(ca) = std::env::var("GITTA_SERVER_CA") { curl.args(["--cacert", &ca]); }
                let ok = curl.args(["-fsS", "--proto", "=http,https", "--connect-timeout", "1", "--max-time", "4", "-X", "POST", "-H", "Content-Type: application/json",
                        "-H", &format!("Authorization: Bearer {token}"), "--data-binary", "@-", &format!("{url}/v1/audit")])
                    .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn()
                    .and_then(|mut ch| { ch.stdin.take().unwrap().write_all(body.as_bytes())?; ch.wait() }).map(|s| s.success()).unwrap_or(false);
                if ok { let _ = std::fs::write(&cursor, last.to_string()); }
            });
        }
    });
}

// First ten characters, on a char boundary: audit events can come from a team server, so never slice mid-UTF-8.
fn short(v: &str) -> String { v.chars().take(10).collect() }

fn summary(e: &Value) -> String {
    let d = &e["data"];
    let s = |k: &str| d[k].as_str().unwrap_or("").to_string();
    match e["event"].as_str().unwrap_or("") {
        "agent_started" => format!("{} workspace on branch {} from {}", s("tool"), s("branch"), short(&s("base"))),
        "warning" if s("kind") == "SECURITY_NOTE" => format!("SECURITY_NOTE {} ({}) at {}:{}", s("rule"), s("severity"), s("path"), d["line"]),
        "security_allowed" => format!("{} at {}:{} allowed: {}{}", s("rule"), s("path"), d["line"], s("reason"), if d["added_by_change"].as_bool() == Some(true) { " (added in this change)" } else { "" }),
        "warning" => format!("{} {} in {}{}", s("kind"), s("symbol"), s("path"), if s("other").is_empty() { String::new() } else { format!(" (with {}{})", s("other"), if s("machine").is_empty() { String::new() } else { format!(" on {}", s("machine")) }) }),
        "checks" => d["checks"].as_array().map(|c| c.iter().map(|x| format!("{} {}", x["check"].as_str().unwrap_or(""), x["result"].as_str().unwrap_or(""))).collect::<Vec<_>>().join(", ")).unwrap_or_default(),
        "land_refused" => format!("{}: {}", s("check"), s("reason")),
        "landed" => format!("{} file(s), {} -> {}{}", d["files"].as_array().map(|f| f.len()).unwrap_or(0), short(&s("from")), short(&s("to")), if d["approvals"].as_array().is_some_and(|a| !a.is_empty()) { ", with approval" } else { "" }),
        "approved" => format!("{} approved: {}", e["actor"]["name"].as_str().unwrap_or(""), s("reason")),
        _ => String::new(),
    }
}

/// The whole team's events from the first team server that answers.
fn fetch_team(team: Option<(String, String, String)>) -> Vec<Value> {
    let Some((urls, token, repo)) = team else { eprintln!("gitta: no team server configured (gitta init --server URL --token T)"); std::process::exit(1) };
    for url in urls.split(',').map(|u| u.trim().trim_end_matches('/')).filter(|u| !u.is_empty()) {
        let mut curl = Command::new("curl");
        if let Ok(ca) = std::env::var("GITTA_SERVER_CA") { curl.args(["--cacert", &ca]); }
        let out = curl.args(["-fsS", "--connect-timeout", "2", "--max-time", "30", "-H", &format!("Authorization: Bearer {token}"), "--get", "--data-urlencode", &format!("repo={repo}"), &format!("{url}/v1/audit")]).output();
        if let Some(v) = out.ok().filter(|o| o.status.success()).and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok()).and_then(|v| v["events"].as_array().cloned()) { return v; }
    }
    eprintln!("gitta: no team server answered"); std::process::exit(1)
}

fn line(e: &Value) -> String {
    format!("{}  {:<13} {:<14} {:<18} {}", e["time"].as_str().unwrap_or(""), e["event"].as_str().unwrap_or(""), e["agent"].as_str().unwrap_or(""),
        format!("{}@{}", e["actor"]["name"].as_str().unwrap_or(""), e["actor"]["machine"].as_str().unwrap_or("")), summary(e))
}

/// `gitta audit --follow`: prints each new event as it is written (one JSON line each with --json), so it can be
/// piped into any log shipper. Starts at the end of the trail; with --since it first prints the matching history.
/// Reads the file only, so it never holds up an agent or a merge.
fn follow(root: &Path, args: &[String], history: Option<Vec<Value>>) {
    let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let (agent, kind, json) = (get("--agent"), get("--event"), args.iter().any(|a| a == "--json"));
    let out = |e: &Value| -> bool {
        if agent.as_deref().is_some_and(|a| e["agent"].as_str() != Some(a)) || kind.as_deref().is_some_and(|k| e["event"].as_str() != Some(k)) { return true; }
        let mut o = std::io::stdout().lock();
        writeln!(o, "{}", if json { e.to_string() } else { line(e) }).and_then(|_| o.flush()).is_ok()   // false: the reader went away
    };
    let path = file(root);
    let mut pos = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    if let Some(h) = history { for e in &h { if !out(e) { return; } } }
    let mut partial = String::new();
    loop {
        let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if len < pos { pos = 0; partial.clear(); }   // the trail was replaced: read it from the start
        if len > pos {
            if let Ok(mut f) = std::fs::File::open(&path) {
                let mut buf = String::new();
                if f.seek(SeekFrom::Start(pos)).is_ok() && f.take(len - pos).read_to_string(&mut buf).is_ok() {
                    pos = len;
                    partial.push_str(&buf);
                    while let Some(i) = partial.find('\n') {
                        let l: String = partial.drain(..=i).collect();
                        if let Ok(e) = serde_json::from_str::<Value>(l.trim()) { if !out(&e) { return; } }
                    }
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

pub fn cmd(root: &Path, args: &[String], team: Option<(String, String, String)>) {
    let team = &team;
    if args.first().map(String::as_str) == Some("verify") {
        if args.iter().any(|a| a == "--team") {
            let all = fetch_team(team.clone());
            let mut by: std::collections::BTreeMap<String, Vec<Value>> = Default::default();
            for e in all { by.entry(e["source"].as_str().unwrap_or("").to_string()).or_default().push(e); }
            let mut bad = false;
            for (src, mut evs) in by {
                evs.sort_by_key(|e| e["seq"].as_u64().unwrap_or(0));
                let who = evs.first().map(|e| format!("{}@{}", e["actor"]["name"].as_str().unwrap_or(""), e["actor"]["machine"].as_str().unwrap_or(""))).unwrap_or_default();
                match verify(&evs) { Ok(n) => println!("gitta: {who} ({src}): intact, {n} event(s)"), Err(e) => { bad = true; println!("gitta: {who} ({src}): BROKEN: {e}") } }
            }
            if bad { std::process::exit(1) }
            return;
        }
        match verify(&read(root)) {
            Ok(n) => println!("gitta: audit trail intact: {n} event(s), every one linked to the one before"),
            Err(e) => { eprintln!("gitta: audit trail BROKEN: {e}"); std::process::exit(1) }
        }
        return;
    }
    let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let mut events = if args.iter().any(|a| a == "--team") { fetch_team(team.clone()) } else { read(root) };
    if args.iter().any(|a| a == "--team") { events.sort_by(|a, b| a["time"].as_str().cmp(&b["time"].as_str())); }
    if let Some(s) = get("--since") { events.retain(|e| e["time"].as_str().unwrap_or("") >= s.as_str()); }
    if let Some(a) = get("--agent") { events.retain(|e| e["agent"].as_str() == Some(a.as_str())); }
    if let Some(k) = get("--event") { events.retain(|e| e["event"].as_str() == Some(k.as_str())); }
    if args.iter().any(|a| a == "--follow" || a == "-f") { return follow(root, args, get("--since").is_some().then_some(events)); }
    if args.iter().any(|a| a == "--json") {
        for e in &events { println!("{e}"); }
    } else if args.iter().any(|a| a == "--csv") {
        let q = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
        println!("time,event,agent,person,email,machine,summary,hash");
        for e in &events {
            let a = &e["actor"];
            println!("{}", [e["time"].as_str().unwrap_or(""), e["event"].as_str().unwrap_or(""), e["agent"].as_str().unwrap_or(""), a["name"].as_str().unwrap_or(""),
                a["email"].as_str().unwrap_or(""), a["machine"].as_str().unwrap_or(""), &summary(e), e["hash"].as_str().unwrap_or("")].iter().map(|x| q(x)).collect::<Vec<_>>().join(","));
        }
    } else {
        if events.is_empty() { println!("gitta: no audit events yet"); return; }
        for e in &events { println!("{}", line(e)); }
    }
}

#[cfg(test)]
mod tail_tests {
    use super::*;
    fn trail(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("gitta-audit-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".gitta")).unwrap();
        root
    }
    #[test]
    fn the_chain_goes_on_when_the_last_64_kib_start_inside_a_character() {
        let root = trail("utf8");
        let who = json!({"name": "Łukasz Żółć"});
        // Add events until the next append reads from a byte in the middle of a character, then add one more.
        for n in 1..2000 {
            record(&root, "approved", "a", &who, json!({"reason": "zażółć gęślą jaźń ".repeat(10)}));
            let b = std::fs::read(file(&root)).unwrap();
            if b.len() > 64 * 1024 && b[b.len() - 64 * 1024] & 0xC0 == 0x80 {
                record(&root, "approved", "a", &who, json!({"reason": "one more"}));
                assert_eq!(verify(&read(&root)), Ok(n + 1));
                let _ = std::fs::remove_dir_all(&root);
                return;
            }
        }
        panic!("the trail never started a 64 KiB window inside a character");
    }
    #[test]
    fn the_chain_goes_on_after_an_event_over_64_kib() {
        let root = trail("big");
        record(&root, "warning", "a", &json!({}), json!({"findings": "x".repeat(70 * 1024)}));
        record(&root, "warning", "a", &json!({}), json!({"findings": "y"}));
        assert_eq!(verify(&read(&root)), Ok(2));
        let _ = std::fs::remove_dir_all(&root);
    }
}
