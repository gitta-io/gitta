//! End-to-end tests of the gitta binary on throwaway repositories, with a throwaway HOME.
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::io::Write;

struct Env { dir: PathBuf, home: PathBuf, repo: PathBuf }

fn env(tag: &str) -> Env {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("gitta-e2e-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (home, repo) = (dir.join("home"), dir.join("repo"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    Env { dir, home, repo }
}

/// A git command for test setup. It ignores the user's own git configuration, so a global core.hooksPath never runs
/// the user's hooks on test repositories.
fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// The gitta binary as a person at a terminal runs it: none of the variables that mark an agent session.
fn gitta_cmd() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_gitta"));
    for v in ["CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT", "CURSOR_AGENT", "CODEX_SANDBOX", "CODEX_SANDBOX_NETWORK_DISABLED", "GITTA_AGENT"] { c.env_remove(v); }
    c
}

fn gitta(e: &Env, dir: &Path, args: &[&str]) -> Output {
    gitta_cmd().env("GITTA_NO_NOTIFY", "1").env("GITTA_NO_PROMPT", "1").env("GITTA_WEAKENED_CHECKS", "block").args(args).current_dir(dir).env("HOME", &e.home).output().unwrap()
}

fn gitta_in(e: &Env, dir: &Path, args: &[&str], input: &str) -> Output {
    let mut c = gitta_cmd().env("GITTA_NO_NOTIFY", "1").env("GITTA_NO_PROMPT", "1").env("GITTA_WEAKENED_CHECKS", "block").args(args).current_dir(dir).env("HOME", &e.home)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    c.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    c.wait_with_output().unwrap()
}

fn text(o: &Output) -> String { format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)) }

fn write(p: &Path, s: &str) { if let Some(d) = p.parent() { std::fs::create_dir_all(d).unwrap(); } std::fs::write(p, s).unwrap(); }

const PRICING: &str = "VAT_RATE = 0.25\n\n\ndef net_to_gross(net):\n    return round(net * (1 + VAT_RATE), 2)\n";
const CART: &str = "from shop.pricing import net_to_gross\n\n\ndef total(x):\n    return net_to_gross(x)\n";

fn shop(e: &Env) {
    write(&e.repo.join("shop/__init__.py"), "");
    write(&e.repo.join("shop/pricing.py"), PRICING);
    write(&e.repo.join("shop/cart.py"), CART);
    write(&e.repo.join("shop/text.py"), "def shout(s):\n    return s.upper()\n");
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "base"]);
}

fn worktree(e: &Env, name: &str) -> PathBuf {
    let p = e.dir.join(format!("wt-{name}"));
    git(&e.repo, &["worktree", "add", "-q", "-b", name, p.to_str().unwrap()]);
    p
}

#[test]
fn init_picks_up_worktrees_and_lands_onto_checked_out_main() {
    let e = env("land");
    shop(&e);
    assert!(gitta(&e, &e.repo, &["init", "--no-claude"]).status.success());
    let a = worktree(&e, "feat");
    write(&a.join("shop/text.py"), "def shout(s):\n    return s.upper()\n\n\ndef whisper(s):\n    return s.lower()\n");
    let st = text(&gitta(&e, &e.repo, &["status"]));
    assert!(st.contains("feat"), "status should list the worktree: {st}");
    let o = gitta(&e, &e.repo, &["land", "feat"]);
    assert!(o.status.success(), "{}", text(&o));
    // main is checked out in the repo: it was fast-forwarded, files included, one parent.
    assert!(std::fs::read_to_string(e.repo.join("shop/text.py")).unwrap().contains("whisper"));
    assert_eq!(git(&e.repo, &["rev-list", "--parents", "-n1", "main"]).split(' ').count(), 2, "landing commit has exactly one parent");
}

#[test]
fn land_never_overwrites_uncommitted_edits_in_the_checkout() {
    let e = env("dirty");
    shop(&e);
    gitta(&e, &e.repo, &["init", "--no-claude"]);
    let a = worktree(&e, "feat");
    write(&a.join("shop/text.py"), "def shout(s):\n    return s.upper() + '!'\n");
    write(&e.repo.join("shop/text.py"), "def shout(s):\n    return s.upper()  # local edit\n");
    let o = gitta(&e, &e.repo, &["land", "feat"]);
    assert!(!o.status.success());
    assert!(text(&o).contains("uncommitted edits"), "{}", text(&o));
    assert!(std::fs::read_to_string(e.repo.join("shop/text.py")).unwrap().contains("local edit"));
}

#[test]
fn changed_meaning_through_a_helper_is_reported() {
    let e = env("meaning");
    shop(&e);
    gitta(&e, &e.repo, &["init", "--no-claude"]);
    let (a, b, c) = (worktree(&e, "vat"), worktree(&e, "label"), worktree(&e, "loud"));
    write(&a.join("shop/pricing.py"), &PRICING.replace("0.25", "0.12"));
    write(&b.join("shop/label.py"), "from shop.cart import total\n\n\ndef label(x):\n    return f\"{total(x)} kr incl. 25% VAT\"\n");
    write(&c.join("shop/text.py"), "def shout(s):\n    return s.upper()\n\n\ndef loud(s):\n    return s.upper() + '!'\n");
    let o = gitta(&e, &e.repo, &["land", "vat"]);
    assert!(o.status.success(), "{}", text(&o));
    let o = gitta(&e, &e.repo, &["land", "label"]);
    assert!(o.status.success(), "label relies on the old VAT through cart.total and is reported, and the tests decide");
    let t = text(&o);
    assert!(t.contains("shop.cart.total"), "{t}");
    // The note says what changed and where the agent's code uses it, so one repair round is enough.
    assert!(t.contains("What landed") && t.contains("shop/pricing.py") && t.contains("+VAT_RATE = 0.12"), "{t}");
    assert!(t.contains("shop/label.py:5:"), "{t}");
    let o = gitta(&e, &e.repo, &["land", "loud"]);
    assert!(o.status.success(), "an unrelated agent still lands: {}", text(&o));
}

const NET_H: &str = "#ifndef NET_H\n#define NET_H\n\n#include <stddef.h>\n\nstruct conn {\n    int fd;\n    size_t sent;\n};\n\nint conn_write(struct conn *c, const char *buf, size_t len);\n\n#endif\n";
const NET_C: &str = "#include \"net.h\"\n#include <unistd.h>\n\nint conn_write(struct conn *c, const char *buf, size_t len) {\n    ssize_t n = write(c->fd, buf, len);\n    if (n > 0) c->sent += (size_t)n;\n    return (int)n;\n}\n";

#[test]
fn c_header_change_is_reported_for_new_code_that_uses_it() {
    let e = env("c");
    write(&e.repo.join("include/net.h"), NET_H);
    write(&e.repo.join("src/net.c"), NET_C);
    write(&e.repo.join("src/main.c"), "#include \"net.h\"\n\nint main(void) {\n    return 0;\n}\n");
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "base"]);
    gitta(&e, &e.repo, &["init", "--no-claude"]);
    let (a, b, c) = (worktree(&e, "flags"), worktree(&e, "proxy"), worktree(&e, "usage"));
    // One agent adds a parameter to the declaration in the header (and the definition).
    write(&a.join("include/net.h"), &NET_H.replace("size_t len);", "size_t len, int flags);"));
    write(&a.join("src/net.c"), &NET_C.replace("size_t len) {", "size_t len, int flags) {").replace("write(c->fd, buf, len)", "flags ? send(c->fd, buf, len, flags) : write(c->fd, buf, len)"));
    // Another agent's new file calls it the old way; a third agent's new code does not use it.
    write(&b.join("src/proxy.c"), "#include \"net.h\"\n\nint proxy_forward(struct conn *to, const char *buf, size_t len) {\n    return conn_write(to, buf, len);\n}\n");
    write(&c.join("src/main.c"), "#include \"net.h\"\n#include <stdio.h>\n\nstatic void usage(void) {\n    fputs(\"usage: net [-p port]\\n\", stderr);\n}\n\nint main(void) {\n    usage();\n    return 0;\n}\n");
    let o = gitta(&e, &e.repo, &["land", "flags"]);
    assert!(o.status.success(), "{}", text(&o));
    let o = gitta(&e, &e.repo, &["land", "proxy"]);
    assert!(o.status.success(), "proxy calls conn_write as it was before the header changed: {}", text(&o));
    assert!(text(&o).contains("conn_write"), "{}", text(&o));
    let o = gitta(&e, &e.repo, &["land", "usage"]);
    assert!(o.status.success(), "an unrelated C change still lands: {}", text(&o));
}

#[test]
fn live_warning_reaches_the_affected_agent_through_the_hook() {
    let e = env("hook");
    shop(&e);
    gitta(&e, &e.repo, &["init", "--no-claude"]);
    let (a, b) = (worktree(&e, "vat"), worktree(&e, "label"));
    write(&a.join("shop/pricing.py"), &PRICING.replace("return round(net * (1 + VAT_RATE), 2)", "return int(net * (1 + VAT_RATE) * 100)"));
    write(&b.join("shop/label.py"), "from shop.pricing import net_to_gross\n\n\ndef label(x):\n    return f\"{net_to_gross(x)} kr\"\n");
    let ev = format!("{{\"cwd\": {:?}, \"tool_name\": \"Edit\"}}", b.to_str().unwrap());
    let o = gitta_in(&e, &b, &["hook", "claude"], &ev);
    assert!(o.status.success());
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("net_to_gross") && out.contains("additionalContext"), "hook output: {out}");
}

#[test]
fn hooks_are_silent_outside_gitta_and_on_bad_input() {
    let e = env("quiet");
    let plain = e.dir.join("not-a-repo");
    std::fs::create_dir_all(&plain).unwrap();
    for (dir, input) in [(&plain, format!("{{\"cwd\": {:?}}}", plain.to_str().unwrap())), (&e.repo, "garbage".to_string())] {
        for kind in ["claude", "claude-stop"] {
            let o = gitta_in(&e, dir, &["hook", kind], &input);
            assert!(o.status.success(), "hook {kind} must not fail");
            assert!(o.stdout.is_empty() && o.stderr.is_empty(), "hook {kind} must print nothing: {}", text(&o));
        }
    }
}

#[test]
fn uninstall_restores_every_file_exactly() {
    let e = env("uninst");
    shop(&e);
    let files = [(".claude/settings.json", "{\n  \"model\": \"opus\"\n}"), (".cursor/mcp.json", "{\n  \"mcpServers\": {\n    \"other\": {\n      \"command\": \"x\"\n    }\n  }\n}"),
                 (".codex/config.toml", "model = \"o3\"\n\n[mcp_servers.other]\ncommand = \"y\"\n")];
    for (p, t) in files { write(&e.home.join(p), t); }
    let excl = e.repo.join(".git/info/exclude");
    let excl_before = std::fs::read_to_string(&excl).unwrap_or_default();
    let o = gitta(&e, &e.repo, &["init"]);
    assert!(o.status.success() && text(&o).contains("(backup next to it)"), "{}", text(&o));
    assert!(std::fs::read_to_string(e.home.join(".claude/settings.json")).unwrap().contains("hook claude"));
    let o = gitta(&e, &e.repo, &["uninstall", "--everywhere"]);
    assert!(o.status.success(), "{}", text(&o));
    let exe = std::fs::canonicalize(env!("CARGO_BIN_EXE_gitta")).unwrap();
    assert!(text(&o).contains(&format!("rm {}", exe.display())), "names the program to delete: {}", text(&o));
    assert!(!e.repo.join(".gitta").exists());
    assert_eq!(std::fs::read_to_string(&excl).unwrap_or_default(), excl_before);
    let json = |p: &str| -> serde_json::Value { serde_json::from_str(&std::fs::read_to_string(e.home.join(p)).unwrap()).unwrap() };
    for (p, t) in files {
        if p.ends_with(".json") { assert_eq!(json(p), serde_json::from_str::<serde_json::Value>(t).unwrap(), "{p}"); }
        else { assert_eq!(std::fs::read_to_string(e.home.join(p)).unwrap(), t, "{p}"); }
    }
}

#[test]
fn init_never_writes_into_a_hooks_folder_outside_the_repository() {
    let e = env("sharedhooks");
    shop(&e);
    // core.hooksPath in the user's global config points to a folder that every repository on the machine uses.
    let shared = e.dir.join("shared-hooks");
    write(&shared.join("pre-push"), "#!/bin/sh\nexit 0\n");
    write(&e.home.join(".gitconfig"), &format!("[core]\n\thooksPath = {}\n", shared.display()));
    let o = gitta(&e, &e.repo, &["init", "--no-claude"]);
    assert!(!o.status.success() && text(&o).contains("core.hooksPath"), "{}", text(&o));
    let names: Vec<String> = std::fs::read_dir(&shared).unwrap().map(|x| x.unwrap().file_name().to_string_lossy().to_string()).collect();
    assert_eq!(names, vec!["pre-push"], "the shared folder is unchanged");
    assert_eq!(std::fs::read_to_string(shared.join("pre-push")).unwrap(), "#!/bin/sh\nexit 0\n");
    // With a hooks folder of its own, the repository is set up as usual.
    git(&e.repo, &["config", "--local", "core.hooksPath", &format!("{}/.git/hooks", e.repo.display())]);
    let o = gitta(&e, &e.repo, &["init", "--no-claude"]);
    assert!(o.status.success() && e.repo.join(".git/hooks/pre-merge-commit").exists(), "{}", text(&o));
    assert_eq!(std::fs::read_dir(&shared).unwrap().count(), 1);
}

#[test]
fn clear_errors_for_common_mistakes() {
    let e = env("errors");
    shop(&e);
    let t = text(&gitta(&e, &e.repo, &["status"]));
    assert!(t.contains("gitta init"), "{t}");
    gitta(&e, &e.repo, &["init", "--no-claude"]);
    let t = text(&gitta(&e, &e.repo, &["land", "nobody"]));
    assert!(t.contains("no agent named nobody"), "{t}");
    let t = text(&gitta(&e, &e.repo, &["frobnicate"]));
    assert!(t.contains("unknown command"), "{t}");
    std::fs::write(e.repo.join(".gitta/config.json"), "{").unwrap();
    let o = gitta(&e, &e.repo, &["status"]);
    assert!(!o.status.success() && text(&o).contains("damaged"), "{}", text(&o));
    assert!(gitta(&e, &e.repo, &["init", "--no-claude"]).status.success(), "init repairs damaged settings");
}

#[test]
fn mcp_server_answers_and_survives_errors() {
    let e = env("mcp");
    shop(&e);
    gitta(&e, &e.repo, &["init", "--no-claude"]);
    let a = worktree(&e, "feat");
    write(&a.join("shop/text.py"), "def shout(s):\n    return s.upper()\n\n\ndef whisper(s):\n    return s.lower()\n");
    let p = a.to_str().unwrap();
    let reqs = [
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}),
        serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        serde_json::json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "gitta_sync", "arguments": {"path": p, "agent": "nobody"}}}),
        serde_json::json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "gitta_status", "arguments": {"path": p}}}),
        serde_json::json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "gitta_claim", "arguments": {"path": p, "target": "shop.text.shout"}}}),
    ];
    let input: String = reqs.iter().map(|r| r.to_string() + "\n").collect();
    let o = gitta_in(&e, &e.home, &["mcp"], &input);
    let lines: Vec<serde_json::Value> = String::from_utf8_lossy(&o.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 5, "one answer per request with an id: {lines:?}");
    assert_eq!(lines[1]["result"]["tools"].as_array().unwrap().len(), 5);
    assert_eq!(lines[2]["result"]["isError"], true, "an unknown agent is an error result, not a dead server");
    assert!(lines[2]["result"]["content"][0]["text"].as_str().unwrap().contains("no agent named nobody"));
    assert!(lines[3]["result"]["content"][0]["text"].as_str().unwrap().contains("feat"), "status after an error: {}", lines[3]);
    assert!(lines[4]["result"]["isError"].is_null(), "claim without agent uses the workspace's agent: {}", lines[4]);
}

#[test]
fn after_a_landing_synced_files_are_not_reported_as_changes() {
    let e = env("synced");
    shop(&e);
    gitta(&e, &e.repo, &["init", "--no-claude"]);
    let (a, b) = (worktree(&e, "vat"), worktree(&e, "words"));
    write(&a.join("shop/pricing.py"), &PRICING.replace("0.25", "0.12"));
    write(&b.join("shop/text.py"), "def shout(s):\n    return s.upper()\n\n\ndef whisper(s):\n    return s.lower()\n");
    assert!(gitta(&e, &e.repo, &["land", "vat"]).status.success());
    let st = text(&gitta(&e, &e.repo, &["status"]));
    let words = st.lines().find(|l| l.trim_start().starts_with("words")).unwrap_or("").to_string();
    assert!(words.contains("1 file changed"), "words changed only text.py; pricing.py came from the landing: {st}");
    let vat = st.lines().find(|l| l.trim_start().starts_with("vat")).unwrap_or("").to_string();
    assert!(vat.contains("merged into main at") && vat.contains("no new changes since"), "vat's work is on main now: {st}");
}

#[test]
fn a_second_landing_never_undoes_work_that_landed_in_between() {
    let e = env("noundo");
    shop(&e);
    gitta(&e, &e.repo, &["init", "--no-claude"]);
    let (a, b) = (worktree(&e, "vat"), worktree(&e, "words"));
    write(&a.join("shop/pricing.py"), &PRICING.replace("0.25", "0.12"));
    write(&b.join("shop/text.py"), "def shout(s):\n    return s.upper()\n\n\ndef whisper(s):\n    return s.lower()\n");
    assert!(gitta(&e, &e.repo, &["land", "vat"]).status.success());
    assert!(gitta(&e, &e.repo, &["land", "words"]).status.success());
    // words keeps working and lands again: the VAT change must survive.
    write(&b.join("shop/text.py"), "def shout(s):\n    return s.upper()\n\n\ndef whisper(s):\n    return s.lower()\n\n\ndef calm(s):\n    return s.capitalize()\n");
    let o = gitta(&e, &e.repo, &["land", "words"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(std::fs::read_to_string(e.repo.join("shop/pricing.py")).unwrap().contains("0.12"), "the landed VAT change was undone");
    assert!(std::fs::read_to_string(b.join("shop/pricing.py")).unwrap().contains("0.12"), "the landed agent's workspace was not brought up to date");
}

#[test]
fn work_that_reaches_main_with_plain_git_is_noticed_by_agents() {
    let e = env("plaingit");
    shop(&e);
    gitta(&e, &e.repo, &["init", "--no-claude"]);
    let b = worktree(&e, "label");
    write(&b.join("shop/label.py"), "from shop.pricing import net_to_gross\n\n\ndef label(x):\n    return f\"{net_to_gross(x)} kr\"\n");
    // Someone changes main the ordinary way: edit and commit in the main checkout. No gitta command.
    write(&e.repo.join("shop/pricing.py"), &PRICING.replace("return round(net * (1 + VAT_RATE), 2)", "return int(net * (1 + VAT_RATE) * 100)"));
    git(&e.repo, &["commit", "-qam", "prices in ore"]);
    // The agent's next step (its hook) brings both the warning and the current code.
    let ev = format!("{{\"cwd\": {:?}, \"tool_name\": \"Edit\"}}", b.to_str().unwrap());
    let o = gitta_in(&e, &b, &["hook", "claude"], &ev);
    let out = String::from_utf8_lossy(&o.stdout);
    // Work on main is no note to the agent: the tests at merge time decide. The log has it for people.
    assert!(!out.contains("net_to_gross"), "hook output: {out}");
    assert!(std::fs::read_to_string(e.repo.join(".gitta/gitta.log")).unwrap().contains("net_to_gross"));
    assert!(!std::fs::read_to_string(b.join("shop/pricing.py")).unwrap().contains("* 100"), "an adopted worktree gets the note, never foreign uncommitted files");
    assert!(std::fs::read_to_string(b.join("shop/label.py")).unwrap().contains("def label"), "the agent's own work is untouched");
}

#[test]
fn audit_trail_records_who_what_and_checks_and_shows_tampering() {
    let e = env("audit");
    shop(&e);
    git(&e.repo, &["config", "user.name", "Anna Berg"]);
    git(&e.repo, &["config", "user.email", "anna@example.com"]);
    assert!(gitta(&e, &e.repo, &["init", "--no-claude", "--gate", "python3 -c 'import shop.text'"]).status.success());
    // A refused landing: the agent put a credential in the code.
    let bad = worktree(&e, "leaky");
    write(&bad.join("shop/text.py"), "API_KEY = \"sk-live-4f9a8b7c6d5e4f3a2b1c0d9e8f7a6b5c\"\n\ndef shout(s):\n    return s.upper()\n");
    assert!(!gitta(&e, &e.repo, &["land", "leaky"]).status.success());
    // A good landing, with an agent commit carrying a Co-Authored-By trailer.
    let a = worktree(&e, "feat");
    write(&a.join("shop/text.py"), "def shout(s):\n    return s.upper()\n\n\ndef whisper(s):\n    return s.lower()\n");
    git(&a, &["add", "-A"]);
    git(&a, &["commit", "-qm", "Add whisper\n\nCo-Authored-By: Claude <noreply@anthropic.com>"]);
    let o = gitta(&e, &e.repo, &["land", "feat"]);
    assert!(o.status.success(), "{}", text(&o));

    // Work that reaches main outside Gitta (a merged pull request): recorded at the next agent step.
    let pr = e.dir.join("wt-pr");
    git(&e.repo, &["worktree", "add", "-q", "-b", "pr", pr.to_str().unwrap()]);
    write(&pr.join("shop/cart.py"), &format!("{CART}\n\ndef empty():\n    return 0\n"));
    git(&pr, &["add", "-A"]);
    git(&pr, &["commit", "-qm", "Add empty cart\n\nCo-Authored-By: Codex <codex@openai.com>"]);
    git(&e.repo, &["merge", "-q", "--no-ff", "-m", "Merge pull request #7", "pr"]);
    gitta_in(&e, &a, &["hook", "claude"], &format!("{{\"cwd\": {:?}}}", a.to_str().unwrap()));
    let log = text(&gitta(&e, &e.repo, &["audit", "--json"]));
    let events: Vec<serde_json::Value> = log.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let kinds: Vec<&str> = events.iter().map(|e| e["event"].as_str().unwrap()).collect();
    let reached = events.iter().find(|e| e["event"] == "reached_main").expect("merged pull request recorded");
    assert_eq!(reached["data"]["ai_co_authored"], 1, "{}", reached["data"]);
    assert!(reached["data"]["commits"].to_string().contains("Merge pull request #7"));
    for k in ["agent_started", "land_refused", "checks", "landed"] { assert!(kinds.contains(&k), "missing {k}: {kinds:?}"); }
    let refused = events.iter().find(|e| e["event"] == "land_refused").unwrap();
    assert_eq!(refused["agent"], "leaky");
    assert_eq!(refused["data"]["check"], "credentials");
    let landed = events.iter().find(|e| e["event"] == "landed").unwrap();
    assert_eq!(landed["actor"]["name"], "Anna Berg", "who landed it");
    assert_eq!(landed["agent"], "feat");
    assert!(landed["data"]["commits"].to_string().contains("Co-Authored-By: Claude"), "agent commits and trailers: {}", landed["data"]["commits"]);
    assert!(landed["data"]["checks"].to_string().contains("\"tests\""), "the test run is recorded");
    assert_eq!(landed["data"]["files"][0]["path"], "shop/text.py");
    // The landed commit carries a Git note pointing at the audit event; no commit was changed for it.
    let head = landed["data"]["to"].as_str().unwrap().to_string();
    let note = git(&e.repo, &["notes", "--ref=gitta", "show", &head]);
    assert!(note.contains("\"agent\": \"feat\"") && note.contains(landed["hash"].as_str().unwrap()), "{note}");
    // Readable table and CSV for auditors.
    assert!(text(&gitta(&e, &e.repo, &["audit"])).contains("landed"));
    assert!(text(&gitta(&e, &e.repo, &["audit", "--csv"])).starts_with("time,event,agent,person"));
    // Intact, then tampered with.
    let ok = gitta(&e, &e.repo, &["audit", "verify"]);
    assert!(ok.status.success() && text(&ok).contains("intact"), "{}", text(&ok));
    let f = e.repo.join(".gitta/audit.jsonl");
    let t = std::fs::read_to_string(&f).unwrap().replace("\"check\":\"credentials\"", "\"check\":\"merge\"");
    std::fs::write(&f, t).unwrap();
    let broken = gitta(&e, &e.repo, &["audit", "verify"]);
    assert!(!broken.status.success() && text(&broken).contains("changed after it was written"), "{}", text(&broken));
}

#[test]
fn open_source_build_has_no_team_server() {
    let e = env("oss");
    let o = gitta(&e, &e.repo, &["server"]);
    assert!(!o.status.success() && text(&o).contains("Gitta Enterprise"), "{}", text(&o));
}

#[test]
fn a_changed_meaning_is_information_and_the_tests_decide() {
    let e = env("adapt");
    shop(&e);
    gitta(&e, &e.repo, &["init", "--no-claude"]);
    let (a, b) = (worktree(&e, "vat"), worktree(&e, "label"));
    write(&a.join("shop/pricing.py"), &PRICING.replace("return round(net * (1 + VAT_RATE), 2)", "return int(net * (1 + VAT_RATE) * 100)"));
    let uses = "from shop.pricing import net_to_gross\n\n\ndef label(x):\n    return f\"{net_to_gross(x)} kr\"\n";
    write(&b.join("shop/label.py"), uses);
    assert!(gitta(&e, &e.repo, &["land", "vat"]).status.success());
    let s = text(&gitta(&e, &e.repo, &["sync", "label"]));
    assert!(s.contains("vat was merged into main and changed `net_to_gross`") && !s.contains("--fits"), "{s}");
    // `gitta status` tells the story as a note: nothing waits for a confirmation.
    let st = text(&gitta(&e, &e.repo, &["status"]));
    assert!(st.contains("label 1 file changed, ready to merge") && st.contains("Check that shop/label.py still works with it."), "{st}");
    let o = gitta(&e, &e.repo, &["land", "label"]);
    assert!(o.status.success(), "{}", text(&o));
}

#[test]
fn csharp_and_kotlin_changed_meaning_is_reported() {
    let e = env("cskt");
    let vat_cs = "namespace Shop.Pricing\n{\n    public static class Vat\n    {\n        public const decimal Rate = 0.25m;\n\n        public static decimal NetToGross(decimal net)\n        {\n            return net * (1 + Rate);\n        }\n    }\n}\n";
    let vat_kt = "package shop.pricing\n\nfun netToGross(net: Double): Double {\n    return net * 1.25\n}\n";
    write(&e.repo.join("src/Shop.Pricing/Vat.cs"), vat_cs);
    write(&e.repo.join("app/src/main/kotlin/shop/pricing/Vat.kt"), vat_kt);
    write(&e.repo.join("app/src/main/kotlin/shop/text/Text.kt"), "package shop.text\n\nfun shout(s: String) = s.uppercase()\n");
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "base"]);
    gitta(&e, &e.repo, &["init", "--no-claude"]);
    let (a, b, c, d, f) = (worktree(&e, "vat"), worktree(&e, "label"), worktree(&e, "cents"), worktree(&e, "badge"), worktree(&e, "loud"));
    write(&f.join("app/src/main/kotlin/shop/text/Text.kt"), "package shop.text\n\nfun shout(s: String) = s.uppercase()\n\nfun loud(s: String) = shout(s) + \"!\"\n");
    // C#: the VAT rate changes; new code in another namespace reads it.
    write(&a.join("src/Shop.Pricing/Vat.cs"), &vat_cs.replace("0.25m", "0.12m"));
    write(&b.join("src/Shop.Web/Label.cs"), "using Shop.Pricing;\n\nnamespace Shop.Web;\n\npublic static class Label\n{\n    public static string Text(decimal x) => $\"{Vat.NetToGross(x)} kr incl. 25% VAT\";\n}\n");
    // Kotlin: the function now returns cents; new code in another package calls it through an aliased import.
    write(&c.join("app/src/main/kotlin/shop/pricing/Vat.kt"), &vat_kt.replace("return net * 1.25", "return net * 125"));
    write(&d.join("app/src/main/kotlin/shop/web/Badge.kt"), "package shop.web\n\nimport shop.pricing.netToGross as gross\n\nfun badge(x: Double) = \"${gross(x)} kr\"\n");
    assert!(gitta(&e, &e.repo, &["land", "vat"]).status.success());
    let o = gitta(&e, &e.repo, &["land", "label"]);
    assert!(o.status.success() && text(&o).contains("Shop.Pricing.Vat"), "label relies on the old VAT rate and is reported, and the tests decide: {}", text(&o));
    assert!(gitta(&e, &e.repo, &["land", "cents"]).status.success());
    let o = gitta(&e, &e.repo, &["land", "badge"]);
    assert!(o.status.success() && text(&o).contains("netToGross"), "badge relies on the old result and is reported, and the tests decide: {}", text(&o));
    let o = gitta(&e, &e.repo, &["land", "loud"]);
    assert!(o.status.success(), "an unrelated agent still lands: {}", text(&o));
}

/// `gitta approve` asks a person at a terminal: run it on a pseudo-terminal, outside any agent session.
fn approve(e: &Env, agent: &str, reason: &str) -> Output {
    let bin = env!("CARGO_BIN_EXE_gitta");
    let mut c = if cfg!(target_os = "macos") { let mut c = Command::new("script"); c.args(["-q", "/dev/null", bin, "approve", agent, reason]); c }
                else { let mut c = Command::new("script"); c.args(["-qec", &format!("{bin} approve {agent} '{reason}'"), "/dev/null"]); c };
    for v in ["CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT", "CURSOR_AGENT", "CODEX_SANDBOX", "CODEX_SANDBOX_NETWORK_DISABLED", "GITTA_AGENT"] { c.env_remove(v); }
    let mut ch = c.current_dir(&e.repo).env("HOME", &e.home).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    // Answer once the prompt is up, and keep the terminal open until it is read (an early end of input reads as no answer).
    let mut stdin = ch.stdin.take().unwrap();
    let answer = format!("{agent}\n");
    let t = std::thread::spawn(move || { std::thread::sleep(std::time::Duration::from_millis(700)); let _ = stdin.write_all(answer.as_bytes()); std::thread::sleep(std::time::Duration::from_millis(1500)); });
    let o = ch.wait_with_output().unwrap();
    t.join().ok();
    o
}

#[test]
fn a_removed_login_check_is_a_security_hazard_and_needs_a_person() {
    let e = env("security");
    const VIEWS: &str = "from django.contrib.auth.decorators import login_required\n\n\n@login_required\ndef export_orders(request):\n    return list_orders(request)\n\n\ndef list_orders(request):\n    return []\n";
    write(&e.repo.join("shop/__init__.py"), "");
    write(&e.repo.join("shop/views.py"), VIEWS);
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "base"]);
    gitta(&e, &e.repo, &["init", "--no-claude"]);
    let (a, b) = (worktree(&e, "open-export"), worktree(&e, "reports"));
    write(&a.join("shop/views.py"), &VIEWS.replace("@login_required\ndef export", "def export"));
    write(&b.join("shop/reports.py"), "from shop.views import export_orders\n\n\ndef nightly_report(request):\n    return export_orders(request)\n");
    for (w, m) in [(&a, "open the export"), (&b, "nightly report")] { git(w, &["add", "-A"]); git(w, &["commit", "-qm", m]); }
    // In session: the agent whose new code calls the endpoint is told at its next step.
    let o = gitta_in(&e, &b, &["hook", "claude"], &format!("{{\"cwd\": {:?}}}", b.to_str().unwrap()));
    let out = text(&o);
    assert!(out.contains("SECURITY (high severity)") && out.contains("removed the login check (`@login_required`) from `shop.views.export_orders`")
        && out.contains("without a logged-in user"), "hook output: {out}");
    let st = text(&gitta(&e, &e.repo, &["status"]));
    assert!(st.contains("SECURITY potentially affected: open-export removed the login check") && st.contains("SECURITY: shop/views.py by open-export"), "{st}");
    // Pull request check: a Security line.
    let pr = text(&gitta(&e, &e.repo, &["pr-check", "--pr", "reports", "--other", "open-export"]));
    assert!(pr.contains("- **Security** with open-export:"), "{pr}");
    let js = text(&gitta(&e, &e.repo, &["pr-check", "--pr", "reports", "--other", "open-export", "--json"]));
    assert!(js.contains("\"security\": [") && js.contains("removed the login check"), "{js}");
    // Removing the decorator from an existing endpoint needs a person's approval; an agent cannot give it.
    let o = gitta(&e, &e.repo, &["land", "open-export"]);
    assert!(!o.status.success() && text(&o).contains("land refused") && text(&o).contains("shop/views.py"), "{}", text(&o));
    let o = Command::new(env!("CARGO_BIN_EXE_gitta")).env("GITTA_NO_NOTIFY", "1").env("GITTA_NO_PROMPT", "1").env("GITTA_WEAKENED_CHECKS", "block").args(["approve", "open-export", "ok"]).current_dir(&e.repo).env("HOME", &e.home).env("CLAUDECODE", "1").output().unwrap();
    assert!(!o.status.success() && text(&o).contains("not inside an agent session"), "{}", text(&o));
    let o = approve(&e, "open-export", "the export is public by design");
    assert!(o.status.success(), "{}", text(&o));
    assert!(gitta(&e, &e.repo, &["land", "open-export"]).status.success());
    // The other agent's call now runs without the login check: refused until its code changes or a person approves.
    let o = gitta(&e, &e.repo, &["land", "reports"]);
    assert!(!o.status.success() && text(&o).contains("land refused") && text(&o).contains("shop/reports.py"), "{}", text(&o));
    let s = text(&gitta(&e, &e.repo, &["sync", "reports"]));
    assert!(s.contains("export_orders"), "{s}");
    assert!(approve(&e, "reports", "reports run as a logged-in job").status.success());
    let o = gitta(&e, &e.repo, &["land", "reports"]);
    assert!(o.status.success(), "{}", text(&o));
    let audit = text(&gitta(&e, &e.repo, &["audit", "--json"]));
    for want in ["\"check\":\"security\"", "SECURITY_HAZARD", "SECURITY_WEAKENED", "\"kind\":\"SECURITY\""] { assert!(audit.replace(": ", ":").contains(want), "{want} missing in audit"); }
}

/// Set an agent's recorded base in .gitta/config.json (as if its start commit had been recorded long ago).
fn set_base(e: &Env, agent: &str, base: &str) {
    let p = e.repo.join(".gitta/config.json");
    let mut c: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    for a in c["agents"].as_array_mut().unwrap() { if a["name"] == agent { a["base"] = base.into(); } }
    std::fs::write(&p, serde_json::to_string_pretty(&c).unwrap()).unwrap();
}

#[test]
fn a_worktree_whose_base_commit_is_gone_is_skipped_with_a_fix() {
    let e = env("badobj");
    shop(&e);
    gitta(&e, &e.repo, &["init", "--no-claude"]);
    let (a, b) = (worktree(&e, "lost"), worktree(&e, "fine"));
    write(&a.join("shop/text.py"), "def shout(s):\n    return s.upper() + '!'\n");
    write(&b.join("shop/new.py"), "X = 1\n");
    gitta(&e, &e.repo, &["status"]);
    // A commit that once was the base, then removed for good: a rewritten branch, an expired reflog, `git gc`.
    git(&e.repo, &["switch", "-q", "-c", "tmp"]);
    write(&e.repo.join("gone.txt"), "x\n");
    git(&e.repo, &["add", "gone.txt"]);
    git(&e.repo, &["commit", "-qm", "gone"]);
    let gone = git(&e.repo, &["rev-parse", "HEAD"]);
    git(&e.repo, &["switch", "-q", "main"]);
    git(&e.repo, &["branch", "-qD", "tmp"]);
    git(&e.repo, &["reflog", "expire", "--expire=now", "--all"]);
    git(&e.repo, &["gc", "-q", "--prune=now"]);
    set_base(&e, "lost", &gone);
    let o = gitta(&e, &e.repo, &["status"]);
    let t = text(&o);
    assert!(o.status.success(), "status must not fail on one bad worktree: {t}");
    assert!(t.contains("skipped lost") && t.contains("no longer in this repository") && t.contains("gitta adopt --reset lost"), "{t}");
    assert!(t.contains("fine") && t.contains("1 file changed"), "the other agents are still shown: {t}");
    let j: serde_json::Value = serde_json::from_slice(&gitta(&e, &e.repo, &["status", "--json"]).stdout).unwrap();
    assert_eq!(j["skipped"][0]["name"], "lost");
    // The agent itself hears it once, and its hook never fails.
    let ev = format!("{{\"cwd\": {:?}}}", a.to_str().unwrap());
    let o = gitta_in(&e, &a, &["hook", "claude"], &ev);
    assert!(o.status.success() && String::from_utf8_lossy(&o.stdout).contains("cannot check this workspace"), "{}", text(&o));
    assert!(gitta_in(&e, &a, &["hook", "claude"], &ev).stdout.is_empty(), "the note is given once");
    // The fix named in the note works.
    assert!(gitta(&e, &e.repo, &["adopt", "--reset", "lost"]).status.success());
    let t = text(&gitta(&e, &e.repo, &["status"]));
    assert!(!t.contains("skipped") && t.contains("lost"), "{t}");
}

#[test]
fn a_deleted_or_locked_worktree_never_stops_the_others() {
    let e = env("gone");
    shop(&e);
    gitta(&e, &e.repo, &["init", "--no-claude"]);
    let (a, b, c) = (worktree(&e, "deleted"), worktree(&e, "locked"), worktree(&e, "detached"));
    gitta(&e, &e.repo, &["status"]);
    std::fs::remove_dir_all(&a).unwrap();
    git(&e.repo, &["worktree", "lock", b.to_str().unwrap(), "--reason", "on another disk"]);
    write(&b.join("shop/new.py"), "X = 1\n");
    git(&c, &["checkout", "-q", "--detach", "HEAD"]);
    let o = gitta(&e, &e.repo, &["status"]);
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(t.contains("skipped deleted") && t.contains("is gone") && t.contains("git worktree prune"), "{t}");
    assert!(t.contains("locked") && t.contains("1 file changed") && t.contains("detached"), "{t}");
    // Landing another agent still works and leaves the gone one alone.
    git(&b, &["add", "-A"]);
    git(&b, &["commit", "-qm", "x"]);
    let o = gitta(&e, &e.repo, &["land", "locked"]);
    assert!(o.status.success(), "{}", text(&o));
}

#[test]
fn every_command_answers_help_and_refuses_unknown_options_without_side_effects() {
    let e = env("help");
    shop(&e);
    for cmd in ["init", "run", "up", "status", "sync", "check", "claim", "release", "approve", "land", "adopt", "pr-check", "hook", "scan", "mcp", "watch", "install", "login", "team", "audit", "report", "down", "uninstall"] {
        for h in ["--help", "-h"] {
            let o = gitta(&e, &e.repo, &[cmd, h]);
            assert!(o.status.success() && String::from_utf8_lossy(&o.stdout).contains(&format!("gitta {cmd}")), "{cmd} {h}: {}", text(&o));
        }
        let o = gitta(&e, &e.repo, &[cmd, "--no-such-option"]);
        assert!(!o.status.success() && text(&o).contains("--no-such-option"), "{cmd} must refuse an unknown option: {}", text(&o));
        assert!(!e.repo.join(".gitta").exists(), "{cmd} changed something");
    }
    // Free text stays free text.
    gitta(&e, &e.repo, &["init", "--no-claude"]);
    worktree(&e, "feat");
    assert!(gitta(&e, &e.repo, &["claim", "feat", "shop.pricing", "-", "rewriting", "it"]).status.success());
}

/// An edit in another worktree made without a gitta hook (a person, a tool) shows up at the next hook of any
/// agent, not only after the cached scan of that worktree expires.
#[test]
fn hook_sees_an_edit_made_without_a_hook_at_once() {
    let e = env("nohook");
    shop(&e);
    gitta(&e, &e.repo, &["init", "--no-claude"]);
    let (a, b) = (worktree(&e, "vat"), worktree(&e, "label"));
    write(&b.join("shop/label.py"), "from shop.pricing import net_to_gross\n\n\ndef label(x):\n    return f\"{net_to_gross(x)} kr\"\n");
    let ev = format!("{{\"cwd\": {:?}, \"tool_name\": \"Edit\"}}", b.to_str().unwrap());
    let o = gitta_in(&e, &b, &["hook", "claude"], &ev);
    assert!(o.status.success() && !String::from_utf8_lossy(&o.stdout).contains("net_to_gross"), "nothing to warn about yet");
    // A's worktree was clean and cached as clean. Now its file changes, and A's hook does not run.
    write(&a.join("shop/pricing.py"), &PRICING.replace("return round(net * (1 + VAT_RATE), 2)", "return int(net * (1 + VAT_RATE) * 100)"));
    let o = gitta_in(&e, &b, &["hook", "claude"], &ev);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success() && out.contains("net_to_gross"), "B's next hook reports A's edit: {out}");
}

/// Many worktrees: a hook does work only for its own agent. The others come from their last scan, so the
/// number of Git processes per hook does not grow with the number of worktrees.
#[test]
fn the_hook_does_not_rescan_unchanged_worktrees() {
    let e = env("many");
    shop(&e);
    gitta(&e, &e.repo, &["init", "--no-claude"]);
    let wts: Vec<PathBuf> = (0..62).map(|i| worktree(&e, &format!("w{i}"))).collect();
    write(&wts[0].join("shop/text.py"), "def shout(s):\n    return s.upper() + '!'\n");
    write(&wts[1].join("shop/cart.py"), &CART.replace("return net_to_gross(x)", "return net_to_gross(x) + 1"));
    // Work that reached main since: every agent's base is behind (the case that made status slow).
    write(&e.repo.join("shop/pricing.py"), &PRICING.replace("0.25", "0.12"));
    git(&e.repo, &["commit", "-qam", "landed"]);
    let t0 = std::time::Instant::now();
    assert!(gitta(&e, &e.repo, &["status"]).status.success());
    let first = t0.elapsed();
    // Count Git processes through a wrapper first on PATH.
    let real = String::from_utf8_lossy(&Command::new("sh").args(["-c", "command -v git"]).output().unwrap().stdout).trim().to_string();
    let bin = e.dir.join("bin"); std::fs::create_dir_all(&bin).unwrap();
    let log = e.dir.join("git.log");
    write(&bin.join("git"), &format!("#!/bin/sh\necho \"$*\" >> {:?}\nexec {real:?} \"$@\"\n", log.to_str().unwrap()));
    Command::new("chmod").args(["+x", bin.join("git").to_str().unwrap()]).status().unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let run = |dir: &Path, args: &[&str], input: &str| -> (Output, Vec<String>) {
        let _ = std::fs::remove_file(&log);
        let mut c = Command::new(env!("CARGO_BIN_EXE_gitta")).env("GITTA_NO_NOTIFY", "1").env("GITTA_NO_PROMPT", "1").env("GITTA_WEAKENED_CHECKS", "block").args(args).current_dir(dir).env("HOME", &e.home).env("PATH", &path)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        c.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        (c.wait_with_output().unwrap(), std::fs::read_to_string(&log).unwrap_or_default().lines().map(String::from).collect())
    };
    let ev = format!("{{\"cwd\": {:?}}}", wts[1].to_str().unwrap());
    let t0 = std::time::Instant::now();
    let (o, calls) = run(&wts[1], &["hook", "claude"], &ev);
    let hook = t0.elapsed();
    assert!(o.status.success());
    // This first hook also syncs the landed work in and records it: still a fixed number, whatever the worktree count.
    assert!(calls.len() <= 25, "hook with 62 worktrees ran {} git processes: {calls:#?}", calls.len());
    assert!(calls.iter().filter(|c| c.contains(" status ")).all(|c| c.contains("wt-w1 ")), "only the hook's own worktree is scanned: {calls:#?}");
    let (o, calls) = run(&wts[1], &["hook", "claude"], &ev);
    assert!(o.status.success());
    assert!(calls.len() <= 12, "a repeat hook ran {} git processes: {calls:#?}", calls.len());
    let (o, calls) = run(&e.repo, &["status"], "");
    assert!(o.status.success());
    // One scan per worktree, plus a handful for the whole run and for what holds each agent with changes;
    // nothing more per unchanged worktree.
    assert!(calls.len() <= wts.len() + 22, "status ran {} git processes: {calls:#?}", calls.len());
    assert!(hook < std::time::Duration::from_secs(5) && first < std::time::Duration::from_secs(60), "hook {hook:?}, status {first:?}");
}

#[cfg(feature = "mainframe")]
#[test]
fn cobol_copybook_change_is_reported() {
    let e = env("cobol");
    let fx = |s: &str| s.lines().map(|l| if l.is_empty() { String::new() } else { format!("       {l}") }).collect::<Vec<_>>().join("\n") + "\n";
    let custrec = fx("01  CUSTOMER-RECORD.\n    05  CUSTOMER-ID       PIC 9(6).\n    05  CUSTOMER-NAME     PIC X(30).\n    05  CUSTOMER-BALANCE  PIC S9(7)V99 COMP-3.\n");
    let billing = fx("IDENTIFICATION DIVISION.\nPROGRAM-ID. BILLING.\nDATA DIVISION.\nWORKING-STORAGE SECTION.\nCOPY CUSTREC.\nPROCEDURE DIVISION.\nMAIN-PARA.\n    DISPLAY CUSTOMER-NAME\n    GOBACK.\n");
    write(&e.repo.join("copybook/CUSTREC.cpy"), &custrec);
    write(&e.repo.join("src/BILLING.cbl"), &billing);
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "base"]);
    gitta(&e, &e.repo, &["init", "--no-claude"]);
    let (a, b, c) = (worktree(&e, "shrink"), worktree(&e, "interest"), worktree(&e, "names"));
    // Agent shrink: the balance loses two integer digits in the shared copybook.
    write(&a.join("copybook/CUSTREC.cpy"), &custrec.replace("S9(7)V99", "S9(5)V99"));
    // Agent interest: a new paragraph in another program adds interest to the balance.
    write(&b.join("src/BILLING.cbl"), &(billing.replace("    GOBACK.\n", "    PERFORM ADD-INTEREST\n           GOBACK.\n") + &fx("ADD-INTEREST.\n    COMPUTE CUSTOMER-BALANCE = CUSTOMER-BALANCE * 1.015.\n")));
    // Agent names: unrelated work that only shows the name.
    write(&c.join("src/BILLING.cbl"), &billing.replace("    DISPLAY CUSTOMER-NAME\n", "    DISPLAY 'CUSTOMER ' CUSTOMER-NAME\n"));
    assert!(gitta(&e, &e.repo, &["land", "shrink"]).status.success());
    let o = gitta(&e, &e.repo, &["land", "interest"]);
    assert!(o.status.success() && text(&o).contains("CUSTOMER-BALANCE") && text(&o).contains("shrink was merged"), "interest relies on the old balance PIC: reported, the tests decide: {}", text(&o));
    // names edits the same lines of BILLING.cbl as interest: a plain text conflict, refused until it syncs.
    assert!(!gitta(&e, &e.repo, &["land", "names"]).status.success());
}

#[cfg(feature = "mainframe")]
#[test]
fn hook_warns_live_on_a_copybook_in_a_folder_listed_in_gitta_copybooks() {
    let e = env("cobol-dirs");
    let fx = |s: &str| s.lines().map(|l| if l.is_empty() { String::new() } else { format!("       {l}") }).collect::<Vec<_>>().join("\n") + "\n";
    let custrec = fx("01  CUSTOMER-RECORD.\n    05  CUSTOMER-ID       PIC 9(6).\n    05  CUSTOMER-BALANCE  PIC S9(7)V99 COMP-3.\n");
    let billing = fx("IDENTIFICATION DIVISION.\nPROGRAM-ID. BILLING.\nDATA DIVISION.\nWORKING-STORAGE SECTION.\nCOPY CUSTREC.\nPROCEDURE DIVISION.\nMAIN-PARA.\n    DISPLAY CUSTOMER-ID\n    GOBACK.\n");
    // A member copied from a PDS: no extension, in a folder only `.gitta-copybooks` names.
    write(&e.repo.join("legacy/copylib/CUSTREC"), &custrec);
    write(&e.repo.join(".gitta-copybooks"), "legacy/copylib  # copied members\n");
    write(&e.repo.join("src/BILLING.cbl"), &billing);
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "base"]);
    gitta(&e, &e.repo, &["init", "--no-claude"]);
    let (a, b) = (worktree(&e, "shrink"), worktree(&e, "interest"));
    write(&a.join("legacy/copylib/CUSTREC"), &custrec.replace("S9(7)V99", "S9(5)V99"));
    write(&b.join("src/BILLING.cbl"), &(billing.replace("    GOBACK.\n", "    PERFORM ADD-INTEREST\n           GOBACK.\n") + &fx("ADD-INTEREST.\n    COMPUTE CUSTOMER-BALANCE = CUSTOMER-BALANCE * 1.015.\n")));
    let ev = format!("{{\"cwd\": {:?}, \"tool_name\": \"Edit\"}}", b.to_str().unwrap());
    let o = gitta_in(&e, &b, &["hook", "claude"], &ev);
    assert!(o.status.success());
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("CUSTOMER-BALANCE") && out.contains("additionalContext"), "hook output: {out}");
}

#[cfg(feature = "mainframe")]
#[test]
fn genapp_widened_commarea_field_refuses_a_new_use() {
    // IBM CICS GenApp (EPL-2.0), see gitta-core/tests/fixtures/genapp/NOTICE.
    let e = env("genapp");
    let read = |f: &str| std::fs::read_to_string(format!("{}/../gitta-core/tests/fixtures/genapp/{f}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let (cpy, upd) = (read("lgcmarea.cpy"), read("lgucdb01.cbl"));
    write(&e.repo.join("base/src/lgcmarea.cpy"), &cpy);
    write(&e.repo.join("base/src/lgucdb01.cbl"), &upd);
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "base"]);
    gitta(&e, &e.repo, &["init", "--no-claude"]);
    let (a, b) = (worktree(&e, "widen"), worktree(&e, "display"));
    write(&a.join("base/src/lgcmarea.cpy"), &cpy.replace("CA-EMAIL-ADDRESS      PIC X(100)", "CA-EMAIL-ADDRESS      PIC X(254)"));
    let para = "       UPDATE-CUSTOMER-INFO.\n";
    write(&b.join("base/src/lgucdb01.cbl"), &upd.replacen(para, &format!("{para}           DISPLAY CA-EMAIL-ADDRESS\n"), 1));
    let st = text(&gitta(&e, &e.repo, &["status"]));
    assert!(st.contains("LGCMAREA.CA-EMAIL-ADDRESS"), "{st}");
    assert!(gitta(&e, &e.repo, &["land", "widen"]).status.success());
    let o = gitta(&e, &e.repo, &["land", "display"]);
    assert!(o.status.success() && text(&o).contains("LGCMAREA.CA-EMAIL-ADDRESS"), "a new use of the widened email is reported, the tests decide: {}", text(&o));
}

#[cfg(feature = "mainframe")]
#[test]
fn a_new_shell_step_in_jcl_needs_a_person() {
    let e = env("jcl-sec");
    const JOB: &str = "//NIGHTLY  JOB (ACCT),'NIGHTLY',CLASS=A,MSGCLASS=X\n//COPY     EXEC PGM=IEBGENER\n//SYSUT1   DD DSN=APP.ORDERS,DISP=SHR\n//SYSUT2   DD DSN=APP.ORDERS.BACKUP,DISP=SHR\n//SYSIN    DD DUMMY\n//SYSPRINT DD SYSOUT=*\n";
    write(&e.repo.join("jcl/NIGHTLY.jcl"), JOB);
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "base"]);
    gitta(&e, &e.repo, &["init", "--no-claude"]);
    let a = worktree(&e, "export-step");
    write(&a.join("jcl/NIGHTLY.jcl"), &(JOB.to_string() + "//EXPORT   EXEC PGM=BPXBATCH,PARM='SH /u/app/export.sh'\n//STDOUT   DD SYSOUT=*\n"));
    git(&a, &["add", "-A"]); git(&a, &["commit", "-qm", "export step"]);
    let st = text(&gitta(&e, &e.repo, &["status"]));
    assert!(st.contains("SECURITY: jcl/NIGHTLY.jcl by export-step: newly runs Unix shell commands from batch (`PGM=BPXBATCH`)"), "{st}");
    let o = gitta(&e, &e.repo, &["land", "export-step"]);
    assert!(!o.status.success() && text(&o).contains("land refused") && text(&o).contains("jcl/NIGHTLY.jcl"), "{}", text(&o));
    println!("LAND REFUSED:\n{}", text(&o));
    assert!(approve(&e, "export-step", "the export script is reviewed by ops").status.success());
    assert!(gitta(&e, &e.repo, &["land", "export-step"]).status.success());
}

#[cfg(feature = "mainframe")]
#[test]
fn a_weakened_cobol_security_check_is_a_hazard_for_the_caller() {
    let e = env("cobol-sec");
    let fx = |s: &str| s.lines().map(|l| if l.is_empty() { String::new() } else { format!("       {l}") }).collect::<Vec<_>>().join("\n") + "\n";
    let chk = fx("IDENTIFICATION DIVISION.\nPROGRAM-ID. AUTHCHK.\nDATA DIVISION.\nLINKAGE SECTION.\n01  LK-USER-LEVEL PIC 9.\nPROCEDURE DIVISION USING LK-USER-LEVEL.\n    IF LK-USER-LEVEL < 5\n       EXEC CICS ABEND ABCODE('AUTH') END-EXEC\n    END-IF\n    GOBACK.\n");
    write(&e.repo.join("src/AUTHCHK.cbl"), &chk);
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "base"]);
    gitta(&e, &e.repo, &["init", "--no-claude"]);
    let (a, b) = (worktree(&e, "relax-check"), worktree(&e, "payout"));
    write(&a.join("src/AUTHCHK.cbl"), &chk.replace("       EXEC CICS ABEND ABCODE('AUTH') END-EXEC\n", "       CONTINUE\n"));
    write(&b.join("src/PAYOUT.cbl"), &fx("IDENTIFICATION DIVISION.\nPROGRAM-ID. PAYOUT.\nDATA DIVISION.\nWORKING-STORAGE SECTION.\n01  WS-LEVEL PIC 9.\nPROCEDURE DIVISION.\nMAIN-PARA.\n    CALL 'AUTHCHK' USING WS-LEVEL\n    GOBACK.\n"));
    for (w, m) in [(&a, "relax"), (&b, "payout")] { git(w, &["add", "-A"]); git(w, &["commit", "-qm", m]); }
    let o = gitta_in(&e, &b, &["hook", "claude"], &format!("{{\"cwd\": {:?}}}", b.to_str().unwrap()));
    let out = text(&o);
    assert!(out.contains("SECURITY (high severity)") && out.contains("`AUTHCHK.AUTHCHK`"), "hook output: {out}");
    let st = text(&gitta(&e, &e.repo, &["status"]));
    println!("STATUS:\n{st}");
    assert!(st.contains("SECURITY potentially affected: relax-check removed a refusal (an error, raise or throw) from `AUTHCHK.AUTHCHK`"), "{st}");
    let o = gitta(&e, &e.repo, &["land", "relax-check"]);
    assert!(!o.status.success() && text(&o).contains("land refused") && text(&o).contains("src/AUTHCHK.cbl"), "{}", text(&o));
    assert!(approve(&e, "relax-check", "level 5 check moved to RACF").status.success());
    assert!(gitta(&e, &e.repo, &["land", "relax-check"]).status.success());
    let o = gitta(&e, &e.repo, &["land", "payout"]);
    println!("LAND PAYOUT:\n{}", text(&o));
    assert!(!o.status.success() && text(&o).contains("land refused") && text(&o).contains("src/PAYOUT.cbl"), "{}", text(&o));
}

#[test]
fn security_notes_on_added_lines_warn_live_and_high_ones_need_a_person() {
    let e = env("sec1");
    // An old risky call that was already there before any agent: never reported.
    const DB: &str = "import requests\n\n\ndef ping(url):\n    return requests.get(url, verify=False)\n";
    write(&e.repo.join("app/__init__.py"), "");
    write(&e.repo.join("app/db.py"), DB);
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "base"]);
    gitta(&e, &e.repo, &["init", "--no-claude"]);
    let (a, b, c) = (worktree(&e, "search"), worktree(&e, "devices"), worktree(&e, "lookup"));
    write(&a.join("app/db.py"), &format!("{DB}\n\ndef find(cur, name):\n    cur.execute(f\"SELECT * FROM users WHERE name = '{{name}}'\")\n    return cur.fetchall()\n"));
    write(&b.join("app/devices.py"), "import requests\n\n\ndef status(url):\n    return requests.get(url, verify=False).json()\n");
    write(&c.join("app/lookup.py"), "def by_id(cur, i):\n    cur.execute(f\"SELECT * FROM t WHERE id = {i}\")  # gitta: allow sql-text i is an int from the router\n");
    for (w, m) in [(&a, "search"), (&b, "devices"), (&c, "lookup")] { git(w, &["add", "-A"]); git(w, &["commit", "-qm", m]); }
    // Live, in the agent's session: once per finding.
    let hook = |w: &PathBuf| text(&gitta_in(&e, w, &["hook", "claude"], &format!("{{\"cwd\": {:?}}}", w.to_str().unwrap())));
    let out = hook(&a);
    println!("HOOK:\n{out}");
    assert!(out.contains("gitta: security note (medium): app/db.py:9 builds SQL from text and variables. Use a parameterised query."), "{out}");
    assert!(!out.contains("db.py:5"), "the old call was there before: {out}");
    assert!(!hook(&a).contains("security note"), "the same finding is not repeated");
    let out = hook(&b);
    assert!(out.contains("security note (high): app/devices.py:5 turns off certificate checks.") && out.contains("decides whether it stops the merge"), "{out}");
    assert!(!hook(&c).contains("security note"), "an allowed line is quiet");
    // Status, status --json and the pull request check say the same.
    let st = text(&gitta(&e, &e.repo, &["status"]));
    assert!(st.contains("security note (medium): app/db.py:9") && st.contains("[sql-text, by search]"), "{st}");
    let js = text(&gitta(&e, &e.repo, &["status", "--json"]));
    assert!(js.contains("\"security_notes\"") && js.contains("\"rule\":\"tls-off\""), "{js}");
    let pr = text(&gitta(&e, &e.repo, &["pr-check", "--pr", "devices", "--other", "search"]));
    println!("PR-CHECK:\n{pr}");
    assert!(pr.contains("- **Security**: app/devices.py:5 turns off certificate checks."), "{pr}");
    let pj = text(&gitta(&e, &e.repo, &["pr-check", "--pr", "devices", "--other", "search", "--json"]));
    assert!(pj.contains("\"security_notes\"") && pj.contains("tls-off"), "{pj}");
    // Medium lands with the note in the output; the allowed line lands and the allow is in the audit trail.
    let o = gitta(&e, &e.repo, &["land", "search"]);
    println!("LAND search:\n{}", text(&o));
    assert!(o.status.success() && text(&o).contains("gitta: security note (medium): app/db.py:9"), "{}", text(&o));
    let o = gitta(&e, &e.repo, &["land", "lookup"]);
    assert!(o.status.success() && text(&o).contains("allowed sql-text at app/lookup.py:2"), "{}", text(&o));
    // High needs a person; an agent cannot approve it.
    let o = gitta(&e, &e.repo, &["land", "devices"]);
    println!("LAND devices:\n{}", text(&o));
    assert!(!o.status.success() && text(&o).contains("turns off certificate checks") && text(&o).contains("app/devices.py"), "{}", text(&o));
    let o = Command::new(env!("CARGO_BIN_EXE_gitta")).env("GITTA_NO_NOTIFY", "1").env("GITTA_NO_PROMPT", "1").env("GITTA_WEAKENED_CHECKS", "block").args(["approve", "devices", "ok"]).current_dir(&e.repo).env("HOME", &e.home).env("CLAUDECODE", "1").output().unwrap();
    assert!(!o.status.success() && text(&o).contains("not inside an agent session"), "{}", text(&o));
    assert!(approve(&e, "devices", "the devices use self-signed certificates on the plant network").status.success());
    let o = gitta(&e, &e.repo, &["land", "devices"]);
    assert!(o.status.success(), "{}", text(&o));
    let audit = text(&gitta(&e, &e.repo, &["audit", "--json"])).replace(": ", ":");
    for want in ["\"check\":\"security rules\"", "SECURITY_RULE", "\"kind\":\"SECURITY_NOTE\"", "\"event\":\"security_allowed\"", "i is an int from the router"] { assert!(audit.contains(want), "{want} missing in audit"); }
}

#[test]
fn the_security_threshold_is_a_setting() {
    let e = env("sec1-block");
    write(&e.repo.join("app/__init__.py"), "");
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "base"]);
    gitta(&e, &e.repo, &["init", "--no-claude", "--security-block", "medium"]);
    let (a, b) = (worktree(&e, "q"), worktree(&e, "tls"));
    write(&a.join("app/q.py"), "def f(cur, i):\n    cur.execute(f\"SELECT * FROM t WHERE id = {i}\")\n");
    write(&b.join("app/t.py"), "import requests\n\n\ndef f(u):\n    return requests.get(u, verify=False)\n");
    for (w, m) in [(&a, "q"), (&b, "tls")] { git(w, &["add", "-A"]); git(w, &["commit", "-qm", m]); }
    let o = gitta(&e, &e.repo, &["land", "q"]);
    assert!(!o.status.success() && text(&o).contains("app/q.py"), "medium blocks at block = medium: {}", text(&o));
    assert!(!gitta(&e, &e.repo, &["init", "--no-claude", "--security-block", "low"]).status.success());
    gitta(&e, &e.repo, &["init", "--no-claude", "--security-block", "none"]);
    let o = gitta(&e, &e.repo, &["land", "tls"]);
    assert!(o.status.success() && text(&o).contains("security note (high): app/t.py:5"), "nothing blocks at none, and the note is still shown: {}", text(&o));
}

#[test]
fn a_nested_worktree_gets_its_own_warnings_and_is_not_the_outer_agents_change() {
    // Claude Code started inside an agent's worktree creates its worktree under that worktree's `.claude/worktrees/`.
    let e = env("nested");
    shop(&e);
    gitta(&e, &e.repo, &["init", "--no-claude"]);
    let outer = e.repo.join(".claude/worktrees/outer");
    git(&e.repo, &["worktree", "add", "-q", "-b", "outer", outer.to_str().unwrap()]);
    let inner = outer.join(".claude/worktrees/inner");
    git(&outer, &["worktree", "add", "-q", "-b", "inner", inner.to_str().unwrap()]);
    write(&outer.join("shop/pricing.py"), &PRICING.replace("return round(net * (1 + VAT_RATE), 2)", "return int(net * (1 + VAT_RATE) * 100)"));
    write(&inner.join("shop/label.py"), "from shop.pricing import net_to_gross\n\n\ndef label(x):\n    return f\"{net_to_gross(x)} kr\"\n");
    let st = text(&gitta(&e, &e.repo, &["status", "--json"]));
    assert!(!st.contains("\".claude/worktrees/inner/\""), "the nested worktree is not a change of outer: {st}");
    let hook = |dir: &Path| String::from_utf8_lossy(&gitta_in(&e, dir, &["hook", "claude"], &format!("{{\"cwd\": {:?}}}", dir.to_str().unwrap())).stdout).to_string();
    let i = hook(&inner.join("shop"));
    assert!(i.contains("net_to_gross") && i.contains("outer"), "inner hears about outer's change: {i}");
    let o = hook(&outer);
    assert!(o.contains("net_to_gross") && o.contains("inner"), "outer still gets its own warning: {o}");
    assert!(hook(&e.repo).is_empty(), "the main checkout is no agent");
}

#[test]
fn install_claude_puts_the_hooks_where_worktree_sessions_read_them() {
    let e = env("instroot");
    shop(&e);
    write(&e.repo.join(".claude/settings.local.json"), "{\n  \"permissions\": {\n    \"allow\": [\"Bash(make)\"]\n  }\n}");
    gitta(&e, &e.repo, &["init", "--no-claude", "--no-cow"]);
    let a = worktree(&e, "a");
    assert!(gitta(&e, &e.repo, &["install", "claude"]).status.success());
    let root = std::fs::read_to_string(e.repo.join(".claude/settings.local.json")).unwrap();
    assert!(root.contains("hook claude\"") && root.contains("hook claude-stop") && root.contains("Bash(make)"), "merged into the main checkout's file: {root}");
    assert!(std::fs::read_to_string(a.join(".claude/settings.local.json")).unwrap().contains("hook claude"));
    assert!(gitta(&e, &e.repo, &["install", "claude"]).status.success());
    assert_eq!(std::fs::read_to_string(e.repo.join(".claude/settings.local.json")).unwrap().matches("hook claude-stop").count(), 1, "installed once");
    assert!(gitta(&e, &e.repo, &["uninstall"]).status.success());
    let after = std::fs::read_to_string(e.repo.join(".claude/settings.local.json")).unwrap();
    assert!(!after.contains("hook claude") && after.contains("Bash(make)"), "uninstall takes only Gitta's entries: {after}");
}

#[cfg(feature = "mainframe")]
#[test]
fn a_wrong_code_page_conversion_needs_a_person() {
    let e = env("codepage");
    const ORDCHK: &str = "       IDENTIFICATION DIVISION.\n       PROGRAM-ID. ORDCHK.\n       DATA DIVISION.\n       WORKING-STORAGE SECTION.\n       01  WS-STATUS      PIC XX.\n       PROCEDURE DIVISION.\n           IF WS-STATUS ¬= '00'\n              DISPLAY 'ORDCHK: FILE STATUS ' WS-STATUS\n           END-IF\n           GOBACK.\n";
    write(&e.repo.join(".gitattributes"), "*.cbl zos-working-tree-encoding=ibm-1047 git-encoding=utf-8\n*.rexx zos-working-tree-encoding=ibm-1047 git-encoding=utf-8\n");
    write(&e.repo.join("src/ORDCHK.cbl"), ORDCHK);
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "base"]);
    gitta(&e, &e.repo, &["init", "--no-claude"]);
    let (a, b) = (worktree(&e, "reconvert"), worktree(&e, "proc"));
    // The member came back through a transfer that read IBM-1047 as IBM-037: `¬` is now `^`.
    write(&a.join("src/ORDCHK.cbl"), &ORDCHK.replace('¬', "^"));
    git(&a, &["add", "-A"]); git(&a, &["commit", "-qm", "refresh ORDCHK from the host"]);
    // The other agent adds JCL, for which no code page is set.
    write(&b.join("jcl/ORDLOAD.jcl"), "//ORDLOAD  JOB (ACCT),'LOAD',CLASS=A\n//STEP1    EXEC PGM=ORDCHK\n");
    git(&b, &["add", "-A"]); git(&b, &["commit", "-qm", "load job"]);
    let out = text(&gitta_in(&e, &a, &["hook", "claude"], &format!("{{\"cwd\": {:?}}}", a.to_str().unwrap())));
    assert!(out.contains("code page conversion changed characters") && out.contains("`¬` → `^` (1)") && out.contains("decides whether it stops the merge"), "hook output: {out}");
    let out = text(&gitta_in(&e, &b, &["hook", "claude"], &format!("{{\"cwd\": {:?}}}", b.to_str().unwrap())));
    assert!(out.contains("jcl/ORDLOAD.jcl is a mainframe source without a code page"), "hook output: {out}");
    let enc = text(&gitta(&e, &e.repo, &["encoding"]));
    assert!(enc.contains("ibm-1047: 1 file(s)"), "{enc}");
    let o = gitta(&e, &e.repo, &["land", "reconvert"]);
    assert!(!o.status.success() && text(&o).contains("land refused") && text(&o).contains("src/ORDCHK.cbl"), "{}", text(&o));
    assert!(approve(&e, "reconvert", "the host copy uses ^ on purpose").status.success());
    assert!(gitta(&e, &e.repo, &["land", "reconvert"]).status.success());
    let audit = text(&gitta(&e, &e.repo, &["audit", "--json"])).replace(": ", ":");
    for want in ["\"check\":\"code page\"", "CODE_PAGE", "\"kind\":\"CODE_PAGE_RISK\""] { assert!(audit.contains(want), "{want} missing in audit"); }
}

#[test]
fn pr_check_keeps_code_overlaps_as_warnings_and_clean_doc_test_and_import_overlaps_as_info() {
    let e = env("quiet");
    let readme = "# Tool\n\nIntro.\n\n\n\n\n\n## Usage\n\nRun it.\n";
    const TOOLS: &str = "import { a } from \"./a\";\n\nexport const TOOLS = {\n  one: 1,\n\n\n\n\n  two: 2,\n};\n";
    const TEST: &str = "def test_tools():\n    assert 1\n\n\n\n\n    assert 2\n";
    write(&e.repo.join("README.md"), readme);
    write(&e.repo.join("src/index.ts"), TOOLS);
    write(&e.repo.join("src/a.ts"), "export const a = 1;\nexport const b = 2;\n");
    write(&e.repo.join("tests/test_tools.py"), TEST);
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "base"]);
    git(&e.repo, &["checkout", "-qb", "left"]);
    write(&e.repo.join("README.md"), &readme.replace("Intro.", "Intro, now with scraping."));
    write(&e.repo.join("src/index.ts"), &TOOLS.replace("  one: 1,", "  one: 1,\n  scrape: 3,").replace("{ a }", "{ a, b }"));
    write(&e.repo.join("tests/test_tools.py"), &TEST.replace("assert 1", "assert 1 == 1"));
    git(&e.repo, &["commit", "-qam", "left"]);
    git(&e.repo, &["checkout", "-q", "main"]);
    git(&e.repo, &["checkout", "-qb", "right"]);
    write(&e.repo.join("README.md"), &readme.replace("Run it.", "Run it with npx."));
    write(&e.repo.join("src/index.ts"), &(TOOLS.replace("  two: 2,", "  scrape: 4,\n  two: 2,") + "import { a, b } from \"./a\";\n"));
    write(&e.repo.join("tests/test_tools.py"), &TEST.replace("assert 2", "assert 2 == 2"));
    git(&e.repo, &["commit", "-qam", "right"]);
    git(&e.repo, &["checkout", "-q", "main"]);
    let check = |repo: &Path| -> serde_json::Value {
        let o = gitta(&e, repo, &["pr-check", "--pr", "left", "--other", "right", "--base", "main", "--json"]);
        let t = text(&o);
        serde_json::from_str::<serde_json::Value>(&t[t.find('[').unwrap()..]).unwrap()[0].clone()
    };
    let has = |r: &serde_json::Value, k: &str, s: &str| r[k].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains(s));
    let r = check(&e.repo);
    // Both added a key to one object: a duplicate key the text merge cannot see. That stays a warning.
    assert!(has(&r, "warnings", "src/index.ts assign TOOLS"), "{r}");
    // The import line ends up twice in the merge: it declares the names twice, so it stays a warning too.
    assert!(has(&r, "warnings", "src/index.ts import"), "{r}");
    // A document and a test function that merge cleanly only inform.
    assert!(has(&r, "info", "README.md") && has(&r, "info", "tests/test_tools.py def test_tools"), "{r}");
    assert!(!has(&r, "warnings", "README.md") && !has(&r, "warnings", "tests/test_tools.py"), "{r}");
    // The same answer from a blob-less clone, where the file contents are fetched in one request.
    let clone = e.dir.join("partial");
    let o = Command::new("git").args(["-c", "uploadpack.allowFilter=true", "clone", "-q", "--no-checkout", "--filter=blob:none"])
        .arg(format!("file://{}", e.repo.display())).arg(&clone).output().unwrap();
    assert!(o.status.success(), "{}", text(&o));
    for b in ["left", "right"] { git(&clone, &["branch", "-q", b, &format!("origin/{b}")]); }
    let p = check(&clone);
    assert_eq!(p["warnings"], r["warnings"]);
    assert_eq!(p["info"], r["info"]);
}

/// A fake release source on disk (file:// URLs): latest.json and one release with its checksum.
fn fake_release(dir: &Path, tag: &str, body: &str, good_sum: bool) -> (String, String) {
    let target = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin", ("macos", _) => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl", ("linux", "s390x") => "s390x-unknown-linux-gnu", _ => "x86_64-unknown-linux-musl",
    };
    let name = format!("gitta-{}-{target}", tag.trim_start_matches('v'));
    let rel = dir.join("download").join(tag);
    std::fs::create_dir_all(rel.join(&name)).unwrap();
    std::fs::write(rel.join(&name).join("gitta"), body).unwrap();
    std::fs::write(rel.join(&name).join("gitta-sandbox"), "sandbox").unwrap();
    assert!(Command::new("tar").arg("-czf").arg(format!("{name}.tar.gz")).arg(&name).current_dir(&rel).status().unwrap().success());
    let sum = Command::new("shasum").args(["-a", "256"]).arg(format!("{name}.tar.gz")).current_dir(&rel).output()
        .or_else(|_| Command::new("sha256sum").arg(format!("{name}.tar.gz")).current_dir(&rel).output()).unwrap();
    let mut s = String::from_utf8_lossy(&sum.stdout).to_string();
    if !good_sum { s = format!("{} x", "0".repeat(64)); }
    std::fs::write(rel.join(format!("{name}.tar.gz.sha256")), s).unwrap();
    std::fs::write(dir.join("latest.json"), format!("{{\"tag_name\":\"{tag}\"}}")).unwrap();
    (format!("file://{}", dir.join("latest.json").display()), format!("file://{}", dir.join("download").display()))
}

fn copied_gitta(e: &Env) -> PathBuf {
    let bin = e.dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_gitta"), bin.join("gitta")).unwrap();
    bin
}

fn run_update(e: &Env, bin: &Path, api: &str, dl: &str, args: &[&str]) -> Output {
    Command::new(bin.join("gitta")).arg("update").args(args).current_dir(&e.repo).env("HOME", &e.home)
        .env("GITTA_UPDATE_API", api).env("GITTA_UPDATE_DOWNLOAD", dl).env("GITTA_CACHE_DIR", e.dir.join("cache"))
        .env_remove("CARGO_HOME").env_remove("HOMEBREW_PREFIX").output().unwrap()
}

#[test]
fn init_mentions_a_backup_only_when_it_made_one() {
    let e = env("fresh");
    let o = gitta(&e, &e.repo, &["init"]);
    assert!(!o.status.success() && text(&o).contains("no commits yet") && !e.repo.join(".gitta").exists(), "{}", text(&o));
    git(&e.repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
    let o = gitta(&e, &e.repo, &["init", "--onto", "nope"]);
    assert!(!o.status.success() && text(&o).contains("no branch `nope`"), "{}", text(&o));
    let o = gitta(&e, &e.repo, &["init"]);
    assert!(o.status.success() && text(&o).contains("Claude Code hooks added") && !text(&o).contains("(backup"), "{}", text(&o));
    let _ = std::fs::remove_dir_all(&e.dir);
}

#[test]
fn uninstall_leaves_no_empty_agent_folders_in_the_repository() {
    // On Linux, init writes .claude/settings.local.json and .cursor/worktrees.json; uninstall removes the folders it emptied.
    let e = env("emptydirs");
    git(&e.repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
    assert!(gitta(&e, &e.repo, &["init"]).status.success());
    assert!(gitta(&e, &e.repo, &["uninstall"]).status.success());
    assert!(!e.repo.join(".claude").exists() && !e.repo.join(".cursor").exists());
    let _ = std::fs::remove_dir_all(&e.dir);
}

#[test]
fn update_without_curl_says_so() {
    let e = env("nocurl");
    let empty = e.dir.join("emptybin"); std::fs::create_dir_all(&empty).unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_gitta")).env("GITTA_NO_NOTIFY", "1").env("GITTA_NO_PROMPT", "1").env("GITTA_WEAKENED_CHECKS", "block").args(["update", "--check"]).current_dir(&e.repo).env("HOME", &e.home)
        .env("PATH", &empty).env("GITTA_CACHE_DIR", e.dir.join("cache")).output().unwrap();
    assert!(!o.status.success() && text(&o).contains("needs the curl command") && !text(&o).contains("network"), "{}", text(&o));
    let _ = std::fs::remove_dir_all(&e.dir);
}

#[test]
fn version_prints_the_package_version() {
    let e = env("version");
    for a in ["--version", "version", "-V"] {
        let o = gitta(&e, &e.repo, &[a]);
        assert!(o.status.success(), "{o:?}");
        assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), format!("gitta {}", env!("CARGO_PKG_VERSION")));
    }
    let _ = std::fs::remove_dir_all(&e.dir);
}

#[test]
fn update_renames_the_new_release_over_the_binary() {
    let e = env("update");
    let bin = copied_gitta(&e);
    std::fs::write(bin.join("gitta-sandbox"), "old").unwrap();
    let (api, dl) = fake_release(&e.dir.join("rel"), "v99.0.0", "#!/bin/sh\necho new gitta\n", true);
    let o = run_update(&e, &bin, &api, &dl, &["--check"]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("99.0.0 is available"), "{o:?}");
    assert!(std::fs::read(bin.join("gitta")).unwrap().len() > 1000, "--check changes nothing");
    #[cfg(unix)] let ino = { use std::os::unix::fs::MetadataExt; std::fs::metadata(bin.join("gitta")).unwrap().ino() };
    let o = run_update(&e, &bin, &api, &dl, &[]);
    assert!(o.status.success(), "{o:?}");
    assert_eq!(std::fs::read_to_string(bin.join("gitta")).unwrap(), "#!/bin/sh\necho new gitta\n");
    assert_eq!(std::fs::read_to_string(bin.join("gitta-sandbox")).unwrap(), "sandbox");
    #[cfg(unix)] { use std::os::unix::fs::MetadataExt; assert_ne!(std::fs::metadata(bin.join("gitta")).unwrap().ino(), ino, "renamed over, not written in place"); }
    let left: Vec<_> = std::fs::read_dir(&bin).unwrap().flatten().map(|d| d.file_name()).collect();
    assert_eq!(left.len(), 2, "no temporary files left: {left:?}");
    let _ = std::fs::remove_dir_all(&e.dir);
}

#[test]
fn update_refuses_a_bad_checksum_and_no_network() {
    let e = env("update-bad");
    let bin = copied_gitta(&e);
    let before = std::fs::read(bin.join("gitta")).unwrap();
    let (api, dl) = fake_release(&e.dir.join("rel"), "v99.0.0", "bad", false);
    let o = run_update(&e, &bin, &api, &dl, &[]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("checksum mismatch"), "{o:?}");
    assert_eq!(std::fs::read(bin.join("gitta")).unwrap(), before);
    assert_eq!(std::fs::read_dir(&bin).unwrap().count(), 1);
    let o = run_update(&e, &bin, "file:///nonexistent/latest.json", &dl, &[]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("cannot reach the release server"), "{o:?}");
    let (api, dl) = fake_release(&e.dir.join("rel2"), "v0.0.1", "old", true);
    let o = run_update(&e, &bin, &api, &dl, &[]);
    assert!(o.status.success() && String::from_utf8_lossy(&o.stdout).contains("is the latest version"), "{o:?}");
    let _ = std::fs::remove_dir_all(&e.dir);
}

#[test]
fn update_leaves_a_cargo_install_alone() {
    let e = env("update-cargo");
    let bin = e.home.join(".cargo/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_gitta"), bin.join("gitta")).unwrap();
    let before = std::fs::read(bin.join("gitta")).unwrap();
    let (api, dl) = fake_release(&e.dir.join("rel"), "v99.0.0", "new", true);
    let o = run_update(&e, &bin, &api, &dl, &[]);
    assert!(o.status.success() && String::from_utf8_lossy(&o.stdout).contains("cargo install"), "{o:?}");
    assert_eq!(std::fs::read(bin.join("gitta")).unwrap(), before);
    let _ = std::fs::remove_dir_all(&e.dir);
}

const FAILING_GATE: &str = "if grep -rq BROKEN shop; then echo 'FAIL: test_total (tests.test_cart.CartTest)'; echo 'FAILED (failures=1)'; exit 1; fi";

#[test]
fn a_gate_tool_missing_from_the_checkout_is_a_setup_problem_not_a_refusal() {
    let e = env("missingtool");
    shop(&e);
    assert!(gitta(&e, &e.repo, &["init", "--no-claude", "--gate", "no-such-tool-gitta-test --check"]).status.success());
    let a = worktree(&e, "feat");
    write(&a.join("shop/text.py"), "def shout(s):\n    return s.upper()\n\n\ndef whisper(s):\n    return s.lower()\n");
    let o = gitta(&e, &e.repo, &["land", "feat"]);
    let t = text(&o);
    assert_eq!(o.status.code(), Some(3), "{t}");
    assert!(t.contains("setup problem") && t.contains("no-such-tool-gitta-test") && !t.contains("land refused"), "{t}");
    let log = text(&gitta(&e, &e.repo, &["audit", "--json"]));
    assert!(log.contains("gate_environment") && !log.contains("land_refused"), "{log}");
}

#[test]
fn the_gate_finds_installed_dependencies_of_the_main_checkout() {
    let e = env("deps");
    shop(&e);
    write(&e.repo.join(".gitignore"), "node_modules/\n.venv/\n");
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "ignore deps"]);
    // Installed in the main checkout only, as `npm ci` leaves it.
    write(&e.repo.join("node_modules/.bin/tsc"), "#!/bin/sh\necho tsc ok\n");
    Command::new("chmod").args(["+x", e.repo.join("node_modules/.bin/tsc").to_str().unwrap()]).status().unwrap();
    assert!(gitta(&e, &e.repo, &["init", "--no-claude", "--gate", "node_modules/.bin/tsc --noEmit"]).status.success());
    let a = worktree(&e, "feat");
    write(&a.join("shop/text.py"), "def shout(s):\n    return s.upper()\n\n\ndef whisper(s):\n    return s.lower()\n");
    let o = gitta(&e, &e.repo, &["land", "feat"]);
    assert!(o.status.success() && text(&o).contains("tsc ok"), "{}", text(&o));
    assert!(!git(&e.repo, &["show", "--name-only", "--format=", "main"]).contains("node_modules"), "the linked folder is never committed");
}

#[test]
fn a_failed_gate_names_the_test_and_its_first_error_lines() {
    let e = env("firsterr");
    shop(&e);
    write(&e.repo.join("check.sh"), "echo '___ test_whisper ___'\necho 'E       AssertionError: assert 1 == 2'\necho 'tests/test_text.py:7: AssertionError'\necho 'FAILED tests/test_text.py::test_whisper - AssertionError'\nexit 1\n");
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "check"]);
    assert!(gitta(&e, &e.repo, &["init", "--no-claude", "--gate", "sh check.sh"]).status.success());
    let a = worktree(&e, "feat");
    write(&a.join("shop/text.py"), "def shout(s):\n    return s.upper()\n\n\ndef whisper(s):\n    return s.lower()\n");
    let o = gitta(&e, &e.repo, &["land", "feat"]);
    let t = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(!o.status.success());
    assert!(t.contains("First failing test: tests/test_text.py::test_whisper") && t.contains("E       AssertionError: assert 1 == 2") && t.contains("tests/test_text.py:7"), "{t}");
}

#[test]
fn a_credential_name_already_in_a_file_does_not_stop_an_unrelated_edit() {
    let e = env("flynt");
    shop(&e);
    // flynt's test/test_edits.py: a format string that names a password field (no secret), already on main.
    let t = "def test_110():\n    s_in = \"'{conn.login}:{conn.password}@'.format(conn=x)\"\n    assert s_in\n";
    write(&e.repo.join("tests/test_edits.py"), t);
    write(&e.repo.join("shop/old.py"), "PASSWORD = \"Zx9q-Lm2w-Pp7r-old\"\n");
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "tests"]);
    gitta(&e, &e.repo, &["init", "--no-claude"]);
    let a = worktree(&e, "edit");
    write(&a.join("tests/test_edits.py"), &format!("{t}\n\ndef test_more():\n    assert True\n"));
    write(&a.join("shop/old.py"), "PASSWORD = \"Zx9q-Lm2w-Pp7r-old\"\nX = 1\n");
    let o = gitta(&e, &e.repo, &["land", "edit"]);
    assert!(o.status.success(), "{}", text(&o));
    // A credential the agent adds is still refused, with its line.
    let b = worktree(&e, "leak");
    write(&b.join("shop/text.py"), "def shout(s):\n    return s.upper()\n\nDB_PASSWORD = 'Tr0ub4dor-3xyz-9'\n");
    let o = gitta(&e, &e.repo, &["land", "leak"]);
    assert!(!o.status.success() && text(&o).contains("shop/text.py:4 looks like it contains a credential"), "{}", text(&o));
}

/// A plain `git` command, with the environment a person's terminal would have (no prompt in tests).
fn git_run(e: &Env, dir: &Path, args: &[&str]) -> Output {
    Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args)
        .env("HOME", &e.home).env("GITTA_NO_NOTIFY", "1").env("GITTA_NO_PROMPT", "1").env_remove("CLAUDECODE").output().unwrap()
}

/// `git_run` as an AI agent's tool call runs it (Claude Code sets CLAUDECODE).
fn git_run_agent(e: &Env, dir: &Path, args: &[&str]) -> Output {
    Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args)
        .env("HOME", &e.home).env("GITTA_NO_NOTIFY", "1").env("GITTA_NO_PROMPT", "1").env("CLAUDECODE", "1").output().unwrap()
}

#[test]
fn git_merge_runs_the_tests_on_the_combined_code_and_stops_only_when_they_fail() {
    let e = env("gitmerge");
    shop(&e);
    let o = gitta(&e, &e.repo, &["init", "--no-claude", "--gate", FAILING_GATE]);
    assert!(text(&o).contains("Merging works as always, with git merge") && text(&o).contains("tail -f .gitta/gitta.log"), "{}", text(&o));
    let (ok, bad) = (worktree(&e, "whisper"), worktree(&e, "totals"));
    write(&ok.join("shop/text.py"), "def whisper(s):\n    return s.lower()\n");
    git(&ok, &["add", "-A"]); git(&ok, &["commit", "-qm", "whisper"]);
    write(&bad.join("shop/cart.py"), &format!("{CART}\n\n# BROKEN\n"));
    git(&bad, &["add", "-A"]); git(&bad, &["commit", "-qm", "totals"]);
    // A fast-forward: checked after the fact, and it passes.
    let o = git_run(&e, &e.repo, &["merge", "--no-edit", "whisper"]);
    assert!(o.status.success() && !text(&o).contains("gitta:"), "a passing merge is silent: {}", text(&o));
    assert!(std::fs::read_to_string(e.repo.join(".gitta/gitta.log")).unwrap().contains("Tests passed on main combined with whisper"));
    let main_before = git(&e.repo, &["rev-parse", "main"]);
    // A real merge whose combined code fails: stopped before the commit, with the failing test named.
    let o = git_run(&e, &e.repo, &["merge", "--no-edit", "totals"]);
    let t = text(&o);
    assert!(!o.status.success() && t.contains("tests fail on main with totals") && t.contains("tests.test_cart.CartTest.test_total") && t.contains("merge again"), "{t}");
    let _ = git_run(&e, &e.repo, &["merge", "--abort"]);   // gitta may already have cancelled it
    assert_eq!(git(&e.repo, &["rev-parse", "main"]), main_before, "main stays as it was");
    // Fixed on the branch, the same command goes through.
    write(&bad.join("shop/cart.py"), &format!("{CART}\n\n# fixed\n"));
    git(&bad, &["commit", "-qam", "fix"]);
    let o = git_run(&e, &e.repo, &["merge", "--no-edit", "totals"]);
    assert!(o.status.success(), "{}", text(&o));
    let log = std::fs::read_to_string(e.repo.join(".gitta/gitta.log")).unwrap();
    assert!(log.contains("gitta stopped the git merge: tests fail on main combined with totals") && log.contains("totals was merged into main with git merge"), "{log}");
    let w = text(&gitta(&e, &e.repo, &["watch", "--once"]));
    assert!(w.contains("Today gitta checked") && w.contains("merged 2 pieces of work"), "{w}");
}

#[test]
fn the_committed_gate_counts_in_git_merge_and_only_a_person_sets_the_gate() {
    let e = env("teamgate");
    shop(&e);
    write(&e.repo.join(".gitta.toml"), &format!("gate = {FAILING_GATE:?}\n"));
    git(&e.repo, &["add", "-A"]); git(&e.repo, &["commit", "-qm", "team gate"]);
    gitta(&e, &e.repo, &["init", "--no-claude"]);
    let bad = worktree(&e, "totals");
    write(&bad.join("shop/cart.py"), &format!("{CART}\n\n# BROKEN\n"));
    git(&bad, &["commit", "-qam", "totals"]);
    let before = git(&e.repo, &["rev-parse", "main"]);
    // No local gate: git merge runs the committed one.
    let o = git_run(&e, &e.repo, &["merge", "--no-edit", "totals"]);
    assert!(!o.status.success() && text(&o).contains("tests fail on main with totals"), "{}", text(&o));
    let _ = git_run(&e, &e.repo, &["merge", "--abort"]);
    assert_eq!(git(&e.repo, &["rev-parse", "main"]), before);
    // An agent session cannot set the gate, not even from its own worktree.
    let o = Command::new(env!("CARGO_BIN_EXE_gitta")).env("GITTA_NO_NOTIFY", "1").env("CLAUDECODE", "1").args(["init", "--no-claude", "--gate", "true"])
        .current_dir(&bad).env("HOME", &e.home).output().unwrap();
    assert!(!o.status.success() && text(&o).contains("only a person sets the gate"), "{}", text(&o));
    // A person can set a local gate. The committed one still counts, and the change is in the audit trail.
    let o = gitta(&e, &e.repo, &["init", "--no-claude", "--gate", "true"]);
    assert!(o.status.success() && text(&o).contains("that one counts"), "{}", text(&o));
    let o = git_run(&e, &e.repo, &["merge", "--no-edit", "totals"]);
    assert!(!o.status.success() && text(&o).contains("tests fail on main with totals"), "{}", text(&o));
    let _ = git_run(&e, &e.repo, &["merge", "--abort"]);
    assert!(text(&gitta(&e, &e.repo, &["audit", "--json"])).contains("\"gate_changed\""));
}

#[test]
fn a_fast_forward_that_fails_the_tests_is_refused_before_main_moves() {
    let e = env("gitff");
    shop(&e);
    gitta(&e, &e.repo, &["init", "--no-claude", "--gate", FAILING_GATE]);
    let bad = worktree(&e, "totals");
    write(&bad.join("shop/cart.py"), &format!("{CART}\n\n# BROKEN\n"));
    git(&bad, &["add", "-A"]); git(&bad, &["commit", "-qm", "totals"]);
    let before = git(&e.repo, &["rev-parse", "main"]);
    // A watcher in every hook git runs records where main points: it must never point at the untested commit.
    let hooks = e.repo.join(".git/hooks");
    let seen = e.repo.join(".git/seen-main");
    for h in ["reference-transaction", "post-merge", "post-checkout"] {
        let p = hooks.join(h);
        let t = std::fs::read_to_string(&p).unwrap_or_else(|_| "#!/bin/sh\n".into());
        let t = t.replacen("#!/bin/sh\n", &format!("#!/bin/sh\ngit rev-parse refs/heads/main >> {:?}\n", seen), 1);
        std::fs::write(&p, t).unwrap();
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap(); }
    }
    // An unrelated uncommitted edit stays exactly as it was.
    write(&e.repo.join("shop/text.py"), "def shout(s):\n    return s.upper() + '!'\n");
    let cart = std::fs::read_to_string(e.repo.join("shop/cart.py")).unwrap();
    let o = git_run(&e, &e.repo, &["merge", "--no-edit", "totals"]);
    let t = text(&o);
    assert!(!o.status.success() && t.contains("tests fail on main with totals") && t.contains("main did not move"), "{t}");
    assert_eq!(t.lines().filter(|l| l.starts_with("gitta:")).count(), 1, "one line: {t}");
    let seen = std::fs::read_to_string(&seen).unwrap();
    assert!(!seen.is_empty() && seen.lines().all(|l| l == before), "main moved during the merge: {seen}");
    assert_eq!(git(&e.repo, &["rev-parse", "main"]), before);
    assert_eq!(git(&e.repo, &["status", "--porcelain", "--untracked-files=no"]), "M shop/text.py", "nothing is left behind");
    assert!(std::fs::read_to_string(e.repo.join("shop/text.py")).unwrap().contains("'!'"));
    assert_eq!(std::fs::read_to_string(e.repo.join("shop/cart.py")).unwrap(), cart, "the files are as before");
    assert!(!e.repo.join(".git/MERGE_HEAD").exists());
    // git pull that fast-forwards takes the same path.
    let o = git_run(&e, &e.repo, &["pull", "--no-rebase", "--ff-only", ".", "totals"]);
    assert!(!o.status.success() && text(&o).contains("main did not move"), "{}", text(&o));
    assert_eq!(git(&e.repo, &["rev-parse", "main"]), before);
    assert_eq!(git(&e.repo, &["status", "--porcelain", "--untracked-files=no"]), "M shop/text.py");
    // --no-verify and GITTA_SKIP go ahead and are recorded in the audit trail.
    let o = git_run(&e, &e.repo, &["merge", "--no-verify", "--no-edit", "totals"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&gitta(&e, &e.repo, &["audit", "--json"])).contains("git merge --no-verify"));
}

#[test]
fn an_agent_commit_on_main_in_the_main_checkout_is_checked_like_a_merge() {
    let e = env("agentcommit");
    shop(&e);
    gitta(&e, &e.repo, &["init", "--no-claude", "--gate", FAILING_GATE]);
    let before = git(&e.repo, &["rev-parse", "main"]);
    // An agent that works in the main checkout commits code that fails the tests: main stays.
    write(&e.repo.join("shop/cart.py"), &format!("{CART}\n\n# BROKEN\n"));
    git(&e.repo, &["add", "-A"]);
    let o = git_run_agent(&e, &e.repo, &["commit", "-qm", "broken"]);
    assert!(!o.status.success() && text(&o).contains("tests fail on main"), "{}", text(&o));
    assert_eq!(git(&e.repo, &["rev-parse", "main"]), before);
    // Fixed, the same commit goes ahead.
    write(&e.repo.join("shop/cart.py"), &format!("{CART}\n\n# fixed\n"));
    git(&e.repo, &["add", "-A"]);
    let o = git_run_agent(&e, &e.repo, &["commit", "-qm", "fixed"]);
    assert!(o.status.success(), "{}", text(&o));
    // A person's own commit on main is not checked, as before.
    write(&e.repo.join("shop/text.py"), "def shout(s):\n    return s.upper()\n# BROKEN\n");
    let o = git_run(&e, &e.repo, &["commit", "-qam", "mine"]);
    assert!(o.status.success(), "{}", text(&o));
}

#[test]
fn a_passing_fast_forward_is_silent_and_a_merge_commit_is_tested_once() {
    let e = env("gitffok");
    shop(&e);
    // The gate counts its runs.
    let runs = e.repo.join(".git/gate-runs");
    gitta(&e, &e.repo, &["init", "--no-claude", "--gate", &format!("echo x >> {:?}", runs)]);
    let a = worktree(&e, "whisper");
    write(&a.join("shop/text.py"), "def whisper(s):\n    return s.lower()\n");
    git(&a, &["add", "-A"]); git(&a, &["commit", "-qm", "whisper"]);
    let o = git_run(&e, &e.repo, &["merge", "--no-edit", "whisper"]);
    assert!(o.status.success() && !text(&o).contains("gitta"), "{}", text(&o));
    assert_eq!(git(&e.repo, &["rev-parse", "main"]), git(&a, &["rev-parse", "HEAD"]));
    // The commit ran the tests on the code the merge gives; the fast-forward used that result.
    assert_eq!(std::fs::read_to_string(&runs).unwrap().lines().count(), 1);
    // A true merge: pre-merge-commit tests it, and the ref update of the same tree is not tested again.
    let b = worktree(&e, "totals");
    write(&b.join("shop/cart.py"), &format!("{CART}\n\n# more\n"));
    git(&b, &["add", "-A"]); git(&b, &["commit", "-qm", "totals"]);
    write(&e.repo.join("README"), "main\n");
    git(&e.repo, &["add", "-A"]);
    assert!(git_run(&e, &e.repo, &["commit", "-qm", "own"]).status.success(), "a person's own commit on main");
    let o = git_run(&e, &e.repo, &["merge", "--no-edit", "totals"]);
    assert!(o.status.success() && !text(&o).contains("gitta"), "{}", text(&o));
    // The commit tested totals on main as it was then (2); main moved after it, so the merge commit tests once more (3).
    assert_eq!(std::fs::read_to_string(&runs).unwrap().lines().count(), 3, "the merge commit ran the tests once");
}

#[test]
fn removed_tests_in_a_merge_warn_without_a_terminal_and_block_when_set() {
    let e = env("gitweak");
    shop(&e);
    write(&e.repo.join("tests/test_cart.py"), "from shop.cart import total\n\n\ndef test_total():\n    assert total(10) == 12.5\n\n\ndef test_zero():\n    assert total(0) == 0\n");
    git(&e.repo, &["add", "-A"]); git(&e.repo, &["commit", "-qm", "tests"]);
    let o = gitta(&e, &e.repo, &["init", "--no-claude", "--weakened-checks", "block"]);
    assert!(text(&o).contains("Commit it"), "{}", text(&o));
    // Team policy counts only once it is committed on main, so a reviewed commit decides it.
    let toml = std::fs::read_to_string(e.repo.join(".gitta.toml")).unwrap();
    assert!(toml.contains("weakened_checks = \"block\""), "{toml}");
    git(&e.repo, &["add", ".gitta.toml"]); git(&e.repo, &["commit", "-qm", "gitta policy"]);
    let a = worktree(&e, "trim");
    write(&a.join("tests/test_cart.py"), "from shop.cart import total\n\n\ndef test_total():\n    assert total(10) == 12.5\n");
    git(&a, &["add", "-A"]); git(&a, &["commit", "-qm", "trim"]);
    let before = git(&e.repo, &["rev-parse", "main"]);
    let o = git_run(&e, &e.repo, &["merge", "--no-edit", "trim"]);
    assert!(text(&o).contains("trim") && text(&o).contains("tests/test_cart.py") && text(&o).contains("was not merged"), "{}", text(&o));
    assert_eq!(git(&e.repo, &["rev-parse", "main"]), before);
    // An uncommitted edit of the policy changes nothing.
    write(&e.repo.join(".gitta.toml"), "weakened_checks = \"warn\"\n");
    let o = git_run(&e, &e.repo, &["merge", "--no-edit", "trim"]);
    assert!(text(&o).contains("was not merged") && text(&o).contains("main did not move"), "{}", text(&o));
    // The CI check reads the same committed policy: listed, and exit 3 when it blocks.
    let o = gitta(&e, &e.repo, &["pr-check", "--pr", "trim", "--base", "main"]);
    assert_eq!(o.status.code(), Some(3), "{}", text(&o));
    assert!(text(&o).contains("Weakens a check") && text(&o).contains("tests/test_cart.py"), "{}", text(&o));
    git(&e.repo, &["commit", "-qam", "policy warn"]);
    let back = git(&e.repo, &["rev-parse", "main"]);
    // An agent's merge that goes ahead says nothing about it: the warning is for people, in the log and audit trail.
    let o = git_run_agent(&e, &e.repo, &["merge", "--no-edit", "trim"]);
    assert!(o.status.success() && !text(&o).contains("gitta"), "{}", text(&o));
    git(&e.repo, &["reset", "-q", "--hard", &back]);
    let o = git_run(&e, &e.repo, &["merge", "--no-edit", "trim"]);
    assert!(o.status.success() && text(&o).contains("weakened_checks is warn"), "{}", text(&o));
    assert!(text(&gitta(&e, &e.repo, &["audit", "--json"])).contains("\"warned\""));
}

#[test]
fn every_other_way_main_moves_is_checked_and_refused_when_the_tests_fail() {
    let e = env("gitrefs");
    shop(&e);
    gitta(&e, &e.repo, &["init", "--no-claude", "--gate", FAILING_GATE]);
    let bad = worktree(&e, "totals");
    write(&bad.join("shop/cart.py"), &format!("{CART}\n\n# BROKEN\n"));
    git(&bad, &["add", "-A"]); git(&bad, &["commit", "-qm", "totals"]);
    // main gets one good commit of its own, so a rebase and a cherry-pick really create new commits.
    write(&e.repo.join("shop/text.py"), "def shout(s):\n    return s.upper()\n");
    git(&e.repo, &["add", "-A"]); git(&e.repo, &["commit", "-q", "--allow-empty", "-m", "text"]);
    let before = git(&e.repo, &["rev-parse", "main"]);
    let broken = git(&bad, &["rev-parse", "HEAD"]);
    let refused = |o: Output, how: &str| {
        let t = text(&o);
        assert!(t.contains("tests fail on main with") && t.contains("tests.test_cart.CartTest.test_total"), "{how}: {t}");
        assert_eq!(git(&e.repo, &["rev-parse", "main"]), before, "{how}: main must not move");
    };
    refused(git_run(&e, &e.repo, &["cherry-pick", &broken]), "cherry-pick");
    let _ = git_run(&e, &e.repo, &["cherry-pick", "--abort"]);
    git(&e.repo, &["reset", "-q", "--hard", &before]);
    refused(git_run(&e, &e.repo, &["rebase", "totals"]), "rebase");
    let _ = git_run(&e, &e.repo, &["rebase", "--abort"]);
    refused(git_run(&e, &e.repo, &["pull", "--rebase", ".", "totals"]), "");
    let _ = git_run(&e, &e.repo, &["rebase", "--abort"]);
    git(&e.repo, &["reset", "-q", "--hard", &before]);
    assert!(git_run(&e, &e.repo, &["merge", "--squash", "totals"]).status.success());
    refused(git_run(&e, &e.repo, &["commit", "-qm", "squashed totals"]), "merge --squash and commit");
    git(&e.repo, &["reset", "-q", "--hard", &before]);
    refused(git_run(&e, &e.repo, &["reset", "--hard", &broken]), "reset");
    refused(git_run(&e, &e.repo, &["update-ref", "refs/heads/main", &broken]), "update-ref");
    // A plain commit on main is the person's own work and stays fast; GITTA_SKIP passes and is recorded.
    git(&e.repo, &["reset", "-q", "--hard", "main"]);
    write(&e.repo.join("shop/text.py"), "def shout(s):\n    return s.upper() + '!'\n");
    let o = git_run(&e, &e.repo, &["commit", "-qam", "own work"]);
    assert!(o.status.success() && !text(&o).contains("gitta"), "a plain commit runs no tests: {}", text(&o));
    let o = Command::new("git").arg("-C").arg(&e.repo).args(["update-ref", "refs/heads/main", &broken]).env("HOME", &e.home).env("GITTA_SKIP", "1").output().unwrap();
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&gitta(&e, &e.repo, &["audit", "--json"])).contains("GITTA_SKIP"));
}

const LABEL: &str = "from shop.pricing import net_to_gross\n\n\ndef label(x):\n    return f\"{net_to_gross(x)} kr\"\n";
const INT_GROSS: &str = "return int(net * (1 + VAT_RATE) * 100)";

#[test]
fn the_stop_hook_never_tests_unfinished_work_and_tells_a_failed_merge_once() {
    let e = env("stop");
    shop(&e);
    assert!(gitta(&e, &e.repo, &["init", "--no-claude", "--gate", FAILING_GATE]).status.success());
    let (good, bad) = (worktree(&e, "good"), worktree(&e, "bad"));
    write(&good.join("shop/text.py"), "def whisper(s):\n    return s.lower()\n");
    write(&bad.join("shop/cart.py"), &format!("{CART}\n\n# BROKEN\n"));
    let stop = |w: &Path| String::from_utf8_lossy(&gitta_in(&e, w, &["hook", "claude-stop"], &format!("{{\"cwd\": {:?}}}", w.to_str().unwrap())).stdout).to_string();
    // The other agent's unfinished work breaks the tests, but nothing was merged: the Stop hook stays silent.
    assert_eq!(stop(&good), "", "no combined test against unfinished work");
    assert_eq!(stop(&bad), "");
    git(&good, &["add", "-A"]); git(&good, &["commit", "-qm", "good"]);
    assert!(git_run(&e, &e.repo, &["merge", "--no-edit", "good"]).status.success());
    git(&bad, &["add", "-A"]); git(&bad, &["commit", "-qm", "bad"]);
    assert!(!git_run(&e, &e.repo, &["merge", "--no-edit", "bad"]).status.success());
    let _ = git_run(&e, &e.repo, &["merge", "--abort"]);
    let t = stop(&bad);
    assert!(t.contains("\"block\"") && t.contains("tests.test_cart.CartTest.test_total") && t.contains("Fix it and merge again."), "{t}");
    assert!(!t.contains("gitta sync") && !t.contains("gitta land"), "{t}");
    assert_eq!(stop(&bad), "", "told once per failed merge");
}

#[test]
fn a_changed_meaning_with_passing_tests_merges_and_the_hook_suggests_no_peer_sync() {
    let e = env("meaningok");
    shop(&e);
    assert!(gitta(&e, &e.repo, &["init", "--no-claude", "--gate", "true"]).status.success());
    let (a, b) = (worktree(&e, "vat"), worktree(&e, "label"));
    write(&a.join("shop/pricing.py"), &PRICING.replace("return round(net * (1 + VAT_RATE), 2)", INT_GROSS));
    write(&b.join("shop/label.py"), LABEL);
    let ev = format!("{{\"cwd\": {:?}, \"tool_name\": \"Edit\"}}", b.to_str().unwrap());
    let out = String::from_utf8_lossy(&gitta_in(&e, &b, &["hook", "claude"], &ev).stdout).to_string();
    assert!(out.contains("agent vat changed `net_to_gross`") && out.contains("shop/pricing.py:4: def net_to_gross(net):"), "{out}");
    assert!(!out.contains("--from") && !out.contains("gitta sync") && !out.contains("gitta check") && !out.contains("BREAKS"), "{out}");
    // The same finding is told once.
    let again = String::from_utf8_lossy(&gitta_in(&e, &b, &["hook", "claude"], &ev).stdout).to_string();
    assert!(!again.contains("net_to_gross"), "{again}");
    git(&a, &["add", "-A"]); git(&a, &["commit", "-qm", "vat"]);
    git(&b, &["add", "-A"]); git(&b, &["commit", "-qm", "label"]);
    assert!(git_run(&e, &e.repo, &["merge", "--no-edit", "vat"]).status.success());
    let o = git_run(&e, &e.repo, &["merge", "--no-edit", "label"]);
    assert!(o.status.success(), "a changed meaning never stops a merge whose tests pass: {}", text(&o));
}

#[test]
fn a_failed_merge_with_a_changed_meaning_shows_the_new_definition_and_the_next_step() {
    let e = env("repair");
    shop(&e);
    let gate = "if grep -q 'int(net' shop/pricing.py && test -f shop/label.py; then echo 'FAILED tests/test_label.py::test_label - AssertionError'; echo 'E       AssertionError: assert 12500 == 125.0'; exit 1; fi";
    assert!(gitta(&e, &e.repo, &["init", "--no-claude", "--gate", gate]).status.success());
    let (a, b) = (worktree(&e, "vat"), worktree(&e, "label"));
    write(&a.join("shop/pricing.py"), &PRICING.replace("return round(net * (1 + VAT_RATE), 2)", INT_GROSS));
    git(&a, &["add", "-A"]); git(&a, &["commit", "-qm", "vat"]);
    write(&b.join("shop/label.py"), LABEL);
    git(&b, &["add", "-A"]); git(&b, &["commit", "-qm", "label"]);
    assert!(git_run(&e, &e.repo, &["merge", "--no-edit", "vat"]).status.success());
    let o = git_run(&e, &e.repo, &["merge", "--no-edit", "label"]);
    let t = text(&o);
    assert!(!o.status.success(), "{t}");
    assert!(t.contains("tests/test_label.py::test_label") && t.contains("E       AssertionError: assert 12500 == 125.0") && t.contains("Now on main: shop/pricing.py:4: def net_to_gross(net):") && t.contains("Fix it and merge again."), "{t}");
    assert!(!t.contains("gitta sync") && !t.contains("gitta land"), "{t}");
}

#[test]
fn an_agents_own_merged_change_is_no_changed_meaning_finding() {
    let e = env("ownmerged");
    shop(&e);
    assert!(gitta(&e, &e.repo, &["init", "--no-claude"]).status.success());
    let base = git(&e.repo, &["rev-parse", "main"]);
    let a = worktree(&e, "vat");
    write(&a.join("shop/pricing.py"), &PRICING.replace("return round(net * (1 + VAT_RATE), 2)", INT_GROSS));
    git(&a, &["add", "-A"]); git(&a, &["commit", "-qm", "vat"]);
    assert!(git_run(&e, &e.repo, &["merge", "--no-edit", "vat"]).status.success());
    // vat goes on from where it started (as after a stale copy), changes the helper again and adds code that uses it.
    set_base(&e, "vat", &base);
    write(&a.join("shop/pricing.py"), &format!("{}\n# rounded to whole cents\n", PRICING.replace("return round(net * (1 + VAT_RATE), 2)", INT_GROSS)));
    write(&a.join("shop/label.py"), LABEL);
    let o = gitta(&e, &e.repo, &["land", "vat"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(!text(&o).contains("changed `net_to_gross`"), "vat's own change against vat's own code: {}", text(&o));
}

#[test]
fn a_stopped_merge_cancels_itself_so_the_next_agent_can_merge() {
    let e = env("cancel");
    shop(&e);
    assert!(gitta(&e, &e.repo, &["init", "--no-claude", "--gate", FAILING_GATE]).status.success());
    let (first, bad, next) = (worktree(&e, "first"), worktree(&e, "bad"), worktree(&e, "next"));
    write(&first.join("shop/first.py"), "X = 1\n");
    git(&first, &["add", "-A"]); git(&first, &["commit", "-qm", "first"]);
    assert!(git_run(&e, &e.repo, &["merge", "--no-edit", "first"]).status.success());
    write(&bad.join("shop/cart.py"), &format!("{CART}\n\n# BROKEN\n"));
    git(&bad, &["add", "-A"]); git(&bad, &["commit", "-qm", "bad"]);
    write(&next.join("shop/text.py"), "def whisper(s):\n    return s.lower()\n");
    git(&next, &["add", "-A"]); git(&next, &["commit", "-qm", "next"]);
    let main_before = git(&e.repo, &["rev-parse", "main"]);
    let o = git_run(&e, &e.repo, &["merge", "--no-edit", "bad"]);
    assert!(!o.status.success() && text(&o).contains("merge cancelled"), "{}", text(&o));
    // No `git merge --abort` by anyone: gitta cancels the attempt itself.
    let merge_head = e.repo.join(".git/MERGE_HEAD");
    for _ in 0..500 { if !merge_head.exists() { break; } std::thread::sleep(std::time::Duration::from_millis(10)); }
    assert!(!merge_head.exists(), "the stopped merge was cancelled");
    assert_eq!(git(&e.repo, &["status", "--porcelain", "--untracked-files=no"]), "", "the checkout is clean");
    assert_eq!(git(&e.repo, &["rev-parse", "main"]), main_before);
    assert!(std::fs::read_to_string(bad.join("shop/cart.py")).unwrap().contains("BROKEN"), "the refused agent's work is kept");
    let o = git_run(&e, &e.repo, &["merge", "--no-edit", "next"]);
    assert!(o.status.success(), "the next agent merges: {}", text(&o));
}

#[test]
fn gitta_up_gives_each_agent_its_own_branch_head_and_index() {
    let e = env("upbranches");
    shop(&e);
    let o = gitta(&e, &e.repo, &["up", "-n", "2", "--backend", "worktree"]);
    assert!(o.status.success() && text(&o).contains("branch gitta/a1"), "{}", text(&o));
    let (a1, a2) = (e.repo.join(".gitta/agents/a1"), e.repo.join(".gitta/agents/a2"));
    assert_eq!(git(&a1, &["rev-parse", "--abbrev-ref", "HEAD"]), "gitta/a1");
    assert_eq!(git(&a2, &["rev-parse", "--abbrev-ref", "HEAD"]), "gitta/a2");
    assert_ne!(git(&a1, &["rev-parse", "--git-dir"]), git(&a2, &["rev-parse", "--git-dir"]));
    // Work staged in one agent is not in the other's index, and commits do not stack.
    write(&a1.join("a1.txt"), "1\n");
    git(&a1, &["add", "a1.txt"]);
    assert_eq!(git(&a2, &["status", "--porcelain"]), "");
    git(&a1, &["commit", "-qm", "a1 work"]);
    write(&a2.join("a2.txt"), "2\n");
    git(&a2, &["add", "a2.txt"]); git(&a2, &["commit", "-qm", "a2 work"]);
    assert_eq!(git(&e.repo, &["log", "-1", "--format=%s", "gitta/a2~1"]), "base", "a2's commit sits on the base, not on a1's");
    assert!(gitta(&e, &e.repo, &["down"]).status.success());
}

#[test]
fn gitta_up_after_init_adds_agents_to_the_existing_setup() {
    let e = env("upinit");
    shop(&e);
    assert!(gitta(&e, &e.repo, &["init", "--no-claude", "--gate", FAILING_GATE]).status.success());
    let feat = worktree(&e, "feat");
    let o = gitta(&e, &e.repo, &["up", "-n", "2", "--backend", "worktree"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("integration branch main"), "{}", text(&o));
    let cfg: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(e.repo.join(".gitta/config.json")).unwrap()).unwrap();
    assert_eq!(cfg["watch"], true, "the init setup is kept");
    let names: Vec<&str> = cfg["agents"].as_array().unwrap().iter().map(|a| a["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"feat") && names.contains(&"a1") && names.contains(&"a2"), "{names:?}");
    let st = text(&gitta(&e, &e.repo, &["status"]));
    assert!(st.contains("a1") && st.contains("a2") && st.contains("feat"), "{st}");
    // The same names twice: refused, nothing changed.
    let o = gitta(&e, &e.repo, &["up", "-n", "1"]);
    assert!(!o.status.success() && text(&o).contains("agents a1 already exist"), "{}", text(&o));
    // The new agents branch off main, and git merge checks their work like any other.
    let a1 = e.repo.join(".gitta/agents/a1");
    write(&a1.join("shop/text.py"), "def whisper(s):\n    return s.lower()\n");
    git(&a1, &["commit", "-qam", "whisper"]);
    let o = git_run(&e, &e.repo, &["merge", "--no-edit", "gitta/a1"]);
    assert!(o.status.success() && !text(&o).contains("gitta:"), "{}", text(&o));
    let _ = feat;
}

#[test]
fn cherry_pick_and_rebase_refusals_name_the_persons_commit() {
    let e = env("gitsrc");
    shop(&e);
    gitta(&e, &e.repo, &["init", "--no-claude", "--gate", FAILING_GATE]);
    let bad = worktree(&e, "totals");
    write(&bad.join("shop/cart.py"), &format!("{CART}\n\n# BROKEN\n"));
    git(&bad, &["add", "-A"]); git(&bad, &["commit", "-qm", "totals"]);
    write(&e.repo.join("shop/text.py"), "def shout(s):\n    return s.upper()\n");
    git(&e.repo, &["add", "-A"]); git(&e.repo, &["commit", "-q", "--allow-empty", "-m", "text"]);
    let before = git(&e.repo, &["rev-parse", "main"]);
    let broken = git(&bad, &["rev-parse", "HEAD"]);
    let o = git_run(&e, &e.repo, &["cherry-pick", &broken]);
    assert!(text(&o).contains("tests fail on main with totals "), "cherry-pick: {}", text(&o));
    let _ = git_run(&e, &e.repo, &["cherry-pick", "--abort"]);
    git(&e.repo, &["reset", "-q", "--hard", &before]);
    let o = git_run(&e, &e.repo, &["rebase", "totals"]);
    assert!(text(&o).contains("tests fail on main with totals "), "rebase: {}", text(&o));
    let _ = git_run(&e, &e.repo, &["rebase", "--abort"]);
    assert_eq!(git(&e.repo, &["rev-parse", "main"]), before);
}

#[test]
fn a_git_merge_leaves_other_finished_worktrees_exactly_as_their_agents_left_them() {
    let e = env("cleanwt");
    shop(&e);
    assert!(gitta(&e, &e.repo, &["init", "--no-claude", "--gate", FAILING_GATE]).status.success());
    let (a, b, c) = (worktree(&e, "vat"), worktree(&e, "words"), worktree(&e, "idle"));
    write(&a.join("shop/pricing.py"), &PRICING.replace("0.25", "0.12"));
    git(&a, &["add", "-A"]); git(&a, &["commit", "-qm", "vat"]);
    write(&b.join("shop/text.py"), "def shout(s):\n    return s.upper()\n\n\ndef whisper(s):\n    return s.lower()\n");
    git(&b, &["add", "-A"]); git(&b, &["commit", "-qm", "words"]);
    assert!(git_run(&e, &e.repo, &["merge", "--no-edit", "words"]).status.success());
    let o = git_run(&e, &e.repo, &["merge", "--no-edit", "vat"]);
    assert!(o.status.success(), "{}", text(&o));
    // Committed, merged and idle worktrees get no foreign uncommitted changes: their branches stay on main.
    for w in [&a, &b, &c] {
        let st = git_run(&e, w, &["status", "--porcelain", "--untracked-files=no"]);
        assert!(text(&st).trim().is_empty(), "{}: {}", w.display(), text(&st));
    }
    for br in ["vat", "words"] { assert!(git_run(&e, &e.repo, &["merge-base", "--is-ancestor", br, "main"]).status.success(), "{br}"); }
    let st = text(&gitta(&e, &e.repo, &["status"]));
    assert!(!st.contains("both edit"), "finished work clashes with no one: {st}");
}

#[test]
fn team_rules_in_gitta_toml_stop_agent_work_with_a_plain_line() {
    let e = env("policy");
    shop(&e);
    write(&e.repo.join("infra/main.tf"), "x = 1\n");
    write(&e.repo.join("db/migrations/0001.sql"), "create table a (id int);\n");
    write(&e.repo.join(".gitta.toml"), "[policy]\nagents_push_to_main = \"block\"\nagents_never_change = [\"infra/**\"]\nneeds_approval = [\"db/migrations/**\"]\n");
    git(&e.repo, &["add", "-A"]); git(&e.repo, &["commit", "-qm", "rules"]);
    assert!(gitta(&e, &e.repo, &["init", "--no-claude"]).status.success());
    // An agent changes a path agents never change: its merge fails like a check, and main stays
    let a = worktree(&e, "infra-edit");
    write(&a.join("infra/main.tf"), "x = 2\n");
    git(&a, &["commit", "-qam", "infra"]);
    let before = git(&e.repo, &["rev-parse", "main"]);
    let o = git_run(&e, &e.repo, &["merge", "--no-edit", "infra-edit"]);
    assert!(!o.status.success() && text(&o).contains("agents never change infra/**"), "{}", text(&o));
    assert_eq!(git(&e.repo, &["rev-parse", "main"]), before);
    // needs_approval: the agent merging itself is refused; a person running the merge is the approval
    let m = worktree(&e, "migrate");
    write(&m.join("db/migrations/0002.sql"), "alter table a add b int;\n");
    git(&m, &["add", "-A"]); git(&m, &["commit", "-qm", "migration"]);
    let o = git_run_agent(&e, &e.repo, &["merge", "--no-edit", "migrate"]);
    assert!(!o.status.success() && text(&o).contains("need a person's approval"), "{}", text(&o));
    let o = git_run(&e, &e.repo, &["merge", "--no-edit", "migrate"]);
    assert!(o.status.success(), "{}", text(&o));
    // An agent may not push to main directly
    let remote = e.dir.join("remote.git");
    git(&e.dir, &["init", "-q", "--bare", remote.to_str().unwrap()]);
    git(&e.repo, &["remote", "add", "origin", remote.to_str().unwrap()]);
    let o = git_run_agent(&e, &e.repo, &["push", "-q", "origin", "main"]);
    assert!(!o.status.success() && text(&o).contains("may not push to main directly"), "{}", text(&o));
    assert!(git_run(&e, &e.repo, &["push", "-q", "origin", "main"]).status.success(), "a person may push");
    // The pull request check: an AI co-author makes it agent work; an approving review covers needs_approval only
    let pr = worktree(&e, "pr-infra");
    write(&pr.join("infra/main.tf"), "x = 3\n");
    git(&pr, &["commit", "-qam", "infra\n\nCo-Authored-By: Claude <noreply@anthropic.com>"]);
    let o = gitta(&e, &e.repo, &["pr-check", "--pr", "pr-infra", "--base", "main", "--approved"]);
    assert_eq!(o.status.code(), Some(3), "{}", text(&o));
    assert!(text(&o).contains("Breaks a team rule") && text(&o).contains("infra/**"), "{}", text(&o));
    let pm = worktree(&e, "pr-mig");
    write(&pm.join("db/migrations/0003.sql"), "select 1;\n");
    git(&pm, &["add", "-A"]); git(&pm, &["commit", "-qm", "mig\n\nCo-Authored-By: Claude <noreply@anthropic.com>"]);
    assert_eq!(gitta(&e, &e.repo, &["pr-check", "--pr", "pr-mig", "--base", "main"]).status.code(), Some(3));
    assert_eq!(gitta(&e, &e.repo, &["pr-check", "--pr", "pr-mig", "--base", "main", "--approved"]).status.code(), Some(0));
    // A server rule adds to the repository's and names its source
    write(&e.repo.join(".gitta/server-policy.toml"), "[policy]\nagents_never_change = [\"shop/pricing.py\"]\n");
    let s = worktree(&e, "price");
    write(&s.join("shop/pricing.py"), "VAT_RATE = 0.2\n");
    git(&s, &["commit", "-qam", "vat"]);
    let o = git_run(&e, &e.repo, &["merge", "--no-edit", "price"]);
    assert!(!o.status.success() && text(&o).contains("team policy on the gitta server"), "{}", text(&o));
    // A broken rules section fails the check instead of turning the rules off
    write(&e.repo.join(".gitta.toml"), "[policy]\nagents_never_change = [\"infra/**\"\n");
    git(&e.repo, &["commit", "-qam", "broken rules"]);
    let o = git_run(&e, &e.repo, &["merge", "--no-edit", "infra-edit"]);
    assert!(!o.status.success() && text(&o).contains("cannot read the rules"), "{}", text(&o));
    // Every refusal is a stopped merge in the report
    let r = text(&gitta(&e, &e.repo, &["report", "--period", "all", "--csv"]));
    let total = r.lines().find(|l| l.starts_with("all,all,all,")).unwrap_or_else(|| panic!("{r}"));
    let n: Vec<u64> = total.split(',').skip(3).map(|x| x.parse().unwrap()).collect();
    assert!(n[3] >= 4, "stopped merges: {r}");
}

#[test]
fn report_counts_agents_changes_and_stopped_merges_in_three_formats() {
    let e = env("report");
    shop(&e);
    write(&e.repo.join("tests/test_cart.py"), "from shop.cart import total\n\n\ndef test_total():\n    assert total(10) == 12.5\n");
    git(&e.repo, &["add", "-A"]); git(&e.repo, &["commit", "-qm", "tests"]);
    assert!(gitta(&e, &e.repo, &["init", "--no-claude", "--gate", "grep -q 0.25 shop/pricing.py"]).status.success());
    let good = worktree(&e, "whisper");
    write(&good.join("shop/text.py"), "def shout(s):\n    return s.upper()\n\n\ndef whisper(s):\n    return s.lower()\n");
    let bad = worktree(&e, "vat");
    write(&bad.join("shop/pricing.py"), PRICING.replace("0.25", "0.3").as_str());
    assert!(gitta(&e, &e.repo, &["land", "whisper"]).status.success());
    let o = gitta(&e, &e.repo, &["land", "vat"]);
    assert!(!o.status.success(), "{}", text(&o));
    {
        // the failing tests stopped a broken main
        let j: serde_json::Value = serde_json::from_str(&text(&gitta(&e, &e.repo, &["report", "--json", "--period", "all"]))).unwrap();
        let tot = j["rows"].as_array().unwrap().iter().find(|r| r["repo"] == "all").unwrap().clone();
        assert_eq!(tot["changes"], 1);
        assert!(tot["merges_stopped"].as_u64().unwrap() >= 1);
        assert_eq!(tot["broken_mains_prevented"], 1, "{j}");
    }
    let t = text(&gitta(&e, &e.repo, &["report"]));
    assert!(t.starts_with("period") && t.contains("whisper") && t.contains("total"), "{t}");
    let c = text(&gitta(&e, &e.repo, &["report", "--csv", "--by", "person"]));
    assert!(c.starts_with("period,repo,person,agents_run,changes,clashes_found,merges_stopped,broken_mains_prevented"), "{c}");
    assert!(!gitta(&e, &e.repo, &["report", "--period", "year"]).status.success());
}
