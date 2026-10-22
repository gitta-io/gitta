//! `gitta status` on a repository the size of a busy team: 64 agent worktrees, many of them hundreds of commits
//! behind main. It must stay fast and print its first line at once. The budget is generous for slow CI machines,
//! and a relative check compares it with a status of 4 agents on the same machine, so a return of per-pair or
//! per-agent work that grows much faster than the number of agents fails here even on a fast machine.
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

const MODULES: usize = 40;
const COMMITS: usize = 330;

fn module(i: usize, rev: usize) -> String {
    let next = (i + 1) % MODULES;
    format!("from shop.m{next} import f{next}\n\n\ndef f{i}(x):\n    return x + {rev}\n\n\ndef g{i}(x):\n    return f{next}(x) * 2\n")
}

/// A repository with a long main history (one fast-import), and `agents` worktrees started at points spread over it.
fn build(dir: &Path, agents: usize) -> PathBuf {
    let _ = std::fs::remove_dir_all(dir);
    let (home, repo) = (dir.join("home"), dir.join("repo"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    let mut s = String::new();
    for c in 0..COMMITS {
        let msg = format!("c{c}");
        s += &format!("commit refs/heads/main\nmark :{}\ncommitter t <t@t> {} +0000\ndata {}\n{msg}\n", c + 1, 1_700_000_000 + c, msg.len());
        if c > 0 { s += &format!("from :{c}\n"); }
        let files: Vec<usize> = if c == 0 { (0..MODULES).collect() } else { vec![c % MODULES] };
        for i in files {
            let body = module(i, c);
            s += &format!("M 100644 inline shop/m{i}.py\ndata {}\n{body}\n", body.len());
        }
        if c == 0 { s += "M 100644 inline shop/__init__.py\ndata 0\n\n"; }
    }
    let mut fi = Command::new("git").arg("-C").arg(&repo).args(["fast-import", "--quiet"]).stdin(Stdio::piped()).spawn().unwrap();
    fi.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
    assert!(fi.wait().unwrap().success());
    git(&repo, &["checkout", "-q", "-f", "main"]);
    let ids: Vec<String> = git(&repo, &["rev-list", "--reverse", "main"]).lines().map(String::from).collect();
    for k in 0..agents {
        // Spread over the history: some agents stand on main, the oldest are more than 300 commits behind.
        let at = &ids[COMMITS - 1 - (k * (COMMITS - 1)) / agents.max(2).saturating_sub(1).max(1)];
        let wt = dir.join(format!("wt{k}"));
        git(&repo, &["worktree", "add", "-q", "-b", &format!("agent{k}"), wt.to_str().unwrap(), at]);
        // Most agents edit a module; neighbours often edit the same one, so there are overlaps to find.
        if k % 3 != 2 {
            let i = (k / 2) % MODULES;
            let p = wt.join(format!("shop/m{i}.py"));
            let t = std::fs::read_to_string(&p).unwrap();
            std::fs::write(&p, t.replace("* 2", &format!("* {}", k + 3))).unwrap();
            std::fs::write(wt.join(format!("shop/new{k}.py")), format!("from shop.m{i} import f{i}\n\n\ndef h{k}():\n    return f{i}(1)\n")).unwrap();
        }
    }
    let o = gitta(dir, &repo, &["init", "--no-claude"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    repo
}

fn gitta(dir: &Path, cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_gitta")).env("GITTA_NO_NOTIFY", "1").env("GITTA_NO_PROMPT", "1").args(args).current_dir(cwd).env("HOME", dir.join("home")).output().unwrap()
}

/// Runs `gitta status`: the time to its first line, the total time, and the output.
fn status(dir: &Path, repo: &Path) -> (Duration, Duration, String) {
    let t = Instant::now();
    let mut ch = Command::new(env!("CARGO_BIN_EXE_gitta")).arg("status").env("GITTA_NO_NOTIFY", "1").current_dir(repo).env("HOME", dir.join("home"))
        .stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut out = BufReader::new(ch.stdout.take().unwrap());
    let mut first = String::new();
    out.read_line(&mut first).unwrap();
    let t_first = t.elapsed();
    let mut rest = String::new();
    std::io::Read::read_to_string(&mut out, &mut rest).unwrap();
    assert!(ch.wait().unwrap().success());
    (t_first, t.elapsed(), first + &rest)
}

#[test]
fn status_with_64_worktrees_stays_fast() {
    let base = std::env::temp_dir().join(format!("gitta-status-scale-{}", std::process::id()));
    let small_dir = base.join("small");
    let small = build(&small_dir, 4);
    let big_dir = base.join("big");
    let big = build(&big_dir, 64);

    // Baseline: the same repository shape with 4 agents. Best of two runs each, after one run that fills caches.
    let best = |d: &Path, r: &Path| { status(d, r); let a = status(d, r); let b = status(d, r); if a.1 <= b.1 { a } else { b } };
    let (_, t_small, _) = best(&small_dir, &small);

    // Cold: nothing cached yet.
    let _ = std::fs::remove_dir_all(big.join(".gitta/cache"));
    let (first_cold, t_cold, out) = status(&big_dir, &big);
    let (first_warm, t_warm, out_warm) = best(&big_dir, &big);
    eprintln!("status: 4 agents {t_small:?}; 64 agents cold {t_cold:?} (first line {first_cold:?}), warm {t_warm:?} (first line {first_warm:?})");

    // The results stay complete and the same with and without the cache.
    assert_eq!(out, out_warm, "a cached status must say what an uncached one says");
    assert_eq!(out.lines().filter(|l| l.starts_with("  agent") && (l.contains(" changed, ") || l.contains("no changes yet"))).count(), 64, "{out}");
    assert!(out.contains("main has 300+ newer commits"), "the far-behind agents show a capped count: {out}");
    assert!(out.lines().any(|l| l.contains("main has 5 newer commits")), "{out}");
    assert!(out.contains("both edited: shop/m0.py"), "neighbours editing one module are reported: {out}");

    // Generous absolute budgets (a loaded CI machine), and a relative one against the 4-agent baseline: 16 times the
    // agents may cost at most 25 times the time (pairs grow with the square, so n² work fails this).
    assert!(first_cold < Duration::from_secs(5), "first line after {first_cold:?}");
    assert!(t_cold < Duration::from_secs(90), "cold status took {t_cold:?}");
    assert!(t_warm < Duration::from_secs(30), "warm status took {t_warm:?}");
    assert!(t_warm.as_secs_f64() < (t_small.as_secs_f64() * 25.0).max(1.5),
        "warm status of 64 agents took {t_warm:?}, the 4-agent baseline {t_small:?}");
    let _ = std::fs::remove_dir_all(&base);
}
