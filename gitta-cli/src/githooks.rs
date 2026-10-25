//! gitta inside normal Git: the person's (or agent's) own `git merge`, `git pull` and `git push` to the target
//! branch run the checks. pre-merge-commit checks a merge before its commit is made; reference-transaction
//! checks a fast-forward (and every other way the target moves) before the branch moves; pre-push checks what
//! is pushed to the target. post-merge moves a failed fast-forward back on git older than 2.28.
//! Existing hooks keep running first (they are chained), and core.hooksPath is respected.

use super::*;

const HOOKS: [&str; 6] = ["pre-merge-commit", "post-merge", "pre-push", "reference-transaction", "post-rewrite", "post-commit"];

/// Git 2.28 and later run `reference-transaction` for every ref update; older versions get a warning-only fallback.
pub(crate) fn has_ref_transaction() -> bool {
    let v = Command::new("git").arg("--version").output().map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
    let n: Vec<u32> = v.split_whitespace().nth(2).unwrap_or("0").split('.').take(2).map(|x| x.parse().unwrap_or(0)).collect();
    (n.first().copied().unwrap_or(0), n.get(1).copied().unwrap_or(0)) >= (2, 28)
}
const MARK: &str = "# gitta: checks the combined code before it reaches the target branch";

fn hooks_dir(root: &Path) -> PathBuf {
    let d = git_ok(root, &["rev-parse", "--path-format=absolute", "--git-path", "hooks"]);
    PathBuf::from(d)
}

/// The hooks folder git uses, when it is not this repository's own: core.hooksPath points to a folder that other
/// repositories share, or to one in the working tree. gitta never writes there, or every repository using that folder
/// would run its hooks.
pub(crate) fn foreign_hooks_dir(root: &Path) -> Option<PathBuf> {
    let used = hooks_dir(root);
    let own = PathBuf::from(git_ok(root, &["rev-parse", "--path-format=absolute", "--git-common-dir"])).join("hooks");
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    (canon(&used) != canon(&own)).then_some(used)
}

/// Install the three hooks. A hook that was there before is kept as `<name>.pre-gitta` and runs first.
pub(crate) fn install(root: &Path) -> Vec<String> {
    let dir = hooks_dir(root);
    let _ = std::fs::create_dir_all(&dir);
    let exe = std::env::current_exe().map(|p| p.to_string_lossy().to_string()).unwrap_or("gitta".into());
    let mut chained = vec![];
    let modern = has_ref_transaction();
    for h in HOOKS {
        if (h == "reference-transaction" && !modern) || (h == "post-rewrite" && modern) { continue; }
        let p = dir.join(h);
        let old = std::fs::read_to_string(&p).unwrap_or_default();
        if !old.is_empty() && !old.contains(MARK) {
            let _ = std::fs::rename(&p, dir.join(format!("{h}.pre-gitta")));
            chained.push(h.to_string());
        }
        let script = format!("#!/bin/sh\n{MARK}\nprev=\"$(dirname \"$0\")/{h}.pre-gitta\"\n{}\nexec {:?} git-hook {h} \"$@\"\n",
            if h == "pre-push" || h == "reference-transaction" {
                "input=$(cat)\nif [ -x \"$prev\" ]; then printf '%s\\n' \"$input\" | \"$prev\" \"$@\" || exit $?; fi\n{FAST}printf '%s\\n' \"$input\" | {EXE} git-hook {HOOK} \"$@\"\nexit $?".replace("{EXE}", &format!("{exe:?}")).replace("{HOOK}", h).replace("{FAST}", if h == "reference-transaction" { "[ \"$1\" = prepared ] || exit 0\ncase \"$input\" in *refs/heads/*) ;; *) exit 0 ;; esac\n" } else { "" })
            } else { "if [ -x \"$prev\" ]; then \"$prev\" \"$@\" || exit $?; fi".to_string() }, exe);
        let _ = std::fs::write(&p, script);
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)); }
    }
    // Two merges into main at the same moment: the second waits for the first one's lock instead of failing at once
    // with "Unable to create HEAD.lock" (git's default wait is 0.1 s). Only when the person has not set it.
    if git(root, &["config", "--local", "--get", "core.filesRefLockTimeout"]).stdout.is_empty() {
        let _ = git(root, &["config", "--local", "core.filesRefLockTimeout", REF_LOCK_WAIT_MS]);
    }
    install_merge_driver(root, &exe);
    chained
}

const DRIVER_MARK: &str = "# gitta: merges code function by function in every git merge";
/// Files the structural merger reads; everything else keeps Git's own merge.
const DRIVER_FILES: &[&str] = &["*.py", "*.ts", "*.tsx", "*.mts", "*.cts", "*.js", "*.jsx", "*.mjs", "*.cjs", "*.go", "*.rs", "*.java", "*.cs", "*.kt", "*.kts",
    "*.c", "*.h", "*.cc", "*.cpp", "*.cxx", "*.hpp", "*.hh", "*.json", "requirements*.txt"];

fn attributes_path(root: &Path) -> PathBuf { PathBuf::from(git_ok(root, &["rev-parse", "--path-format=absolute", "--git-path", "info/attributes"])) }

/// An agent's own `git merge` gets the same function-by-function merge as `gitta land`, so two agents' edits to
/// different parts of one file never reach the agent as conflict markers. Set in .git/info/attributes (shared by
/// every worktree, never committed) and the local config.
fn install_merge_driver(root: &Path, exe: &str) {
    let _ = git(root, &["config", "--local", "merge.gitta.name", "gitta structural merge"]);
    let _ = git(root, &["config", "--local", "merge.gitta.driver", &format!("{exe:?} git-hook merge-driver %O %A %B %P %L")]);
    let p = attributes_path(root);
    let old = std::fs::read_to_string(&p).unwrap_or_default();
    if old.contains(DRIVER_MARK) { return; }
    if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
    let mut t = old; if !t.is_empty() && !t.ends_with('\n') { t.push('\n'); }
    t.push_str(DRIVER_MARK); t.push('\n');
    for f in DRIVER_FILES { t.push_str(&format!("{f} merge=gitta\n")); }
    t.push_str(&format!("{DRIVER_MARK} (end)\n"));
    let _ = std::fs::write(&p, t);
}

fn uninstall_merge_driver(root: &Path) {
    let _ = git(root, &["config", "--local", "--remove-section", "merge.gitta"]);
    let p = attributes_path(root);
    let Ok(t) = std::fs::read_to_string(&p) else { return };
    let (mut out, mut skip) = (String::new(), false);
    for l in t.lines() {
        if l == DRIVER_MARK { skip = true; continue; }
        if skip { if l.starts_with(DRIVER_MARK) { skip = false; } continue; }
        out.push_str(l); out.push('\n');
    }
    let _ = std::fs::write(&p, out);
}

/// `gitta git-hook merge-driver %O %A %B %P %L`: Git's merge driver. The structural merge writes the result to %A
/// and exits 0; when it cannot merge, Git's own line merge runs, so the person sees exactly what Git would show.
fn merge_driver(a: &[String]) -> ! {
    let (o, ours, theirs, path) = (&a[0], &a[1], &a[2], a.get(3).cloned().unwrap_or_default());
    let size = a.get(4).cloned().unwrap_or_else(|| "7".into());
    let read = |f: &str| std::fs::read(f).ok();
    if let Ok(Some(m)) = gitta_core::merge::merge_file(&path, read(o).as_deref(), read(ours).as_deref(), read(theirs).as_deref()) {
        if std::fs::write(ours, m).is_ok() { std::process::exit(0) }
    }
    let st = Command::new("git").args(["merge-file", "--marker-size", &size, "-L", "HEAD", "-L", "base", "-L", "incoming", ours, o, theirs]).status();
    std::process::exit(st.ok().and_then(|s| s.code()).unwrap_or(1))
}

/// Milliseconds a git command waits for a branch lock in a gitta repository.
const REF_LOCK_WAIT_MS: &str = "30000";

/// Remove gitta's hooks and put back the ones they chained.
pub(crate) fn uninstall(root: &Path) {
    let dir = hooks_dir(root);
    uninstall_merge_driver(root);
    if String::from_utf8_lossy(&git(root, &["config", "--local", "--get", "core.filesRefLockTimeout"]).stdout).trim() == REF_LOCK_WAIT_MS {
        let _ = git(root, &["config", "--local", "--unset", "core.filesRefLockTimeout"]);
    }
    for h in HOOKS {
        let p = dir.join(h);
        if std::fs::read_to_string(&p).map(|t| t.contains(MARK)).unwrap_or(false) {
            let _ = std::fs::remove_file(&p);
            let prev = dir.join(format!("{h}.pre-gitta"));
            if prev.exists() { let _ = std::fs::rename(&prev, &p); }
        }
    }
}

fn say(l: &str) { eprintln!("gitta: {l}"); }

/// Which agent made `rev`: the agent whose branch or worktree HEAD is that commit; otherwise the branch name.
fn who_made(root: &Path, c: &Config, rev: &str) -> (String, Option<Agent>) {
    for a in &c.agents {
        if a.upper.is_empty() && git_ok(Path::new(&a.path), &["rev-parse", "HEAD"]) == rev { return (a.name.clone(), Some(a.clone())); }
    }
    let n = git_ok(root, &["name-rev", "--name-only", "--exclude=refs/tags/*", rev]);
    let n = n.trim_start_matches("remotes/").split(['~', '^']).next().unwrap_or("").to_string();
    (if n.is_empty() || n == "undefined" { rev[..rev.len().min(8)].to_string() } else { n }, None)
}

/// The commit the person named, for a commit git is making from it: a rebase names its `onto` commit, and a
/// cherry-pick copies a commit with the same author and author date (the source is found among the branches).
/// Otherwise `new` itself.
fn source_of(root: &Path, git_dir: &Path, sub: &str, new: &str) -> String {
    for d in ["rebase-merge", "rebase-apply"] {
        if let Ok(o) = std::fs::read_to_string(git_dir.join(d).join("onto")) { if !o.trim().is_empty() { return o.trim().to_string(); } }
    }
    if sub == "cherry-pick" || sub == "revert" {
        if let Ok(h) = std::fs::read_to_string(git_dir.join("CHERRY_PICK_HEAD")) { return h.trim().to_string(); }
        let id = git_ok(root, &["log", "-1", "--format=%an <%ae> %ad %s", "--date=raw", new]);
        let found = git_ok(root, &["log", "--branches", "-n", "5000", "--format=%H %an <%ae> %ad %s", "--date=raw"]);
        if let Some(h) = found.lines().filter_map(|l| l.split_once(' ')).find(|(h, rest)| *h != new && *rest == id).map(|(h, _)| h.to_string()) { return h; }
    }
    new.to_string()
}

/// The check every merge path shares: weakened tests or security (asks the person), what changed under the
/// incoming code (information), and the tests on the combined code in `dir`. Ok means the merge may go ahead.
fn check(root: &Path, c: &Config, at: &str, target: &str, onto: &str, incoming: &str, how: &str) -> Result<(), String> {
    check_from(root, c, at, target, onto, incoming, how, incoming)
}

/// `check`, with the agent or branch named after `source`: the person's commit when `incoming` is one git just made from it.
/// `at` is a commit holding exactly the code the target would get. Its tests run in a checkout of gitta's own, never
/// in the target's checkout: another agent's git command can change that one while the tests run (measured, token
/// benchmark Oct 2026: two refusals of good work because a second merge wrote its files under the first one's tests).
#[allow(clippy::too_many_arguments)]
fn check_from(root: &Path, c: &Config, at: &str, target: &str, onto: &str, incoming: &str, how: &str, source: &str) -> Result<(), String> {
    let (who, ag) = who_made(root, c, source);
    let mut base = git_ok(root, &["merge-base", onto, incoming]);
    // The target moves back or sideways to an older commit (reset, update-ref): compare with where it was.
    if base == git_ok(root, &["rev-parse", incoming]) { base = git_ok(root, &["rev-parse", onto]); }
    let all: BTreeSet<String> = git_ok(root, &["diff", "--name-only", "--no-renames", &base, incoming]).lines().map(String::from).collect();
    let changed = all.len();
    // What the merge brings to the target: files that already match it there are not new.
    let (on, inc) = (blobs(root, onto, &all), blobs(root, incoming, &all));
    let paths: BTreeSet<String> = all.into_iter().filter(|p| s(&on, p) != s(&inc, p)).collect();
    if paths.is_empty() {
        // git named changed files, yet nothing read as different and we read NO content on either side. A genuine
        // no-op merge (onto already holds incoming's content) leaves `on`/`inc` populated with equal content; both
        // coming back empty is a blob-read regression, and returning Ok() here would silently re-open the approve
        // path (a merge allowed with no checks). Fail loud instead -- never approve on content we could not read.
        if vacuous_read(changed, &on, &inc) {
            die(&format!("gitta read no content for {changed} changed file(s) between {onto} and {incoming}; refusing to approve the {how} on data it could not read"));
        }
        return Ok(());
    }
    // Removed tests, weakened checks and security rules: the person's merge command is the approval.
    let (b, n) = (blobs(root, &base, &paths), blobs(root, incoming, &paths));
    // Same guard for the reads that feed the safety-net and security checks: `paths` is non-empty here, so reading
    // nothing at all on both sides is a read regression, not a real change -- never run the checks on empty content.
    if vacuous_read(paths.len(), &b, &n) {
        die(&format!("gitta read no content at {base} or {incoming} for {} changed file(s); refusing to run the safety-net and security checks on data it could not read", paths.len()));
    }
    let list: Vec<String> = paths.iter().cloned().collect();
    // A credential on a line the merge adds is refused on every route, as `gitta land` refuses it.
    let txt = |s: &gitta_core::Snapshot, p: &str| s.get(p).map(|x| String::from_utf8_lossy(x).to_string()).unwrap_or_default();
    for p in &list {
        let Some((line, label)) = gitta_core::guard::secret_in_new_lines(&txt(&b, p), &txt(&n, p)) else { continue };
        let what = format!("{p}:{line} looks like it contains a credential ({label})");
        audit::record(root, "blocked", &who, &audit::actor(root), serde_json::json!({"setting": "credentials", "findings": [what]}));
        log(root, "refused", &who, &format!("{who} was not merged: {what}."));
        return Err(format!("{who} was not merged: {what}; {{state}}. Remove it and use configuration or a secret store."));
    }
    let mut f = gitta_core::guard::safety_net(&b, &n, &list);
    let rank = block_rank(c);
    for x in gitta_core::code_security::findings(&b, &n, &list).iter().filter(|x| blocks(x, rank)) {
        f.push(gitta_core::guard::Finding { kind: "SECURITY_RULE".into(), path: x.path.clone(), detail: format!("adds code that {} (line {})", x.text.trim_end_matches('.'), x.line) });
    }
    if !weakened_gate(root, c, &who, &f) {
        return Err(format!("{who} was not merged: .gitta.toml sets weakened_checks to block; {{state}}. Restore what it removed."));
    }
    // The team's rules for agents ([policy] in .gitta.toml, plus the team server's): a person running the merge
    // outside an agent session is the approval that needs_approval asks for.
    if ag.is_some() || in_agent() {
        let rules = crate::policy::load(root, &c.main).map_err(|e| format!("gitta cannot read the rules: {e}; {{state}}. Fix .gitta.toml on {target}."))?;
        if let Err(lines) = rules.check_agent_change(&who, &list, !in_agent()) {
            crate::policy_refused(root, &who, &lines);
            return Err(format!("{}; {{state}}.", lines.join(" ").trim_end_matches('.')));
        }
    }
    // Something the incoming code calls changed on the target since it branched off: say so, the tests decide.
    let mut notes = 0;
    let mut hz = vec![];
    if let Some(a) = &ag {
        let a2 = Agent { base: base.clone(), ..a.clone() };
        let mine: BTreeSet<String> = paths.clone();
        hz = meaning_hazards(root, &a2, onto, &mine);
        for w in hz.iter().filter(|w| w.kind != "SECURITY" && !told_before(root, &who, &format!("`{}`", short_sym(&w.symbol.clone().unwrap_or_default())))) {
            let side = if w.changed_by.as_deref() == Some("head") { "main" } else { who.as_str() };
            let lander = story_lander(root, side, &w.symbol.clone().unwrap_or_default(), &w.path, &who);
            log(root, "note", &who, &format!("{}. The tests on the combined code decide.", meaning_story(&who, side, &w.symbol.clone().unwrap_or_default(), &w.path, lander.as_deref(), true).replace(" now has the new version", " gets the new version in this merge").replace(" now has that code too", " gets that code in this merge")));
            notes += 1;
        }
    }
    let what = format!("{who} ({})", files_said(&paths.iter().cloned().collect::<Vec<_>>()));
    let noted = if notes > 0 { format!(", {notes} {} in .gitta/gitta.log", if notes == 1 { "note" } else { "notes" }) } else { String::new() };
    let Some(gate) = &c.gate else {
        if !in_agent() { say(&format!("checked {what} against {target}. No test command is set{noted}.")); }
        log(root, "landed", &who, &format!("{who} was merged into {target} with {how}. No test command is set, so no tests ran."));
        audit::record(root, "merge_checked", &who, &audit::actor(root), serde_json::json!({"onto": target, "incoming": incoming, "how": how, "files": paths.iter().map(|p| serde_json::json!({"path": p})).collect::<Vec<_>>(), "tests": {"result": "no test command"}}));
        return Ok(());
    };
    let t0 = std::time::Instant::now();
    // The same tree with the same test command was already tested when the agent committed (or when main last
    // moved): use that result, so git holds the branch lock for a moment, not for the whole test run.
    let tree = soft(root, &["rev-parse", &format!("{at}^{{tree}}")]).unwrap_or_default();
    let (ok, out, code, cached) = match gate_result(root, c, gate, &tree, at) {
        Some((ok, out, code, cached)) => (ok, out, code, cached),
        None => return Err(format!("gitta could not read the code this would give {target}; {{state}}. Run the same git command again.")),
    };
    let secs = (t0.elapsed().as_secs_f64() * 10.0).round() / 10.0;
    if ok {
        // A gate that exits 0 having run NO tests (pytest collected nothing, `go test` with no test files, a
        // placeholder script) has verified nothing: do not credit it as a pass. Conservative -- only well-known
        // "ran nothing" signatures, so a genuine pass is never downgraded.
        if let Some(sig) = zero_tests(&out) {
            log(root, "warning", &who, &format!("The test command exited 0 but ran no tests ({sig}); gitta is not treating that as a verified pass."));
            audit::record(root, "merge_stopped", &who, &audit::actor(root), serde_json::json!({"onto": target, "incoming": incoming, "how": how, "tests": {"command": gate, "result": "no_tests_ran", "signature": sig}, "features": gitta_core::guard::features()}));
            return Err(format!("the test command ran no tests ({sig}), so nothing was verified; {{state}}. Point .gitta.toml's gate at the test suite and run the same git command again."));
        }
        // Silent when the combined tests pass: git's own output is all the agent needs; the story is in the log.
        let _ = (&what, &noted);
        log(root, "landed", &who, &format!("Tests passed on {target} combined with {} changes. {who} was merged into {target} with {how}.", poss(&who)));
        audit::record(root, "merge_checked", &who, &audit::actor(root), serde_json::json!({"onto": target, "incoming": incoming, "how": how, "files": paths.iter().map(|p| serde_json::json!({"path": p})).collect::<Vec<_>>(), "tests": {"command": gate, "result": "passed", "seconds": secs, "tested_at_commit": cached}, "features": gitta_core::guard::features()}));
        return Ok(());
    }
    if let Some(tool) = missing_tool(&out, code) {
        tell_person(root, &format!("setup|{tool}"), &format!("The tests could not run because {tool} is not installed in {}. Install the project's dependencies there, then run the same git command again.", root.display()));
        return Err(format!("the tests could not run: {tool} was not found, a setup problem, not {who}'s code; {{state}}. Install it and merge again."));
    }
    let failing = failing_tests(&out);
    let errs = first_errors(&out, 6);
    let defs = landed_definitions(root, &base, onto, &hz);
    if let Some(a) = &ag { save_test_hold(root, a, &changed_paths(a, &c.link), &main_commit(c), &failing, &out, &defs); }
    write_failure_package(root, c, onto, incoming, at, &tree, &failing, &out);
    log(root, "refused", &who, &format!("gitta stopped the {how}: tests fail on {target} combined with {who}{}.", failing.first().map(|t| format!(" ({t})")).unwrap_or_default()));
    audit::record(root, "merge_stopped", &who, &audit::actor(root), serde_json::json!({"onto": target, "incoming": incoming, "how": how, "failing": failing, "features": gitta_core::guard::features()}));
    // One line for the agent, the first error as evidence, and the new form of a function its code relies on.
    Err(format!("tests fail on {target} with {who}{}; {{state}}. Fix it and merge again.{}",
        match failing.len() { 0 => String::new(), 1 => format!(" ({})", failing[0]), k => format!(" ({k} tests: {})", failing.iter().take(3).cloned().collect::<Vec<_>>().join(", ")) },
        errs.first().map(|l| format!("\n    {l}")).unwrap_or_default()
            + &defs.first().map(|d| format!("\nNow on {target}: {d}")).unwrap_or_default()))
}

/// A refusal with what happened to the target filled in: in the `{state}` slot, or as a closing sentence.
fn fin(e: &str, state: &str) -> String {
    if e.contains("{state}") { e.replace("{state}", state) } else { format!("{e} {}{}.", state[..1].to_uppercase(), &state[1..]) }
}

/// True when the command runs inside an AI agent's tool call: what gitta prints there enters the agent's context.
pub(crate) fn in_agent() -> bool {
    // The full agent-session list, the same one `gitta approve` uses: a Cursor or Claude Code entrypoint commit on
    // main must be checked too, not only CLAUDECODE/GITTA_AGENT/CODEX_SANDBOX.
    AGENT_ENV.iter().any(|v| std::env::var_os(v).is_some())
}

/// The git subcommand that runs this hook (commit, cherry-pick, rebase, reset, update-ref ...), from the process tree.
fn git_command() -> String { git_cmdline().0 }

/// A changed-file set whose blob reads came back empty on BOTH sides: git named changed files, yet we read no
/// content anywhere. Post the blobs() fix a tree-present blob never reads empty, so this only trips on a read
/// regression -- and it must fail loud, never fall through to the approve / no-check path. A genuine no-op merge
/// (one side already holds the other's content) leaves the snapshots populated with equal content, so it does not
/// trip this. With no changed files it is an ordinary empty diff, also not vacuous.
fn vacuous_read(changed: usize, a: &Snapshot, b: &Snapshot) -> bool { changed > 0 && a.is_empty() && b.is_empty() }

/// Parent count from a `git rev-list --parents -n1` line: the commit id followed by its parent ids. `None` when the
/// output is empty (unexpected state) so the caller bails -- `count - 1` on 0 underflows to usize::MAX in release
/// and would be read as a merge commit, skipping the post-merge check.
fn parent_count(rev_list_line: &str) -> Option<usize> {
    rev_list_line.split_whitespace().count().checked_sub(1)
}

/// The git subcommand that runs this hook and its whole command line.
fn git_cmdline() -> (String, String) {
    let mut cmdline = String::new();
    let mut pid = parent_pid();
    for _ in 0..4 {
        let o = Command::new("ps").args(["-o", "ppid=,args=", "-p", &pid]).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
        let (pp, args) = o.split_once(' ').map(|(a, b)| (a.trim().to_string(), b.trim().to_string())).unwrap_or_default();
        if args.split_whitespace().next().map(|x| x == "git" || x.ends_with("/git")).unwrap_or(false) { cmdline = args; break; }
        if pp.is_empty() { break; }
        pid = pp;
    }
    let w: Vec<&str> = cmdline.split_whitespace().collect();
    let mut i = 1;
    while i < w.len() && w[i].starts_with('-') { i += if matches!(w[i], "-C" | "-c") { 2 } else { 1 }; }
    (w.get(i).copied().unwrap_or("").to_string(), cmdline.clone())
}

fn verified(root: &Path) -> PathBuf { root.join(".gitta/verified") }
fn mark_verified(root: &Path, rev: &str) { if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(verified(root)) { let _ = writeln!(f, "{rev}"); } }
fn is_verified(root: &Path, rev: &str) -> bool { std::fs::read_to_string(verified(root)).unwrap_or_default().lines().any(|l| l == rev) }

/// `gitta git-hook <name>`: run by Git. Exit 0 lets the command go ahead.
pub(crate) fn run(args: &[String]) {
    // Git sets these for its hooks; gitta's own git calls work in other folders and need them gone.
    let name = args.first().map(String::as_str).unwrap_or("");
    if name == "merge-driver" && args.len() >= 4 { merge_driver(&args[1..]) }
    if std::env::var_os("GITTA_LANDING").is_some() { return; }
    // Every ref update in the repository runs this hook: everything but the target branch leaves at once.
    if name == "reference-transaction" && args.get(1).map(String::as_str) != Some("prepared") { return; }
    let mut input = String::new();
    if matches!(name, "reference-transaction" | "pre-push") { std::io::stdin().read_to_string(&mut input).ok(); }
    if name == "reference-transaction" {
        let here = std::env::current_dir().unwrap_or_default();
        let common = Command::new("git").arg("-C").arg(&here).args(["rev-parse", "--path-format=absolute", "--git-common-dir"]).output().ok().map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()));
        let main = common.and_then(|g| g.parent().map(|r| r.join(".gitta/config.json"))).and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()).and_then(|v| v["main"].as_str().map(String::from));
        let Some(main) = main else { return };
        if !input.lines().any(|l| l.split_whitespace().nth(2) == Some(main.as_str())) { return; }
        // A plain commit is the person's own work and leaves at once. An agent's commit is checked like a merge.
        let gd = Command::new("git").arg("-C").arg(&here).args(["rev-parse", "--path-format=absolute", "--git-dir"]).output().map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim())).unwrap_or_default();
        if git_command() == "commit" && !gd.join("SQUASH_MSG").exists() && !gd.join("MERGE_HEAD").exists() && !in_agent() { return; }
    }
    GATE_QUIET.store(true, std::sync::atomic::Ordering::Relaxed);
    let action = std::env::var("GIT_REFLOG_ACTION").unwrap_or_default();
    for v in ["GIT_DIR", "GIT_INDEX_FILE", "GIT_WORK_TREE", "GIT_PREFIX", "GIT_REFLOG_ACTION"] { std::env::remove_var(v); }
    let rev = |dir: &Path, r: &str| -> String { let o = git(dir, &["rev-parse", "-q", "--verify", &format!("{r}^{{commit}}")]); if o.status.success() { String::from_utf8_lossy(&o.stdout).trim().to_string() } else { String::new() } };
    let here = std::env::current_dir().unwrap_or_default();
    let root = root();
    let Ok(t) = std::fs::read_to_string(cfg_path(&root)) else { return };
    let Ok(mut c) = serde_json::from_str::<Config>(&t) else { return };
    // The gate committed in .gitta.toml on the target branch wins, as in every other command.
    if let Some(g) = team_setting(&root, "gate") { c.gate = Some(g); }
    if c.watch { refresh(&root, &mut c); }
    apply_settings(&root, &c.security, &c.include);
    let target = c.main.trim_start_matches("refs/heads/").to_string();
    let branch = String::from_utf8_lossy(&git(&here, &["symbolic-ref", "-q", "HEAD"]).stdout).trim().to_string();
    let stop = |e: String| -> ! { say(&e); std::process::exit(1) };
    // gitta's own git commands below (moving a branch back, cancelling a merge) need no second check.
    std::env::set_var("GITTA_LANDING", "1");
    match name {
        "pre-merge-commit" => {
            if branch != c.main { return; }
            // Git writes MERGE_HEAD only after this hook; the command line is in GIT_REFLOG_ACTION ("merge <branch>").
            let mut incoming = rev(&here, "MERGE_HEAD");
            if incoming.is_empty() { if let Some(b) = action.strip_prefix("merge ").and_then(|x| x.split_whitespace().last()) { incoming = rev(&here, b); } }
            if incoming.is_empty() { incoming = rev(&here, "FETCH_HEAD"); }
            if incoming.is_empty() { return; }
            // The merge result as git wrote it to the index, as a commit gitta can check out on its own.
            let at = soft(&here, &["write-tree"]).and_then(|t| soft(&root, &["commit-tree", &t, "-p", &rev(&here, "HEAD"), "-p", &incoming, "-m", "gitta check"])).unwrap_or_default();
            match check(&root, &c, &at, &target, "HEAD", &incoming, "git merge") {
                Ok(()) => { mark_verified(&root, &incoming); mark_verified(&root, &git_ok(&here, &["write-tree"])); }
                Err(e) => {
                    // Cancel the attempt for the person once git has finished writing it, but only when nothing
                    // but the merge itself is uncommitted, so no work of theirs can be lost.
                    let merged: BTreeSet<String> = git_ok(&here, &["diff", "--name-only", "HEAD", &incoming]).lines().map(String::from).collect();
                    let dirty: Vec<String> = git_ok(&here, &["status", "--porcelain", "--untracked-files=no"]).lines().map(|l| l[3.min(l.len())..].to_string()).collect();
                    let safe = dirty.iter().all(|p| merged.contains(p));
                    let parent = parent_pid();
                    let spawned = safe && Command::new(std::env::current_exe().unwrap()).args(["git-hook", "abort-after", &parent, &incoming, &rev(&here, "HEAD")])
                        .current_dir(&here).env_remove("GITTA_LANDING").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().is_ok();
                    let next = if spawned { format!("{target} unchanged, merge cancelled (ignore git's note)") }
                        else { "run `git merge --abort` first".to_string() };
                    stop(fin(&e, &next))
                }
            }
        }
        "post-merge" => {
            if branch != c.main { return; }
            let head = rev(&here, "HEAD");
            let Some(parents) = parent_count(&git_ok(&here, &["rev-list", "--parents", "-n", "1", "HEAD"])) else {
                // rev-list named nothing (unexpected: unborn/detached/odd state). `count - 1` would wrap to a huge
                // number in release and be read as a merge commit -- skipping the post-merge check. Bail instead.
                log(&root, "warning", "", &format!("post-merge on {target}: `git rev-list --parents` returned nothing; gitta is not recording this commit as checked."));
                return;
            };
            if parents >= 2 {
                // A merge commit: pre-merge-commit checked it, unless it was skipped with --no-verify.
                let second = rev(&here, "HEAD^2");
                if !is_verified(&root, &second) {
                    audit::record(&root, "checks_skipped", "", &audit::actor(&root), serde_json::json!({"onto": target, "commit": head, "how": "git merge --no-verify"}));
                    log(&root, "warning", "", &format!("A merge into {target} skipped gitta's checks (git merge --no-verify). It is recorded in the audit trail."));
                }
                mark_verified(&root, &head);
                let (who, ag) = who_made(&root, &c, &second);
                if ag.is_some() { after_merge(&mut c, &who, &head); }
                precheck_agents_later(&root);
                return;
            }
            // A fast-forward: reference-transaction checked it before main moved. Git older than 2.28 has no
            // such hook, so there the code is checked now and main steps back if it fails.
            let orig = rev(&here, "ORIG_HEAD");
            if has_ref_transaction() {
                if is_verified(&root, &head) { let (who, ag) = who_made(&root, &c, &head); if ag.is_some() { after_merge(&mut c, &who, &head); } }
                if orig != head { precheck_agents_later(&root); }
                return;
            }
            if orig.is_empty() || orig == head { return; }
            if is_verified(&root, &head) { let (who, ag) = who_made(&root, &c, &head); if ag.is_some() { after_merge(&mut c, &who, &head); } return; }
            match check(&root, &c, &head, &target, &orig, &head, "git merge") {
                Ok(()) => { mark_verified(&root, &head); let (who, ag) = who_made(&root, &c, &head); if ag.is_some() { after_merge(&mut c, &who, &head); } }
                Err(e) => {
                    // Another git command (a second agent's merge) can hold the index for a moment: try again briefly.
                    let mut back = git(&here, &["reset", "-q", "--keep", &orig]);
                    for _ in 0..20 {
                        if back.status.success() || rev(&here, "HEAD") != head { break; }
                        std::thread::sleep(std::time::Duration::from_millis(250));
                        back = git(&here, &["reset", "-q", "--keep", &orig]);
                    }
                    if back.status.success() { stop(fin(&e, &format!("{target} moved back"))) }
                    let why = String::from_utf8_lossy(&back.stderr).lines().find(|l| !l.trim().is_empty()).map(|l| l.trim().trim_start_matches("error: ").to_string()).unwrap_or_default();
                    stop(fin(&e, &format!("undo it with `git reset --keep {}` ({why})", &orig[..orig.len().min(10)])))
                }
            }
        }
        "abort-after" => {
            // Wait for the git process that ran the hook, then undo its paused merge if nothing else moved.
            let (pid, incoming, head) = (args.get(1).cloned().unwrap_or_default(), args.get(2).cloned().unwrap_or_default(), args.get(3).cloned().unwrap_or_default());
            for _ in 0..3000 {
                if !Command::new("kill").args(["-0", &pid]).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false) { break; }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            if rev(&here, "MERGE_HEAD") == incoming && rev(&here, "HEAD") == head && git(&here, &["merge", "--abort"]).status.success() {
                log(&root, "note", "", &format!("gitta cancelled the stopped merge attempt, so {target} is as it was."));
            }
        }
        "reference-transaction" => {
            // Every other way the target moves: cherry-pick, rebase, merge --squash and commit, am, reset, update-ref.
            for l in input.lines() {
                let f: Vec<&str> = l.split_whitespace().collect();
                if f.len() < 3 || f[2] != c.main { continue; }
                let new = f[1];
                if new.chars().all(|x| x == '0') { continue; }
                // Git passes zeros when the caller gave no old value (git update-ref without one): the ref still
                // holds it while the transaction is prepared. A ref that does not exist yet is being created.
                let old = if f[0].chars().all(|x| x == '0') { rev(&root, &c.main) } else { f[0].to_string() };
                if old.is_empty() { continue; }
                let old = old.as_str();
                let tree = |r: &str| git_ok(&root, &["rev-parse", &format!("{r}^{{tree}}")]);
                let nt = tree(new);
                if nt == tree(old) || is_verified(&root, new) || is_verified(&root, &nt) { continue; }
                // A plain commit on the target is the person's own work; a squashed merge is not, and neither is a
                // commit from an agent session. The git command that runs this hook is the parent process.
                let gd = git_ok(&here, &["rev-parse", "--path-format=absolute", "--git-dir"]);
                let squash = Path::new(&gd).join("SQUASH_MSG").exists();
                // `git commit` that finishes a merge stopped by a conflict: pre-merge-commit never ran for it.
                let concludes = Path::new(&gd).join("MERGE_HEAD").exists();
                let (sub, cmdline) = git_cmdline();
                if sub == "commit" && !squash && !concludes && !in_agent() { continue; }
                // git merge and git pull: pre-merge-commit checks a merge commit (post-merge records a skipped
                // one). A fast-forward is checked here, before main moves.
                let merging = matches!(sub.as_str(), "merge" | "pull");
                if merging && git_ok(&root, &["rev-list", "--parents", "-n", "1", new]).split_whitespace().count() > 2 { continue; }
                if merging && cmdline.split_whitespace().any(|w| w == "--no-verify") {
                    audit::record(&root, "checks_skipped", "", &audit::actor(&root), serde_json::json!({"onto": target, "commit": new, "how": format!("git {sub} --no-verify")}));
                    log(&root, "warning", "", &format!("A merge into {target} skipped gitta's checks (git {sub} --no-verify). It is recorded in the audit trail."));
                    continue;
                }
                if std::env::var_os("GITTA_SKIP").is_some() {
                    audit::record(&root, "checks_skipped", "", &audit::actor(&root), serde_json::json!({"onto": target, "commit": new, "how": format!("GITTA_SKIP (git {sub})")}));
                    log(&root, "warning", "", &format!("{target} moved without gitta's checks (GITTA_SKIP). It is recorded in the audit trail."));
                    continue;
                }
                let how = if squash { "git merge --squash and commit".to_string() } else if concludes && sub == "commit" { "git merge, finished with git commit".to_string() } else if !sub.is_empty() { format!("git {sub}") } else { format!("git {}", action.split([' ', ':']).next().unwrap_or("update-ref")) };
                // A fast-forward of the checked-out main has already written the new files here (git updates the
                // working tree before the branch), so the tests run in place and a refusal puts the files back.
                let ff_here = merging && branch == c.main && git(&root, &["merge-base", "--is-ancestor", old, new]).status.success()
                    && git_ok(&here, &["write-tree"]) == nt;
                let src = source_of(&root, Path::new(&gd), &sub, new);
                let r = check_from(&root, &c, new, &target, old, new, &how, &src);
                match r {
                    Ok(()) => { mark_verified(&root, new); mark_verified(&root, &nt); }
                    Err(e) => {
                        if ff_here { let _ = git(&here, &["read-tree", "-m", "-u", &nt, &tree(old)]); }
                        stop(fin(&e, &format!("{target} did not move")))
                    }
                }
            }
        }
        "precheck-agents" => { precheck_agents(&root, &c); }
        "post-commit" => {
            // An agent committed on its own branch: test the code its merge would give now, while no lock is
            // held, so the merge itself only looks the result up. Silent; the merge reports as always.
            if branch.is_empty() || branch == c.main { return; }
            let gd = PathBuf::from(soft(&here, &["rev-parse", "--path-format=absolute", "--git-dir"]).unwrap_or_default());
            if ["rebase-merge", "rebase-apply", "sequencer", "CHERRY_PICK_HEAD", "MERGE_HEAD"].iter().any(|f| gd.join(f).exists()) { return; }
            if agent_at(&c, &here).is_none() { return; }
            let Some(gate) = c.gate.clone() else { return };
            precheck(&root, &c, &gate, &rev(&here, "HEAD"), &rev(&root, &c.main));
        }
        "post-rewrite" => {
            // Git older than 2.28: no reference-transaction hook, so a rewrite of the target is checked afterwards.
            if branch != c.main { return; }
            let (head, orig) = (rev(&here, "HEAD"), rev(&here, "ORIG_HEAD"));
            if orig.is_empty() || head == orig || is_verified(&root, &head) { return; }
            if let Err(e) = check(&root, &c, &head, &target, &orig, &head, "git rebase") {
                say(&fin(&e, "git older than 2.28 cannot be stopped here, so undo it with `git reset --keep ORIG_HEAD`"));
            } else { mark_verified(&root, &head); }
        }
        "pre-push" => {
            for l in input.lines() {
                let f: Vec<&str> = l.split_whitespace().collect();
                if f.len() < 4 || f[2] != c.main || f[1].chars().all(|x| x == '0') { continue; }
                let (local, remote) = (f[1], f[3]);
                if in_agent() {
                    let (who, _) = who_made(&root, &c, local);
                    match crate::policy::load(&root, &c.main).and_then(|r| r.check_agent_push(&who, c.main.trim_start_matches("refs/heads/"))) {
                        Ok(()) => {}
                        Err(e) => { crate::policy_refused(&root, &who, std::slice::from_ref(&e)); stop(fin(&e, "nothing was pushed")) }
                    }
                }
                if is_verified(&root, local) { continue; }
                let onto = if remote.chars().all(|x| x == '0') || git(&root, &["cat-file", "-e", remote]).status.code() != Some(0) { c.main.clone() } else { remote.to_string() };
                if git_ok(&root, &["merge-base", &onto, local]) == local { continue; }
                let r = check(&root, &c, local, &target, &onto, local, "git push");
                match r { Ok(()) => mark_verified(&root, local), Err(e) => stop(fin(&e, "nothing was pushed")) }
            }
        }
        _ => {}
    }
}

/// A git command's output, or None when it fails (no stop: these lookups are optional).
fn soft(dir: &Path, args: &[&str]) -> Option<String> {
    let o = git(dir, args);
    let t = String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").trim().to_string();
    (o.status.success() && !t.is_empty()).then_some(t)
}

fn gate_cache(root: &Path, tree: &str) -> PathBuf { root.join(".gitta/gate-cache").join(tree) }

/// A short FNV-1a hex digest. Used for change detection, not security.
fn fnv_hex(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() { h ^= b as u64; h = h.wrapping_mul(0x100000001b3); }
    format!("{h:016x}")
}

/// A digest of a file's bytes, or a definite marker when it is absent. `None` means "present but could not be
/// read" -- an uncertain state the caller treats as unreliable.
fn file_sig(p: &Path) -> Option<String> {
    match std::fs::read(p) { Ok(b) => Some(fnv_hex(&String::from_utf8_lossy(&b))), Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some("absent".into()), Err(_) => None }
}

/// The installed Python packages under a project's virtualenv, by the version-bearing dist-info/egg-info dir
/// names. "absent" when there is no venv. This reflects what is actually installed, which can drift from a
/// committed lockfile.
fn venv_sig(root: &Path) -> String {
    let mut names: Vec<String> = vec![];
    for base in [root.join(".venv"), root.join("venv")] {
        if let Ok(libs) = std::fs::read_dir(base.join("lib")) {
            for py in libs.flatten() {
                if let Ok(pk) = std::fs::read_dir(py.path().join("site-packages")) {
                    for e in pk.flatten() {
                        if let Some(n) = e.file_name().to_str() { if n.ends_with(".dist-info") || n.ends_with(".egg-info") { names.push(n.to_string()); } }
                    }
                }
            }
        }
    }
    if names.is_empty() { return "absent".into(); }
    names.sort(); fnv_hex(&names.join("\n"))
}

/// A fingerprint of the gate's inputs that are NOT in the git tree, and whether it is reliable. The candidate
/// tree already covers every tracked file, committed lockfiles included; this covers what the tree cannot: the
/// toolchains the gate uses (detected from the command), the installed dependencies in the linked dep dirs
/// (which can drift from a committed lockfile), and the environment variables the project names in `gate_env`.
///
/// `reliable` is false when an input cannot be read with confidence -- a toolchain the gate names but whose
/// version will not run, or an installed-dependency marker that is present but unreadable. The caller then
/// bypasses reuse (runs the gate, caches nothing) rather than assume equivalence. An *absent* dependency dir is
/// a definite state, not an unreliable one. Env var values are only hashed, never returned or stored; an unset
/// variable is distinguished from one set to the empty string.
fn gate_fingerprint(root: &Path, c: &Config, gate: &str) -> (String, bool) {
    let mut parts: Vec<String> = vec!["gate-fp-v1".into()];
    let mut reliable = true;
    let g = gate.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| g.contains(n));
    let ver = |cmd: &str, args: &[&str]| -> Option<String> {
        let o = Command::new(cmd).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).output().ok()?;
        let s = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        s.lines().next().map(|l| l.trim().to_string()).filter(|l| !l.is_empty())
    };
    let tool = |label: &str, cmd: &str, args: &[&str], parts: &mut Vec<String>, reliable: &mut bool| {
        match ver(cmd, args) { Some(v) => parts.push(format!("{label}={v}")), None => { parts.push(format!("{label}=unresolved")); *reliable = false; } }
    };
    if has(&["node", "npm", "npx", "pnpm", "yarn", "jest", "vitest", "tsc", "eslint", "ts-node"]) {
        tool("node", "node", &["--version"], &mut parts, &mut reliable);
        match file_sig(&root.join("node_modules/.package-lock.json")) { Some(s) => parts.push(format!("node_modules={s}")), None => { parts.push("node_modules=unreadable".into()); reliable = false; } }
    }
    if has(&["python", "pytest", "pip", "tox", "ruff", "mypy", "poetry"]) {
        tool("python", "python3", &["--version"], &mut parts, &mut reliable);
        parts.push(format!("site-packages={}", venv_sig(root)));
    }
    if has(&["cargo", "rustc", "clippy"]) { tool("rust", "rustc", &["--version"], &mut parts, &mut reliable); }
    if g.split(|c: char| c.is_whitespace()).any(|w| w == "go") { tool("go", "go", &["version"], &mut parts, &mut reliable); }
    if has(&["java", "mvn", "maven", "gradle"]) { tool("java", "java", &["-version"], &mut parts, &mut reliable); }
    if has(&["deno"]) { tool("deno", "deno", &["--version"], &mut parts, &mut reliable); }
    if has(&["bun"]) { tool("bun", "bun", &["--version"], &mut parts, &mut reliable); }
    for name in &c.gate_env {
        match std::env::var(name) { Ok(v) => parts.push(format!("env:{name}=set:{}", fnv_hex(&v))), Err(_) => parts.push(format!("env:{name}=unset")) }
    }
    (fnv_hex(&parts.join("\n")), reliable)
}

/// The kept result of `gate` on `tree`, when that exact test command ran on that exact code and the same
/// out-of-tree environment (`env`, the gate fingerprint) before.
pub(crate) fn cached_gate(root: &Path, gate: &str, tree: &str, env: &str) -> Option<(bool, String, Option<i32>)> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(gate_cache(root, tree)).ok()?).ok()?;
    if v["gate"].as_str() != Some(gate) { return None; }
    if v["env"].as_str() != Some(env) { return None; }
    Some((v["ok"].as_bool()?, v["out"].as_str().unwrap_or("").to_string(), v["code"].as_i64().map(|x| x as i32)))
}

pub(crate) fn save_gate(root: &Path, gate: &str, tree: &str, env: &str, ok: bool, out: &str, code: Option<i32>) {
    let p = gate_cache(root, tree);
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    let out: String = out.chars().take(200_000).collect();
    let tmp = p.with_extension(format!("tmp{}", std::process::id()));
    if std::fs::write(&tmp, serde_json::json!({"gate": gate, "env": env, "ok": ok, "code": code, "out": out}).to_string()).is_ok() { let _ = std::fs::rename(&tmp, &p); }
}

// ---- Failure packages (item B): a failed landing turned into a reproducible, handoff-ready task. ----
// One JSON format under .gitta/failures/<id>.json, shared by the CLI (`gitta failure ...`) and the agent
// tool (gitta_failures). Immutable revisions, the gate, the out-of-tree fingerprint (a hash, never raw env
// values), bounded diagnostics and a rerun command. "rerun_available" is set when instructions exist;
// "reproduced" is set only after a rerun confirms the same failure against a matching fingerprint.

fn failures_dir(root: &Path) -> PathBuf { root.join(".gitta/failures") }
fn now_secs() -> f64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0) }
fn short(sha: &str) -> &str { &sha[..sha.len().min(12)] }

/// Record (or refresh) the failure package for a gate failure on the merge path. Keyed by the candidate tree,
/// so one failing candidate is one package. Prior rerun attempts are preserved.
pub(crate) fn write_failure_package(root: &Path, c: &Config, base: &str, incoming: &str, candidate_commit: &str, candidate_tree: &str, failing: &[String], out: &str) {
    let Some(gate) = c.gate.as_deref() else { return };
    if candidate_tree.is_empty() { return; }
    let id = short(candidate_tree);
    let (fp, reliable) = gate_fingerprint(root, c, gate);
    let tail: Vec<&str> = out.lines().rev().take(40).collect::<Vec<_>>().into_iter().rev().collect();
    let dir = failures_dir(root); let _ = std::fs::create_dir_all(&dir);
    let p = dir.join(format!("{id}.json"));
    let attempts = std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()).and_then(|v| v["attempts"].as_array().cloned()).unwrap_or_default();
    let pkg = serde_json::json!({
        "id": id, "recorded_at": now_secs(), "status": "recorded", "reproduced": false, "rerun_available": true,
        "base": base, "incoming": incoming, "candidate_commit": candidate_commit, "candidate_tree": candidate_tree,
        "gate": gate, "fingerprint": fp, "fingerprint_reliable": reliable,
        "failing": failing.iter().take(10).collect::<Vec<_>>(), "errors": first_errors(out, 8), "output": tail.join("\n"),
        "rerun": format!("gitta failure rerun {id}"), "attempts": attempts });
    let tmp = p.with_extension(format!("tmp{}", std::process::id()));
    if std::fs::write(&tmp, serde_json::to_string_pretty(&pkg).unwrap()).is_ok() { let _ = std::fs::rename(&tmp, &p); }
}

fn failure_list(root: &Path) -> Vec<serde_json::Value> {
    let mut v: Vec<serde_json::Value> = std::fs::read_dir(failures_dir(root)).into_iter().flatten().flatten()
        .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok()).filter_map(|s| serde_json::from_str(&s).ok()).collect();
    v.sort_by(|a, b| b["recorded_at"].as_f64().unwrap_or(0.0).total_cmp(&a["recorded_at"].as_f64().unwrap_or(0.0)));
    v
}
fn failure_get(root: &Path, id: &str) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(failures_dir(root).join(format!("{}.json", short(id)))).ok()?).ok()
}

/// Re-run the recorded candidate's gate. Only claims "reproduced" when the out-of-tree environment still
/// matches AND the gate fails the same way; a changed environment is recorded as a mismatch, not a reproduction.
fn failure_rerun(root: &Path, c: &Config, id: &str) -> Result<String, String> {
    let mut pkg = failure_get(root, id).ok_or_else(|| format!("no failure package {id}; see `gitta failure list`"))?;
    let gate = c.gate.clone().ok_or("no gate is set, so nothing can be re-run")?;
    let commit = pkg["candidate_commit"].as_str().unwrap_or_default().to_string();
    let recorded_fp = pkg["fingerprint"].as_str().unwrap_or_default().to_string();
    let (now_fp, reliable) = gate_fingerprint(root, c, &gate);
    let mut attempts = pkg["attempts"].as_array().cloned().unwrap_or_default();
    let (status, outcome) = if now_fp != recorded_fp {
        ("environment mismatch".to_string(), format!("the environment differs from when this failure was recorded, so this is not a clean reproduction (recorded fingerprint {}, now {}). Re-create the environment, or treat it as a new run.", short(&recorded_fp), short(&now_fp)))
    } else if !reliable {
        ("environment unverified".to_string(), "the environment could not be fingerprinted reliably, so a reproduction cannot be confirmed.".to_string())
    } else {
        let w = Wt::open(root, "gate", &commit, &c.link);
        let (ok, _out, _code, timed) = run_gate(&gate, &w.path, None, gate_timeout(c));
        w.release();
        if timed { ("could not run".into(), "the gate was stopped at its time limit before a result; not confirmed.".into()) }
        else if ok { pkg["reproduced"] = serde_json::json!(false); ("not reproduced".into(), "the gate passed on the recorded candidate; the failure did not reproduce.".into()) }
        else { pkg["reproduced"] = serde_json::json!(true); ("reproduced".into(), "the gate failed again on the recorded candidate; the failure is confirmed.".into()) }
    };
    attempts.push(serde_json::json!({"at": now_secs(), "result": status, "fingerprint": short(&now_fp)}));
    pkg["attempts"] = serde_json::json!(attempts);
    pkg["status"] = serde_json::json!(status);
    let p = failures_dir(root).join(format!("{}.json", short(id)));
    let _ = std::fs::write(&p, serde_json::to_string_pretty(&pkg).unwrap());
    Ok(outcome)
}

/// `gitta failure [list|show <id>|rerun <id>]`.
pub(crate) fn run_failure(args: &[String]) {
    let root = root();
    let c = load(&root);
    match args.first().map(String::as_str) {
        None | Some("list") => {
            let v = failure_list(&root);
            if v.is_empty() { println!("gitta: no recorded integration failures."); return; }
            for f in &v {
                let first = f["failing"].as_array().and_then(|a| a.first()).and_then(|x| x.as_str()).unwrap_or("");
                println!("{}  {:20}  {}", f["id"].as_str().unwrap_or(""), f["status"].as_str().unwrap_or(""), first);
            }
        }
        Some("show") => { match args.get(1).and_then(|id| failure_get(&root, id)) { Some(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap()), None => die("usage: gitta failure show <id>") } }
        Some("rerun") => { let id = args.get(1).cloned().unwrap_or_else(|| die("usage: gitta failure rerun <id>")); match failure_rerun(&root, &c, &id) { Ok(m) => println!("gitta: {m}"), Err(e) => die(&e) } }
        Some(x) => die(&format!("unknown: gitta failure {x} (try list, show <id>, rerun <id>)")),
    }
}

/// The failure packages as JSON, for the agent tool. `id` None lists them; Some shows one.
pub(crate) fn failures_json(root: &Path, id: Option<&str>) -> String {
    match id { Some(id) => failure_get(root, id).map(|v| serde_json::to_string_pretty(&v).unwrap()).unwrap_or_else(|| format!("no failure package {id}")),
               None => serde_json::to_string_pretty(&failure_list(root)).unwrap() }
}

fn gate_running(root: &Path, tree: &str) -> PathBuf { root.join(".gitta/gate-cache").join(format!("{tree}.running")) }

fn pid_alive(pid: &str) -> bool {
    !pid.trim().is_empty() && Command::new("kill").args(["-0", pid.trim()]).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
}

/// Wait while another gitta process tests this exact tree (a precheck started at commit time or after main moved).
fn wait_running(root: &Path, tree: &str) {
    let p = gate_running(root, tree);
    while let Ok(pid) = std::fs::read_to_string(&p) {
        if !pid_alive(&pid) { let _ = std::fs::remove_file(&p); break; }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// The gate's result on `tree` (checked out from `at`): kept from an earlier run of the same test command on the
/// same code, the run another gitta process has going for it, or a new run in a checkout of gitta's own.
/// The last value says whether the result was kept from before. None: the code could not be checked out.
fn gate_result(root: &Path, c: &Config, gate: &str, tree: &str, at: &str) -> Option<(bool, String, Option<i32>, bool)> {
    if tree.is_empty() { return None; }
    // The out-of-tree environment fingerprint. When it cannot be read reliably, reuse is bypassed entirely:
    // the gate runs and nothing is cached, rather than trust or write a result keyed on an uncertain identity.
    let (fp, reliable) = gate_fingerprint(root, c, gate);
    if reliable {
        if let Some((ok, out, code)) = cached_gate(root, gate, tree, &fp) { return Some((ok, out, code, true)); }
        wait_running(root, tree);
        if let Some((ok, out, code)) = cached_gate(root, gate, tree, &fp) { return Some((ok, out, code, true)); }
    }
    let run = gate_running(root, tree);
    let _ = std::fs::create_dir_all(run.parent().unwrap());
    let _ = std::fs::write(&run, std::process::id().to_string());
    // Bound how many gates run at once across all gitta processes (land, merge-path, precheck share the pool).
    let _slot = acquire_gate_slot(root, gate_cap(c));
    let w = Wt::open(root, "gate", at, &c.link);
    let (ok, out, code, timed_out) = run_gate(gate, &w.path, _slot.path(), gate_timeout(c));
    w.release();
    drop(_slot);
    // Cache only a trustworthy result keyed on a reliable fingerprint: never a run stopped at the time limit,
    // never a no-test pass, never a missing-tool run (already excluded), and never when the environment could
    // not be fingerprinted.
    if reliable && !timed_out && missing_tool(&out, code).is_none() && !(ok && zero_tests(&out).is_some()) { save_gate(root, gate, tree, &fp, ok, &out, code); }
    let _ = std::fs::remove_file(&run);
    Some((ok, out, code, false))
}

/// After main moved: test what each agent's committed work gives on the new main, in the background, so the agent's
/// next merge finds the result ready and git holds main's lock for a moment only. One run at a time per repository;
/// a move during a run asks for one more round.
pub(crate) fn precheck_agents_later(root: &Path) {
    if let Ok(exe) = std::env::current_exe() {
        let _ = Command::new(exe).args(["git-hook", "precheck-agents"]).current_dir(root).env_remove("GITTA_LANDING")
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    }
}

fn precheck_agents(root: &Path, c: &Config) {
    let Some(gate) = c.gate.clone() else { return };
    let (lock, again) = (root.join(".gitta/precheck.lock"), root.join(".gitta/precheck.again"));
    if let Ok(pid) = std::fs::read_to_string(&lock) { if !pid_alive(&pid) { let _ = std::fs::remove_file(&lock); } }
    if std::fs::OpenOptions::new().write(true).create_new(true).open(&lock).map(|mut f| { let _ = write!(f, "{}", std::process::id()); }).is_err() {
        let _ = std::fs::write(&again, "1");
        return;
    }
    for _ in 0..5 {
        let _ = std::fs::remove_file(&again);
        let main = soft(root, &["rev-parse", &c.main]).unwrap_or_default();
        for a in &c.agents {
            let Some(tip) = soft(Path::new(&a.path), &["rev-parse", "HEAD"]) else { continue };
            if git(root, &["merge-base", "--is-ancestor", &tip, &main]).status.success() { continue; }
            precheck(root, c, &gate, &tip, &main);
        }
        if !again.exists() { break; }
    }
    let _ = std::fs::remove_file(&lock);
}

/// Test the code that merging `tip` into `main` gives (the tip itself for a fast-forward) and keep the result.
fn precheck(root: &Path, c: &Config, gate: &str, tip: &str, main: &str) {
    if tip.is_empty() || main.is_empty() || tip == main { return; }
    let (tree, commit) = if git(root, &["merge-base", "--is-ancestor", main, tip]).status.success() {
        (soft(root, &["rev-parse", &format!("{tip}^{{tree}}")]), Some(tip.to_string()))
    } else {
        // A merge that would conflict is left to the merge, which says so.
        let Some(t) = soft(root, &["merge-tree", "--write-tree", main, tip]) else { return };
        let cm = soft(root, &["commit-tree", &t, "-p", main, "-p", tip, "-m", "gitta precheck"]);
        (Some(t), cm)
    };
    let (Some(tree), Some(commit)) = (tree, commit) else { return };
    let _ = gate_result(root, c, gate, &tree, &commit);
}

/// The process id of the parent process (git, when a hook runs). Windows has no stable call for it, so it is 0 there.
fn parent_pid() -> String {
    #[cfg(unix)] { std::os::unix::process::parent_id().to_string() }
    #[cfg(not(unix))] { "0".to_string() }
}

#[cfg(test)]
mod hook_guard_tests {
    use super::*;

    // `git rev-list --parents -n1` prints the commit id then its parent ids. Empty output must be None (the caller
    // bails), never 0 which would underflow `count - 1` to usize::MAX in release and read as a merge commit.
    #[test]
    fn parent_count_handles_empty_and_normal() {
        assert_eq!(parent_count(""), None, "empty output must bail, not underflow");
        assert_eq!(parent_count("   "), None);
        assert_eq!(parent_count("abc123"), Some(0), "a root/normal commit: id only, no parents");
        assert_eq!(parent_count("abc p1"), Some(1), "an ordinary commit: one parent");
        assert_eq!(parent_count("abc p1 p2"), Some(2), "a merge commit: two parents");
        assert_eq!(parent_count("abc p1 p2 p3"), Some(3), "an octopus merge");
    }

    // git named changed files but every blob read empty on both sides -> a read regression that must fail loud,
    // not fall through to the approve/no-check path. Populated snapshots (a real no-op merge) do not trip it, and
    // an empty diff (no changed files) is not vacuous.
    #[test]
    fn vacuous_read_only_on_empty_both_sides_with_changes() {
        let empty = Snapshot::new();
        let mut full = Snapshot::new();
        full.insert("a.txt".into(), b"x".to_vec());
        assert!(vacuous_read(2, &empty, &empty), "changes named but nothing read -> fail loud");
        assert!(!vacuous_read(0, &empty, &empty), "no changed files is an ordinary empty diff");
        assert!(!vacuous_read(2, &full, &empty), "one side has content -> a real change, not vacuous");
        assert!(!vacuous_read(2, &full, &full), "a populated no-op merge is not vacuous");
    }
}

#[cfg(test)]
mod gate_fingerprint_tests {
    use super::*;

    fn cfg(root: &Path, gate_env: &[&str]) -> Config {
        serde_json::from_value(serde_json::json!({
            "root": root.to_string_lossy(), "main": "refs/heads/main", "agents": [],
            "gate": null, "link": [], "auto_sync": true,
            "gate_env": gate_env.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        })).unwrap()
    }
    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("gitta-fp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d
    }

    // Reuse when inputs are unchanged: the fingerprint is stable across calls.
    #[test]
    fn fingerprint_is_stable_when_nothing_changes() {
        let root = tmp("stable"); let c = cfg(&root, &[]);
        let (a, ra) = gate_fingerprint(&root, &c, "true");
        let (b, rb) = gate_fingerprint(&root, &c, "true");
        assert_eq!(a, b); assert_eq!(ra, rb); assert!(ra, "a gate naming no toolchain is reliable");
        let _ = std::fs::remove_dir_all(&root);
    }

    // An unset variable, an empty one and a set one are three distinct states.
    #[test]
    fn fingerprint_distinguishes_unset_empty_and_set_env() {
        let root = tmp("env"); let name = format!("GITTA_FP_TEST_{}", std::process::id()); let c = cfg(&root, &[&name]);
        std::env::remove_var(&name);
        let (unset, _) = gate_fingerprint(&root, &c, "true");
        std::env::set_var(&name, "");
        let (empty, _) = gate_fingerprint(&root, &c, "true");
        std::env::set_var(&name, "value");
        let (set, _) = gate_fingerprint(&root, &c, "true");
        std::env::remove_var(&name);
        assert_ne!(unset, empty, "unset must differ from empty");
        assert_ne!(empty, set, "empty must differ from a value");
        assert_ne!(unset, set, "unset must differ from a value");
        let _ = std::fs::remove_dir_all(&root);
    }

    // Installed node dependencies (npm's recorded state) change the fingerprint even when the tree does not.
    #[test]
    fn fingerprint_changes_with_installed_node_deps() {
        let root = tmp("node"); let c = cfg(&root, &[]);
        std::fs::create_dir_all(root.join("node_modules")).unwrap();
        std::fs::write(root.join("node_modules/.package-lock.json"), "{\"v\":1}").unwrap();
        let (a, _) = gate_fingerprint(&root, &c, "npm test");
        std::fs::write(root.join("node_modules/.package-lock.json"), "{\"v\":2}").unwrap();
        let (b, _) = gate_fingerprint(&root, &c, "npm test");
        assert_ne!(a, b, "a change in installed node deps must change the fingerprint");
        let _ = std::fs::remove_dir_all(&root);
    }

    // An input that is present but cannot be read is unreliable, so reuse is bypassed.
    #[test]
    fn fingerprint_unreliable_when_a_marker_is_unreadable() {
        let root = tmp("unreadable"); let c = cfg(&root, &[]);
        // A directory where the marker file is expected: reading it as a file fails with a non-NotFound error.
        std::fs::create_dir_all(root.join("node_modules/.package-lock.json")).unwrap();
        let (_, reliable) = gate_fingerprint(&root, &c, "npm test");
        assert!(!reliable, "an unreadable installed-deps marker must make the fingerprint unreliable");
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(all(test, unix))]
mod failure_package_tests {
    use super::*;
    #[test]
    fn records_reruns_and_distinguishes_environment_mismatch() {
        let root = std::env::temp_dir().join(format!("gitta-fail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root); std::fs::create_dir_all(&root).unwrap();
        let g = |args: &[&str]| Command::new("git").arg("-C").arg(&root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t")
            .args(args).output().unwrap();
        g(&["init", "-q", "-b", "main"]);
        std::fs::write(root.join("a.txt"), "x").unwrap();
        g(&["add", "-A"]); g(&["commit", "-qm", "base"]);
        let head = String::from_utf8_lossy(&g(&["rev-parse", "HEAD"]).stdout).trim().to_string();
        let tree = String::from_utf8_lossy(&g(&["rev-parse", "HEAD^{tree}"]).stdout).trim().to_string();
        let envname = format!("GITTA_FAIL_ENV_{}", std::process::id());
        let c: Config = serde_json::from_value(serde_json::json!({
            "root": root.to_string_lossy(), "main": "refs/heads/main", "agents": [],
            "gate": "false", "link": [], "auto_sync": true, "gate_env": [envname.clone()] })).unwrap();
        std::env::remove_var(&envname);
        write_failure_package(&root, &c, &head, &head, &head, &tree, &["t_x".into()], "boom\n");
        let id = short(&tree).to_string();
        let v = failure_get(&root, &id).expect("package written");
        assert_eq!(v["rerun_available"], serde_json::json!(true));
        assert_eq!(v["reproduced"], serde_json::json!(false));
        assert!(!failure_list(&root).is_empty());
        // Same environment: the gate ("false") fails again, so the failure is confirmed reproduced.
        let msg = failure_rerun(&root, &c, &id).unwrap();
        assert!(msg.contains("confirmed"), "{msg}");
        assert_eq!(failure_get(&root, &id).unwrap()["reproduced"], serde_json::json!(true));
        // Changed environment: reported as a mismatch, not a reproduction.
        std::env::set_var(&envname, "changed");
        let msg2 = failure_rerun(&root, &c, &id).unwrap();
        std::env::remove_var(&envname);
        assert!(msg2.contains("differs"), "{msg2}");
        assert_eq!(failure_get(&root, &id).unwrap()["status"], serde_json::json!("environment mismatch"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
