//! Rules as a file: the `[policy]` section of the committed `.gitta.toml`, plus the rules a team server adds.
//!
//!   [policy]
//!   agents_push_to_main = "block"                       # agents may not push to main directly ("allow" by default)
//!   agents_never_change = ["infra/**", ".github/**"]    # agents never change these paths
//!   needs_approval = ["db/migrations/**", "auth/**"]    # agent changes here need a person's approval
//!
//! gitta enforces the rules in its git hooks and in the pull request check, and fails like any check, with one
//! plain line. It never asks. The rules come from the target branch's committed file, so only a reviewed commit
//! changes them. A team server can add rules (`GITTA_POLICY_FILE` on the server, same format); its rules are kept
//! in `.gitta/server-policy.toml` and are added to the repository's, never replaced by them.

use std::path::Path;

#[derive(Default, Debug, Clone, PartialEq)]
pub struct Policy {
    pub agents_push_to_main_blocked: bool,
    pub never_change: Vec<String>,
    pub needs_approval: Vec<String>,
    /// where each rule came from, for the message: ".gitta.toml" or "the team server"
    pub from_server: Vec<String>,
}

/// A TOML string list: `["a", 'b']`, possibly over several lines, or a single string.
fn list(v: &str) -> Vec<String> {
    let v = v.trim();
    let inner = v.strip_prefix('[').map(|x| x.trim_end().trim_end_matches(']')).unwrap_or(v);
    inner.split(',').map(|x| x.trim().trim_matches('"').trim_matches('\'').trim().to_string()).filter(|x| !x.is_empty()).collect()
}

/// Reads the `[policy]` section. Keys outside it are other settings and are ignored here.
pub fn parse(toml: &str) -> Result<Policy, String> {
    let mut p = Policy::default();
    let mut in_policy = false;
    let mut pending: Option<(String, String)> = None;
    for (n, raw) in toml.lines().enumerate() {
        let l = strip_comment(raw).trim().to_string();
        if let Some((k, mut acc)) = pending.take() {
            acc.push(' '); acc.push_str(&l);
            if l.contains(']') { set(&mut p, &k, &acc, n)?; } else { pending = Some((k, acc)); }
            continue;
        }
        if l.is_empty() { continue; }
        if l.starts_with('[') && !l.contains('=') { in_policy = l == "[policy]"; continue; }
        if !in_policy { continue; }
        let Some((k, v)) = l.split_once('=') else { return Err(format!("line {}: expected key = value", n + 1)) };
        let (k, v) = (k.trim().to_string(), v.trim().to_string());
        if v.starts_with('[') && !v.contains(']') { pending = Some((k, v)); continue; }
        set(&mut p, &k, &v, n)?;
    }
    if let Some((k, _)) = pending { return Err(format!("{k}: the list is not closed with ]")); }
    Ok(p)
}

fn strip_comment(l: &str) -> &str {
    let mut q: Option<char> = None;
    for (i, c) in l.char_indices() {
        match (c, q) { ('"' | '\'', None) => q = Some(c), (c2, Some(o)) if c2 == o => q = None, ('#', None) => return &l[..i], _ => {} }
    }
    l
}

fn set(p: &mut Policy, k: &str, v: &str, n: usize) -> Result<(), String> {
    match k {
        "agents_push_to_main" => {
            let v = v.trim_matches('"').trim_matches('\'');
            p.agents_push_to_main_blocked = match v { "block" | "false" => true, "allow" | "true" => false, _ => return Err(format!("line {}: agents_push_to_main is \"block\" or \"allow\", not {v}", n + 1)) };
        }
        "agents_never_change" => p.never_change = list(v),
        "needs_approval" => p.needs_approval = list(v),
        _ => return Err(format!("line {}: unknown policy rule {k} (known: agents_push_to_main, agents_never_change, needs_approval)", n + 1)),
    }
    for g in p.never_change.iter().chain(&p.needs_approval) { globset::Glob::new(g).map_err(|e| format!("line {}: {g} is not a valid glob: {e}", n + 1))?; }
    Ok(())
}

impl Policy {
    /// The repository's rules with the server's added. The server can only add: a rule the repository leaves out or
    /// relaxes still holds when the server sets it.
    pub fn with_server(mut self, server: &Policy) -> Policy {
        if server.agents_push_to_main_blocked && !self.agents_push_to_main_blocked { self.agents_push_to_main_blocked = true; self.from_server.push("agents_push_to_main".into()); }
        for (mine, theirs, name) in [(&mut self.never_change, &server.never_change, "agents_never_change"), (&mut self.needs_approval, &server.needs_approval, "needs_approval")] {
            for g in theirs { if !mine.contains(g) { mine.push(g.clone()); self.from_server.push(format!("{name}:{g}")); } }
        }
        self
    }
    pub fn is_empty(&self) -> bool { !self.agents_push_to_main_blocked && self.never_change.is_empty() && self.needs_approval.is_empty() }

    fn origin(&self, rule: &str) -> &'static str { if self.from_server.iter().any(|r| r == rule) { "the team policy on the gitta server" } else { "the [policy] section of .gitta.toml" } }

    /// The first glob of `globs` that `path` matches.
    fn hit<'a>(globs: &'a [String], path: &str) -> Option<&'a String> {
        globs.iter().find(|g| globset::GlobBuilder::new(g).literal_separator(true).build().map(|g| g.compile_matcher().is_match(path)).unwrap_or(false)
            // a directory glob such as `infra/` or `infra` covers what is inside it
            || { let d = g.trim_end_matches('/'); !d.contains(['*', '?', '[']) && path.starts_with(&format!("{d}/")) })
    }

    /// Checks agent work that changes `paths`. `approved`: a person approved it (ran the merge, `gitta approve`, or
    /// an approving pull request review). Err holds one plain line per broken rule.
    pub fn check_agent_change(&self, who: &str, paths: &[String], approved: bool) -> Result<(), Vec<String>> {
        let mut out = vec![];
        for p in paths {
            if let Some(g) = Self::hit(&self.never_change, p) {
                out.push(format!("{who} changed {p}, and agents never change {g}; the rule is in {}.", self.origin(&format!("agents_never_change:{g}"))));
            } else if !approved {
                if let Some(g) = Self::hit(&self.needs_approval, p) {
                    out.push(format!("{who} changed {p}, and changes to {g} need a person's approval; the rule is in {}.", self.origin(&format!("needs_approval:{g}"))));
                }
            }
        }
        if out.is_empty() { Ok(()) } else { Err(out) }
    }

    pub fn check_agent_push(&self, who: &str, main: &str) -> Result<(), String> {
        if !self.agents_push_to_main_blocked { return Ok(()); }
        Err(format!("{who} may not push to {main} directly; the rule is in {}. Push a branch and open a pull request.", self.origin("agents_push_to_main")))
    }
}

/// Where the rules the team server sent last are kept.
pub fn server_file(root: &Path) -> std::path::PathBuf { root.join(".gitta/server-policy.toml") }

/// The rules in force: `.gitta.toml` at `rev` (the target branch), plus the team server's. A broken file is an error
/// the check reports, never a silent pass.
pub fn load(root: &Path, rev: &str) -> Result<Policy, String> {
    let o = std::process::Command::new("git").arg("-C").arg(root).args(["show", &format!("{rev}:.gitta.toml")]).stderr(std::process::Stdio::null()).output();
    let repo = match o { Ok(o) if o.status.success() => parse(&String::from_utf8_lossy(&o.stdout)).map_err(|e| format!(".gitta.toml [policy]: {e}"))?, _ => Policy::default() };
    let server = std::fs::read_to_string(server_file(root)).ok().and_then(|t| parse(&t).ok()).unwrap_or_default();
    Ok(repo.with_server(&server))
}

/// Keeps the rules a team server sent (with a report answer), for the hooks to read without asking the server.
pub fn save_server(root: &Path, text: &str) {
    if parse(text).is_err() { return; }
    let f = server_file(root);
    if std::fs::read_to_string(&f).ok().as_deref() != Some(text) { let _ = std::fs::write(f, text); }
}

/// True when a commit message says an AI tool wrote it (the trailers agents add).
pub fn ai_trailer(msg: &str) -> bool {
    let t = msg.to_lowercase();
    t.lines().any(|l| l.trim_start().starts_with("co-authored-by") && ["claude", "codex", "copilot", "cursor", "gpt", "openai", "anthropic", "gemini", "devin"].iter().any(|k| l.contains(k)))
}

#[cfg(test)]
mod tests {
    use super::*;
    const FILE: &str = r#"
gate = "pytest -q"
weakened_checks = "block"

[policy]
agents_push_to_main = "block"   # agents may not push to main
agents_never_change = [
  "infra/**",
  ".github/workflows/*.yml",  # CI
]
needs_approval = ["db/migrations/**", 'auth']
"#;

    #[test]
    fn parses_the_section() {
        let p = parse(FILE).unwrap();
        assert!(p.agents_push_to_main_blocked);
        assert_eq!(p.never_change, vec!["infra/**", ".github/workflows/*.yml"]);
        assert_eq!(p.needs_approval, vec!["db/migrations/**", "auth"]);
        assert_eq!(parse("gate = \"x\"\n").unwrap(), Policy::default());
        assert!(parse("[policy]\nagents_push_to_main = \"maybe\"\n").is_err());
        assert!(parse("[policy]\nnever = [\"a\"]\n").unwrap_err().contains("unknown policy rule"));
        assert!(parse("[policy]\nagents_never_change = [\"a\"\n").is_err());
        assert!(parse("[policy]\nagents_never_change = [\"a[\"]\n").is_err());
    }

    #[test]
    fn checks_paths_and_pushes() {
        let p = parse(FILE).unwrap();
        let e = p.check_agent_change("agent-a", &["infra/prod/main.tf".into(), "src/app.py".into()], true).unwrap_err();
        assert_eq!(e, vec!["agent-a changed infra/prod/main.tf, and agents never change infra/**; the rule is in the [policy] section of .gitta.toml."]);
        assert!(p.check_agent_change("a", &[".github/workflows/ci.yml".into()], true).is_err());
        assert!(p.check_agent_change("a", &[".github/workflows/sub/ci.yml".into()], false).is_ok(), "* does not cross /");
        assert!(p.check_agent_change("a", &["auth/login.py".into()], false).unwrap_err()[0].contains("need a person's approval"));
        assert!(p.check_agent_change("a", &["auth/login.py".into(), "db/migrations/0003.sql".into()], true).is_ok());
        assert!(p.check_agent_change("a", &["authors.txt".into()], false).is_ok());
        assert!(p.check_agent_push("a", "main").unwrap_err().contains("may not push to main directly"));
        assert!(Policy::default().check_agent_push("a", "main").is_ok());
    }

    #[test]
    fn the_server_adds_rules_and_the_repo_cannot_remove_them() {
        let repo = parse("[policy]\nagents_push_to_main = \"allow\"\nagents_never_change = []\n").unwrap();
        let server = parse("[policy]\nagents_push_to_main = \"block\"\nagents_never_change = [\"secrets/**\"]\n").unwrap();
        let p = repo.with_server(&server);
        assert!(p.agents_push_to_main_blocked);
        assert!(p.check_agent_push("a", "main").unwrap_err().contains("team policy on the gitta server"));
        assert!(p.check_agent_change("a", &["secrets/k.txt".into()], true).unwrap_err()[0].contains("gitta server"));
        assert!(!p.is_empty());
    }

    #[test]
    fn trailers() {
        assert!(ai_trailer("fix\n\nCo-Authored-By: Claude <noreply@anthropic.com>"));
        assert!(!ai_trailer("fix\n\nCo-authored-by: Anna <a@example.com>"));
    }
}
