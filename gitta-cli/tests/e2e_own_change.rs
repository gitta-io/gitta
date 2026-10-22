//! An agent's own change, or another agent's change it already pulled in with `gitta sync --from`, is never a
//! changed-meaning hazard against itself. Found in the token-ROI bench round 2 (azure-devops-mcp, flynt, requests):
//! agent-b ran `gitta sync agent-b --from agent-a`, agent-a landed, and agent-b was refused for code that was
//! identical on both sides.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Env { dir: PathBuf, home: PathBuf, repo: PathBuf }

fn env(tag: &str) -> Env {
    let dir = std::env::temp_dir().join(format!("gitta-e2e-own-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (home, repo) = (dir.join("home"), dir.join("repo"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    Env { dir, home, repo }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn gitta(e: &Env, dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_gitta")).args(args).current_dir(dir).env("HOME", &e.home).output().unwrap()
}

fn text(o: &Output) -> String { format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)) }

fn write(p: &Path, s: &str) { if let Some(d) = p.parent() { std::fs::create_dir_all(d).unwrap(); } std::fs::write(p, s).unwrap(); }

fn worktree(e: &Env, name: &str) -> PathBuf {
    let p = e.dir.join(format!("wt-{name}"));
    git(&e.repo, &["worktree", "add", "-q", "-b", name, p.to_str().unwrap()]);
    p
}

const PRICING: &str = "VAT_RATE = 0.25\n\n\ndef net_to_gross(net):\n    return round(net * (1 + VAT_RATE), 2)\n";
const TEST_PRICING: &str = "from shop.pricing import net_to_gross\n\n\ndef test_gross():\n    assert net_to_gross(100) == 112\n";

fn setup(tag: &str) -> Env {
    let e = env(tag);
    write(&e.repo.join("shop/__init__.py"), "");
    write(&e.repo.join("shop/pricing.py"), PRICING);
    write(&e.repo.join("shop/text.py"), "def shout(s):\n    return s.upper()\n");
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "base"]);
    assert!(gitta(&e, &e.repo, &["init", "--no-claude"]).status.success());
    e
}

#[test]
fn work_pulled_in_from_the_agent_that_landed_is_no_hazard() {
    let e = setup("from");
    let (a, b) = (worktree(&e, "vat"), worktree(&e, "loud"));
    // agent-a changes a helper and adds a test that uses it (azure-devops-mcp: builds.ts and builds.test.ts).
    write(&a.join("shop/pricing.py"), &PRICING.replace("0.25", "0.12"));
    write(&a.join("tests/test_pricing.py"), TEST_PRICING);
    // agent-b does unrelated work, then pulls agent-a's work into its copy (the hook no longer suggests this).
    write(&b.join("shop/text.py"), "def shout(s):\n    return s.upper()\n\n\ndef loud(s):\n    return s.upper() + '!'\n");
    let o = gitta(&e, &b, &["sync", "loud", "--from", "vat"]);
    assert!(o.status.success(), "{}", text(&o));
    let o = gitta(&e, &e.repo, &["land", "vat"]);
    assert!(o.status.success(), "{}", text(&o));
    let o = gitta(&e, &e.repo, &["land", "loud"]);
    assert!(o.status.success(), "agent-b's copy holds exactly what landed; nothing changed under it: {}", text(&o));
}

#[test]
fn a_landed_change_is_still_reported_for_new_code_that_did_not_see_it() {
    let e = setup("real");
    let (a, b) = (worktree(&e, "vat"), worktree(&e, "label"));
    write(&a.join("shop/pricing.py"), &PRICING.replace("0.25", "0.12"));
    write(&b.join("shop/label.py"), "from shop.pricing import net_to_gross\n\n\ndef label(x):\n    return f\"{net_to_gross(x)} kr incl. 25% VAT\"\n");
    assert!(gitta(&e, &e.repo, &["land", "vat"]).status.success());
    let o = gitta(&e, &e.repo, &["land", "label"]);
    // Information, not a gate: the tests on the combined code decide (none are set here).
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("vat was merged into main and changed `net_to_gross`"), "{}", text(&o));
}
