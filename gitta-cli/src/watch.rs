//! `gitta watch` and the activity feed in the terminal: what gitta did, in plain words.

use super::*;

/// One activity line as a person reads it in a terminal: the time, then what happened.
fn feed_line(e: &serde_json::Value) -> String {
    let when = clock(e["t"].as_f64().unwrap_or(0.0));
    let text = e["text"].as_str().unwrap_or("").lines().next().unwrap_or("").replace('`', "").replace("land refused: ", "Merge stopped. ");
    let mark = match e["kind"].as_str().unwrap_or("") { "clash" => "!", "needs-you" => ">", "adapted" | "landed" => "+", _ => " " };
    format!("{when} {mark} {text}")
}

fn activity(root: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(root.join(".gitta/activity.jsonl")).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// The newest `n` events since `since`; returns how many events the log holds.
fn print_feed(root: &Path, since: f64, n: usize) -> usize {
    let ev = activity(root);
    let shown: Vec<&serde_json::Value> = ev.iter().filter(|e| e["t"].as_f64().unwrap_or(0.0) >= since).collect();
    for e in &shown[shown.len().saturating_sub(n)..] { println!("{}", feed_line(e)); }
    ev.len()
}

/// Today's count in one sentence.
fn today_line(root: &Path) -> String {
    let midnight = Command::new("date").arg("+%H %M %S").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).split_whitespace().filter_map(|x| x.parse::<f64>().ok()).collect::<Vec<_>>())
        .filter(|v| v.len() == 3).map(|v| v[0] * 3600.0 + v[1] * 60.0 + v[2]).unwrap_or(86400.0);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    let (mut checked, mut caught, mut merged) = (0, 0, 0);
    for e in activity(root).iter().filter(|e| e["t"].as_f64().unwrap_or(0.0) >= now - midnight) {
        match e["kind"].as_str().unwrap_or("") { "check" | "adapted" => checked += 1, "clash" => { checked += 1; caught += 1 } "landed" => merged += 1, _ => {} }
    }
    let n = |k: usize, one: &str, many: &str| format!("{k} {}", if k == 1 { one } else { many });
    format!("Today gitta checked {}, caught {} before main, and merged {}.", n(checked, "change", "changes"), n(caught, "clash", "clashes"), n(merged, "piece of work", "pieces of work"))
}

/// One line per agent: what it is doing, from gitta's view of the repository.
fn agent_lines(v: &serde_json::Value) -> Vec<(String, String)> {
    let ws = v["warnings_raw"].as_array().cloned().unwrap_or_default();
    v["agents"].as_array().cloned().unwrap_or_default().iter().map(|g| {
        let n = g["name"].as_str().unwrap_or("").to_string();
        let files = g["changed"].as_array().map(|x| x.len()).unwrap_or(0);
        let holds = g["holds"].as_array().cloned().unwrap_or_default();
        let has = |k: &str| holds.iter().any(|h| h["check"] == k);
        let told: Vec<String> = ws.iter().filter(|w| w["kind"] == "HAZARD" && w["severity"] == "high" && w["referenced_by"] == n.as_str())
            .map(|w| format!("{} changed {}", w["changed_by"].as_str().map(|x| if x == "head" || x == "main" { "main" } else { x }).unwrap_or(""), short_sym(w["symbol"].as_str().unwrap_or("")))).collect();
        let state = if files == 0 { g["merged_at"].as_f64().map(|t| format!("merged into main at {}, no new changes since", clock(t))).unwrap_or("no changes yet".into()) }
            else if has("tests") { "fixing tests that failed in its last merge".into() }
            else if has("approval") || has("security") { "removes tests or weakens a security check; reported when merged, and .gitta.toml decides whether that stops the merge".into() }
            else if has("credentials") { "has what looks like a password or key in its code; gitta told it".into() }
            else if !told.is_empty() { format!("adapting, gitta told it that {}", told.join(" and ")) }
            else { format!("{} changed, nothing affected, ready to merge{}", if files == 1 { "1 file".to_string() } else { format!("{files} files") },
                if v["main_fails"].is_array() { " once main passes its tests" } else { "" }) };
        (n, state)
    }).collect()
}

/// What waits on a person, each with the exact command.
fn person_lines(v: &serde_json::Value, target: &str) -> Vec<String> {
    let mut out = vec![];
    for g in v["agents"].as_array().cloned().unwrap_or_default() {
        let n = g["name"].as_str().unwrap_or("");
        let h: Vec<serde_json::Value> = g["holds"].as_array().cloned().unwrap_or_default().into_iter().filter(|h| h["check"] == "approval" || h["check"] == "security").collect();
        if let Some(x) = h.first() {
            out.push(format!("{n}: {} in {}. Decide when you merge it: git merge {n} (on {target}) asks you.", x["text"].as_str().unwrap_or("").trim_end_matches('.'), x["path"].as_str().unwrap_or("")));
        }
    }
    if let Some(p) = v["setup_problem"].as_object() {
        out.push(format!("The tests could not run because {} is missing. Install it, then run the same git command again.", p.get("missing").and_then(|x| x.as_str()).unwrap_or("a tool")));
    }
    out
}

fn wrap(s: &str, w: usize) -> Vec<String> {
    let mut out = vec![String::new()];
    for word in s.split(' ') {
        let cur = out.last_mut().unwrap();
        if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > w { out.push(word.to_string()); } else { if !cur.is_empty() { cur.push(' '); } cur.push_str(word); }
    }
    out
}

fn fit(s: &str, w: usize) -> String { if s.chars().count() <= w { s.to_string() } else { s.chars().take(w.saturating_sub(1)).collect::<String>() + "…" } }

/// `gitta watch`: a live screen in the terminal. Without a terminal it prints the plain log as it grows.
pub(crate) fn watch(args: &[String]) {
    use std::io::IsTerminal;
    let root = root();
    load(&root);
    if args.iter().any(|a| a == "--once") || !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
        println!("{}", today_line(&root));
        let mut seen = print_feed(&root, 0.0, 15);
        if args.iter().any(|a| a == "--once") { return; }
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let ev = activity(&root);
            for e in ev.iter().skip(seen) { println!("{}", feed_line(e)); }
            seen = ev.len();
        }
    }
    let color = std::env::var_os("NO_COLOR").is_none();
    let (b, d, r, red, green, amber) = if color { ("\x1b[1m", "\x1b[2m", "\x1b[0m", "\x1b[31m", "\x1b[32m", "\x1b[33m") } else { ("", "", "", "", "", "") };
    let tty = || std::fs::File::open("/dev/tty").map(Stdio::from).unwrap_or(Stdio::inherit());
    let saved = Command::new("stty").arg("-g").stdin(tty()).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    let _ = Command::new("stty").args(["-icanon", "-echo", "-isig", "min", "1"]).stdin(tty()).status();
    let quit = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    { let q = quit.clone(); std::thread::spawn(move || { let mut b = [0u8; 1]; while std::io::stdin().read(&mut b).map(|n| n > 0).unwrap_or(false) { if matches!(b[0], b'q' | b'Q' | 3 | 4) { q.store(true, std::sync::atomic::Ordering::Relaxed); break; } } }); }
    print!("\x1b[?1049h\x1b[?25l");
    let target = std::fs::read_to_string(cfg_path(&root)).ok().and_then(|t| serde_json::from_str::<Config>(&t).ok()).map(|c| c.main.trim_start_matches("refs/heads/").to_string()).unwrap_or("main".into());
    let name = root.file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
    let mut v = serde_json::Value::Null;
    let mut tick = 0u32;
    while !quit.load(std::sync::atomic::Ordering::Relaxed) {
        if tick % 3 == 0 { v = status_json(&load(&root)); }
        tick += 1;
        let (h, w) = Command::new("stty").arg("size").stdin(tty()).output().ok().map(|o| { let x: Vec<usize> = String::from_utf8_lossy(&o.stdout).split_whitespace().filter_map(|n| n.parse().ok()).collect(); (x.first().copied().unwrap_or(24), x.get(1).copied().unwrap_or(80)) }).unwrap_or((24, 80));
        let mut lines: Vec<String> = vec![];
        lines.push(format!("{b}gitta is running{r} in {name} and checks every agent change.  {d}q quits{r}"));
        lines.push(fit(&today_line(&root), w));
        let people = person_lines(&v, &target);
        if people.is_empty() { lines.push(format!("{green}Nothing needs you.{r}")); }
        lines.push(String::new());
        if !people.is_empty() {
            lines.push(format!("{b}{amber}Needs you{r}"));
            for p in &people { lines.push(format!("  {}", fit(p, w - 2))); }
            lines.push(String::new());
        }
        lines.push(format!("{b}Agents{r}"));
        let ag = agent_lines(&v);
        if ag.is_empty() { lines.push(format!("  {d}No agent worktrees yet. Start an agent the way you always do.{r}")); }
        let nw = ag.iter().map(|(n, _)| n.chars().count()).max().unwrap_or(0).min(28);
        for (n, st) in &ag { lines.push(format!("  {}{:nw$}{}  {}", b, fit(n, nw), r, fit(st, w.saturating_sub(nw + 4)))); }
        lines.push(String::new());
        lines.push(format!("{b}What gitta did{r}"));
        let room = h.saturating_sub(lines.len() + 1).max(1);
        let ev = activity(&root);
        // Newest last; long sentences wrap under their text, and the oldest lines give way.
        let mut feed: Vec<String> = vec![];
        for e in ev.iter().rev() {
            let c = match e["kind"].as_str().unwrap_or("") { "clash" | "refused" => red, "adapted" | "landed" => green, "needs-you" => amber, _ => "" };
            let mut rows = wrap(&feed_line(e), w.saturating_sub(13).max(20));
            for (i, x) in rows.iter_mut().enumerate() { *x = format!("  {c}{}{x}{r}", if i == 0 { "" } else { "           " }); }
            if feed.len() + rows.len() > room { break; }
            rows.extend(feed); feed = rows;
        }
        lines.extend(feed);
        let frame: String = lines.iter().take(h).map(|l| format!("{l}\x1b[K")).collect::<Vec<_>>().join("\r\n");
        print!("\x1b[H{frame}\x1b[J");
        let _ = std::io::stdout().flush();
        for _ in 0..10 { if quit.load(std::sync::atomic::Ordering::Relaxed) { break; } std::thread::sleep(std::time::Duration::from_millis(100)); }
    }
    print!("\x1b[?25h\x1b[?1049l");
    let _ = std::io::stdout().flush();
    if !saved.is_empty() { let _ = Command::new("stty").arg(&saved).stdin(tty()).status(); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_work_waits_for_a_red_main() {
        let v = |red: serde_json::Value| serde_json::json!({"agents": [{"name": "docs", "changed": ["README.md"], "holds": []}], "main_fails": red});
        assert_eq!(agent_lines(&v(serde_json::Value::Null))[0].1, "1 file changed, nothing affected, ready to merge");
        assert_eq!(agent_lines(&v(serde_json::json!(["t"])))[0].1, "1 file changed, nothing affected, ready to merge once main passes its tests");
    }
}
