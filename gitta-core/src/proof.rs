//! Does the test run prove the change? Coverage of the lines an agent changed, read from the coverage report the
//! team's own test run already writes. gitta does not run a coverage tool itself; it reads what the gate produced:
//!   lcov (`SF:` / `DA:`), Cobertura XML (`<class filename>` / `<line number hits>`, also coverage.py's XML),
//!   JaCoCo XML (`<sourcefile name>` / `<line nr ci>`), coverage.py JSON (`executed_lines` / `missing_lines`)
//!   and Go cover profiles (`file:start.col,end.col stmts count`).
//! A changed line the report does not list is not executable (a comment, a blank line, a declaration) and is not
//! counted. Paths in reports are matched to repository paths by their longest common suffix.

use std::collections::{BTreeMap, BTreeSet};

/// Per file in the report: executable line -> whether a test ran it.
pub type Coverage = BTreeMap<String, BTreeMap<u32, bool>>;

/// Read a coverage report in any supported format. None when the text is none of them.
pub fn parse(text: &str) -> Option<Coverage> {
    let t = text.trim_start();
    if t.starts_with('{') { return coverage_py_json(t); }
    if t.contains("<sourcefile") || t.contains("<!DOCTYPE report") { return jacoco(t); }
    if t.contains("<class") && t.contains("<line") { return cobertura(t); }
    if t.starts_with("mode:") { return go_profile(t); }
    if t.contains("SF:") && t.contains("DA:") { return lcov(t); }
    None
}

fn lcov(t: &str) -> Option<Coverage> {
    let mut out = Coverage::new();
    let mut file: Option<String> = None;
    for l in t.lines() {
        if let Some(f) = l.strip_prefix("SF:") { file = Some(f.trim().to_string()); continue; }
        if let (Some(d), Some(f)) = (l.strip_prefix("DA:"), &file) {
            let mut it = d.split(',');
            if let (Some(n), Some(c)) = (it.next().and_then(|x| x.trim().parse().ok()), it.next().and_then(|x| x.trim().parse::<u64>().ok())) {
                let e = out.entry(f.clone()).or_default().entry(n).or_insert(false);
                *e |= c > 0;
            }
        }
        if l.starts_with("end_of_record") { file = None; }
    }
    (!out.is_empty()).then_some(out)
}

/// The value of `attr="..."` in an XML tag.
fn attr<'a>(tag: &'a str, attr: &str) -> Option<&'a str> {
    let k = format!(" {attr}=\"");
    let i = tag.find(&k)? + k.len();
    tag[i..].split('"').next()
}

fn tags<'a>(t: &'a str, name: &str) -> Vec<(usize, &'a str)> {
    let open = format!("<{name} ");
    t.match_indices(open.as_str()).filter_map(|(i, _)| t[i..].find('>').map(|j| (i, &t[i..i + j]))).collect()
}

fn cobertura(t: &str) -> Option<Coverage> {
    let mut out = Coverage::new();
    let classes: Vec<(usize, String)> = tags(t, "class").into_iter().filter_map(|(i, g)| attr(g, "filename").map(|f| (i, f.to_string()))).collect();
    for (k, (start, file)) in classes.iter().enumerate() {
        let end = classes.get(k + 1).map(|c| c.0).unwrap_or(t.len());
        for (_, g) in tags(&t[*start..end], "line").into_iter() {
            if let (Some(n), Some(h)) = (attr(g, "number").and_then(|x| x.parse().ok()), attr(g, "hits").and_then(|x| x.parse::<u64>().ok())) {
                *out.entry(file.clone()).or_default().entry(n).or_insert(false) |= h > 0;
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

fn jacoco(t: &str) -> Option<Coverage> {
    let mut out = Coverage::new();
    let pkgs: Vec<(usize, String)> = tags(t, "package").into_iter().filter_map(|(i, g)| attr(g, "name").map(|n| (i, n.to_string()))).collect();
    let files: Vec<(usize, String)> = tags(t, "sourcefile").into_iter().filter_map(|(i, g)| attr(g, "name").map(|n| (i, n.to_string()))).collect();
    for (k, (start, name)) in files.iter().enumerate() {
        let end = files.get(k + 1).map(|c| c.0).unwrap_or(t.len());
        let pkg = pkgs.iter().rev().find(|(i, _)| i < start).map(|(_, p)| p.as_str()).unwrap_or("");
        let path = if pkg.is_empty() { name.clone() } else { format!("{pkg}/{name}") };
        for (_, g) in tags(&t[*start..end], "line").into_iter() {
            if let (Some(n), Some(ci)) = (attr(g, "nr").and_then(|x| x.parse().ok()), attr(g, "ci").and_then(|x| x.parse::<u64>().ok())) {
                *out.entry(path.clone()).or_default().entry(n).or_insert(false) |= ci > 0;
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

fn coverage_py_json(t: &str) -> Option<Coverage> {
    let v: serde_json::Value = serde_json::from_str(t).ok()?;
    let mut out = Coverage::new();
    for (f, d) in v.get("files")?.as_object()? {
        let m = out.entry(f.clone()).or_default();
        for (k, covered) in [("executed_lines", true), ("missing_lines", false)] {
            for n in d[k].as_array().into_iter().flatten().filter_map(|x| x.as_u64()) { *m.entry(n as u32).or_insert(false) |= covered; }
        }
    }
    (!out.is_empty()).then_some(out)
}

fn go_profile(t: &str) -> Option<Coverage> {
    let mut out = Coverage::new();
    for l in t.lines().skip(1) {
        let Some((file, rest)) = l.split_once(':') else { continue };
        let mut parts = rest.split_whitespace();
        let (Some(range), Some(_stmts), Some(count)) = (parts.next(), parts.next(), parts.next().and_then(|c| c.parse::<u64>().ok())) else { continue };
        let Some((a, b)) = range.split_once(',') else { continue };
        let line = |x: &str| x.split('.').next().and_then(|n| n.parse::<u32>().ok());
        let (Some(s), Some(e)) = (line(a), line(b)) else { continue };
        let m = out.entry(file.to_string()).or_default();
        for n in s..=e { *m.entry(n).or_insert(false) |= count > 0; }
    }
    (!out.is_empty()).then_some(out)
}

/// The 1-based line numbers of `new` that the change added or rewrote, compared with `base`.
pub fn changed_lines(base: &str, new: &str) -> BTreeSet<u32> {
    let (a, b): (Vec<&str>, Vec<&str>) = (base.lines().collect(), new.lines().collect());
    match crate::merge::myers(&a, &b) {
        Some(hunks) => {
            let mut out = BTreeSet::new();
            let mut shift: isize = 0;
            for (s, e, rep) in hunks {
                let start = (s as isize + shift) as usize;
                for i in 0..rep.len() { out.insert((start + i + 1) as u32); }
                shift += rep.len() as isize - (e - s) as isize;
            }
            out
        }
        // Too many edits for the in-process diff: every line that is not in base counts as changed.
        None => { let old: BTreeSet<&str> = a.into_iter().collect(); b.iter().enumerate().filter(|(_, l)| !old.contains(*l)).map(|(i, _)| i as u32 + 1).collect() }
    }
}

/// The report's entry for a repository path: exact, or the report path that shares the longest suffix with it.
pub fn entry<'a>(cov: &'a Coverage, path: &str) -> Option<&'a BTreeMap<u32, bool>> {
    if let Some(m) = cov.get(path) { return Some(m); }
    let segs: Vec<&str> = path.split('/').collect();
    cov.iter().map(|(k, m)| {
        let ks: Vec<&str> = k.trim_start_matches("./").split(['/', '\\']).collect();
        let n = segs.iter().rev().zip(ks.iter().rev()).take_while(|(a, b)| a == b).count();
        (n, m)
    }).filter(|(n, _)| *n > 0 && *n >= segs.len().min(2)).max_by_key(|(n, _)| *n).map(|(_, m)| m)
}

/// Changed executable lines and the ones no test ran, per file.
#[derive(Debug, Default, PartialEq)]
pub struct ChangeCoverage { pub executable: usize, pub uncovered: BTreeMap<String, Vec<u32>> }

impl ChangeCoverage {
    pub fn covered_pct(&self) -> f64 {
        if self.executable == 0 { return 100.0; }
        let miss: usize = self.uncovered.values().map(Vec::len).sum();
        100.0 * (self.executable - miss) as f64 / self.executable as f64
    }
}

/// How much of a change the tests ran: `files` holds (path, base text, new text) for each changed file.
pub fn change_coverage(cov: &Coverage, files: &[(String, String, String)]) -> ChangeCoverage {
    let mut out = ChangeCoverage::default();
    for (p, b, n) in files {
        if crate::guard::is_test(p) { continue; }
        let Some(m) = entry(cov, p) else { continue };
        for l in changed_lines(b, n) {
            match m.get(&l) {
                Some(true) => out.executable += 1,
                Some(false) => { out.executable += 1; out.uncovered.entry(p.clone()).or_default().push(l); }
                None => {}
            }
        }
    }
    out
}

// ---- static analysis reports (SARIF 2.1.0) ----

/// One result from a SARIF report: the tool, its rule, its level and where it points.
#[derive(Debug, Clone, PartialEq)]
pub struct SarifResult { pub tool: String, pub rule: String, pub level: String, pub path: String, pub line: u32, pub message: String }

/// The results of a SARIF report (QAC, Polyspace, Axivion, Parasoft, CodeQL, cppcheck and others write it).
/// Results without a file and line are left out: they cannot be tied to a change.
pub fn sarif(text: &str) -> Option<Vec<SarifResult>> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let mut out = vec![];
    for run in v["runs"].as_array()? {
        let tool = run["tool"]["driver"]["name"].as_str().unwrap_or("static analysis").to_string();
        for r in run["results"].as_array().into_iter().flatten() {
            if r["baselineState"] == "unchanged" || r["suppressions"].as_array().is_some_and(|s| !s.is_empty()) { continue; }
            let loc = &r["locations"][0]["physicalLocation"];
            let Some(uri) = loc["artifactLocation"]["uri"].as_str() else { continue };
            let Some(line) = loc["region"]["startLine"].as_u64() else { continue };
            let path = uri.trim_start_matches("file://").trim_start_matches("./").to_string();
            out.push(SarifResult {
                tool: tool.clone(), rule: r["ruleId"].as_str().unwrap_or("").to_string(),
                level: r["level"].as_str().unwrap_or("warning").to_string(), path, line: line as u32,
                message: r["message"]["text"].as_str().unwrap_or("").lines().next().unwrap_or("").to_string(),
            });
        }
    }
    Some(out)
}

/// The error and warning results that point at a line the change added or rewrote: `files` holds (path, base text,
/// new text) for each changed file. A report path matches a repository path exactly or by its longest shared suffix.
pub fn sarif_on_change<'a>(results: &'a [SarifResult], files: &[(String, String, String)]) -> Vec<&'a SarifResult> {
    let changed: Vec<(&String, BTreeSet<u32>)> = files.iter().map(|(p, b, n)| (p, changed_lines(b, n))).collect();
    results.iter().filter(|r| r.level == "error" || r.level == "warning").filter(|r| {
        changed.iter().any(|(p, ls)| ls.contains(&r.line) && (r.path == **p || r.path.ends_with(&format!("/{p}")) || p.ends_with(&format!("/{}", r.path))))
    }).collect()
}

// ---- mutation testing on changed lines ----

/// One mutant: a changed line with one operator replaced.
#[derive(Debug, Clone, PartialEq)]
pub struct Mutant { pub path: String, pub line: u32, pub before: String, pub after: String }

/// Operator swaps, longest first so `<=` is never read as `<`. Each is (from, to, needs word boundaries).
const SWAPS: &[(&str, &str, bool)] = &[
    ("<=", ">", false), (">=", "<", false), ("==", "!=", false), ("!=", "==", false), ("&&", "||", false), ("||", "&&", false),
    ("<", ">=", false), (">", "<=", false), (" and ", " or ", false), (" or ", " and ", false),
    ("true", "false", true), ("false", "true", true), ("True", "False", true), ("False", "True", true), (" + ", " - ", false), (" - ", " + ", false),
];

/// Lines a mutant there says nothing useful about: comments, logging and printing, imports, blank lines (the kinds
/// Google's diff-based mutation testing suppresses as unproductive, Petrović et al., TSE 2021).
fn unproductive(l: &str) -> bool {
    let t = l.trim();
    t.is_empty() || ["//", "#", "/*", "*", "--", "import ", "from ", "use ", "#include", "package "].iter().any(|p| t.starts_with(p))
        || ["log", "print", "debug", "trace", "console.", "fmt.Print", "println!", "eprintln!", "assert"].iter().any(|w| t.contains(w))
        || t.contains("->") && !t.contains(" = ") && t.ends_with('{')
}

/// Where `from` occurs in `l` outside string and character literals, and as a whole word when asked.
fn find_op(l: &str, from: &str, word: bool) -> Option<usize> {
    let b = l.as_bytes();
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if let Some(q) = quote { if c == b'\\' { i += 2; continue; } if c == q { quote = None; } i += 1; continue; }
        if c == b'"' || c == b'\'' || c == b'`' { quote = Some(c); i += 1; continue; }
        if l[i..].starts_with(from) {
            let before = if i == 0 { b' ' } else { b[i - 1] };
            let after = b.get(i + from.len()).copied().unwrap_or(b' ');
            let ident = |x: u8| x.is_ascii_alphanumeric() || x == b'_';
            // A comparison `<` or `>` stands between spaces; otherwise it is a generic, a shift, an arrow or part of `<=`.
            let angle = matches!(from, "<" | ">") && !(before == b' ' && after == b' ');
            if !angle && (!word || !ident(before) && !ident(after)) { return Some(i); }
        }
        i += 1;
    }
    None
}

/// Up to `budget` mutants on the changed code lines of `files` (path, base text, new text), spread over the lines
/// in a fixed order so the same change always gets the same mutants. Test files are left alone.
pub fn mutants(files: &[(String, String, String)], budget: usize) -> Vec<Mutant> {
    let mut per_line: Vec<Vec<Mutant>> = vec![];
    for (p, b, n) in files {
        if crate::guard::is_test(p) || crate::lang_of(p).is_none() { continue; }
        let lines: Vec<&str> = n.lines().collect();
        for l in changed_lines(b, n) {
            let Some(text) = lines.get(l as usize - 1) else { continue };
            if unproductive(text) { continue; }
            let mut taken: Vec<(usize, usize)> = vec![];
            let mut ms = vec![];
            for (from, to, word) in SWAPS {
                let Some(i) = find_op(text, from, *word) else { continue };
                if taken.iter().any(|(s, e)| i < *e && i + from.len() > *s) { continue; }
                taken.push((i, i + from.len()));
                ms.push(Mutant { path: p.clone(), line: l, before: text.to_string(), after: format!("{}{}{}", &text[..i], to, &text[i + from.len()..]) });
            }
            if !ms.is_empty() { per_line.push(ms); }
        }
    }
    // One mutant from each line first, then the second of each, and so on.
    let mut out = vec![];
    let mut k = 0;
    while out.len() < budget && per_line.iter().any(|ms| ms.len() > k) {
        for ms in &per_line { if let Some(m) = ms.get(k) { if out.len() < budget { out.push(m.clone()); } } }
        k += 1;
    }
    out
}

// ---- changed lines actually compiled (JMake, Lawall and Muller, DSN 2017) ----

/// The source lines of `file` that a preprocessor output keeps, read from its line markers (`# 12 "src/a.c"` and
/// `#line 12 "src/a.c"`). A line the preprocessor dropped (an inactive `#if` branch) is not among them.
pub fn preprocessed_lines(out: &str, file: &str) -> BTreeSet<u32> {
    let mut keep = BTreeSet::new();
    let (mut cur, mut line) = (false, 0u32);
    for l in out.lines() {
        let t = l.trim_start();
        let marker = t.strip_prefix("#line ").or_else(|| t.strip_prefix("# ").filter(|r| r.starts_with(|c: char| c.is_ascii_digit())));
        if let Some(m) = marker {
            let mut it = m.splitn(2, ' ');
            let n = it.next().and_then(|n| n.parse::<u32>().ok());
            let f = it.next().map(|r| r.trim_start().trim_start_matches('"').split('"').next().unwrap_or(""));
            // The compiler names the file as it was given: relative, absolute, or with ./ in front.
            if let (Some(n), Some(f)) = (n, f) { let f = f.trim_start_matches("./"); cur = f == file || f.ends_with(&format!("/{file}")) || file.ends_with(&format!("/{f}")); line = n; }
            continue;
        }
        if cur { if !t.is_empty() { keep.insert(line); } line += 1; }
    }
    keep
}

/// Changed lines of real code (not blank, not a comment, not a preprocessor line) that the preprocessor output does
/// not contain: code the tested build never compiled.
pub fn uncompiled(base: &str, new: &str, compiled: &BTreeSet<u32>) -> Vec<u32> {
    let lines: Vec<&str> = new.lines().collect();
    changed_lines(base, new).into_iter().filter(|l| {
        let t = lines.get(*l as usize - 1).map(|s| s.trim()).unwrap_or("");
        !t.is_empty() && !t.starts_with('#') && !t.starts_with("//") && !t.starts_with("/*") && !t.starts_with('*') && t != "{" && t != "}"
    }).filter(|l| !compiled.contains(l)).collect()
}
