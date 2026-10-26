//! Usage report, built from the audit trail: per period, repository and agent (or person), how many agents ran,
//! how many changes reached main, how many clashes gitta found, how many merges it stopped and how many of those
//! would have broken main (the tests failed on the combined code).
//!
//!   gitta report [--period day|week|month] [--by agent|person|repo] [--since 2026-10-01] [--until 2026-10-31]
//!                [--csv|--json] [--team]
//!
//! `--team` asks a team server for the report over every repository and machine of the team.

use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Period { Day, Week, Month, All }

impl Period {
    pub fn parse(s: &str) -> Option<Period> {
        match s { "day" | "daily" => Some(Period::Day), "week" | "weekly" => Some(Period::Week), "month" | "monthly" => Some(Period::Month), "all" => Some(Period::All), _ => None }
    }
    /// The period a `YYYY-MM-DD...` time falls in: `2026-10-22`, `2026-W43`, `2026-10` or `all`.
    pub fn of(self, time: &str) -> String {
        let d = time.get(..10).unwrap_or("");
        match self {
            Period::Day => d.to_string(),
            Period::Month => d.get(..7).unwrap_or("").to_string(),
            Period::All => "all".into(),
            Period::Week => {
                let p = |a: usize, b: usize| d.get(a..b).and_then(|x| x.parse::<i64>().ok());
                let (Some(y), Some(m), Some(dd)) = (p(0, 4), p(5, 7), p(8, 10)) else { return String::new() };
                let (wy, w) = iso_week(y, m, dd);
                format!("{wy}-W{w:02}")
            }
        }
    }
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400); let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// ISO 8601 week: (week-based year, week number).
pub fn iso_week(y: i64, m: i64, d: i64) -> (i64, i64) {
    let days = days_from_civil(y, m, d);
    let wd = (days + 3).rem_euclid(7) + 1; // Monday 1 .. Sunday 7 (1970-01-01 was a Thursday)
    let thursday = days - wd + 4;
    let ty = { let mut yy = y + 1; while days_from_civil(yy, 1, 1) > thursday { yy -= 1; } yy };
    (ty, (thursday - days_from_civil(ty, 1, 1)) / 7 + 1)
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum By { Agent, Person, Repo }

impl By {
    pub fn parse(s: &str) -> Option<By> { match s { "agent" => Some(By::Agent), "person" => Some(By::Person), "repo" | "team" => Some(By::Repo), _ => None } }
    fn name(self) -> &'static str { match self { By::Agent => "agent", By::Person => "person", By::Repo => "repo" } }
}

/// What one event counts towards.
#[derive(Default, Debug, PartialEq, Clone, Copy)]
pub struct Counts { pub changes: u64, pub clashes: u64, pub merges_stopped: u64, pub broken_mains_prevented: u64 }

pub fn counts(e: &Value) -> Counts {
    let d = &e["data"];
    let mut c = Counts::default();
    match e["event"].as_str().unwrap_or("") {
        // work that reached main: through gitta land, a checked git merge, or plain git
        "landed" | "merge_checked" | "reached_main" => c.changes = 1,
        "warning" if d["kind"].as_str() != Some("SECURITY_NOTE") => c.clashes = 1,
        // a main that already fails its tests alone was not kept working by the stop
        "merge_stopped" => { c.merges_stopped = 1; if d["base_fails"] != true { c.broken_mains_prevented = 1; } }
        "land_refused" => { c.merges_stopped = 1; if d["check"].as_str() == Some("tests") && d["base_fails"] != true { c.broken_mains_prevented = 1; } }
        "blocked" => c.merges_stopped = 1,
        _ => {}
    }
    c
}

fn person(e: &Value) -> String {
    let s = e["sent_by"]["name"].as_str().filter(|s| !s.is_empty()).or(e["actor"]["name"].as_str()).unwrap_or("");
    if s.is_empty() { "(unknown)".into() } else { s.to_string() }
}
fn agent(e: &Value) -> String { e["agent"].as_str().filter(|s| !s.is_empty()).map(String::from).unwrap_or_else(|| "(plain git)".into()) }

#[derive(Default)]
struct Acc { agents: BTreeSet<String>, c: Counts }

/// One row per period, repository and agent (or person), plus a total row per period (`repo` and the group "all").
/// Each event needs `repo` set when events from several repositories are mixed; otherwise `repo` names them all.
pub fn rows(events: &[Value], repo: &str, period: Period, by: By) -> Vec<Value> {
    let mut m: BTreeMap<(String, String, String), Acc> = BTreeMap::new();
    for e in events {
        let t = e["time"].as_str().unwrap_or("");
        if t.is_empty() { continue; }
        let p = period.of(t);
        let r = e["repo"].as_str().filter(|s| !s.is_empty()).unwrap_or(repo).to_string();
        let a = agent(e);
        let g = match by { By::Agent => a.clone(), By::Person => person(e), By::Repo => r.clone() };
        let c = counts(e);
        for key in [(p.clone(), r.clone(), g.clone()), (p.clone(), "all".into(), "all".into())] {
            let acc = m.entry(key).or_default();
            if a != "(plain git)" { acc.agents.insert(format!("{r}/{a}")); }
            acc.c.changes += c.changes; acc.c.clashes += c.clashes; acc.c.merges_stopped += c.merges_stopped; acc.c.broken_mains_prevented += c.broken_mains_prevented;
        }
    }
    let mut out: Vec<Value> = m.into_iter().map(|((p, r, g), a)| json!({
        "period": p, "repo": r, by.name(): g, "agents_run": a.agents.len(), "changes": a.c.changes, "clashes_found": a.c.clashes,
        "merges_stopped": a.c.merges_stopped, "broken_mains_prevented": a.c.broken_mains_prevented,
    })).collect();
    // within a period: the total last
    out.sort_by(|x, y| (x["period"].as_str(), x["repo"] == "all", x["repo"].as_str(), x[by.name()].as_str()).cmp(&(y["period"].as_str(), y["repo"] == "all", y["repo"].as_str(), y[by.name()].as_str())));
    out
}

const COLS: [(&str, &str); 5] = [("agents_run", "agents"), ("changes", "changes"), ("clashes_found", "clashes"), ("merges_stopped", "stopped"), ("broken_mains_prevented", "broken mains prevented")];

pub fn render(rows: &[Value], by: By, fmt: &str) -> String {
    let g = by.name();
    match fmt {
        "json" => serde_json::to_string_pretty(&json!({"by": g, "rows": rows})).unwrap_or_default() + "\n",
        "csv" => {
            let q = |s: &str| if s.contains([',', '"', '\n']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() };
            let gcol = if by == By::Repo { String::new() } else { format!("{g},") };
            let mut s = format!("period,repo,{gcol}{}\n", COLS.iter().map(|c| c.0).collect::<Vec<_>>().join(","));
            for r in rows {
                let gv = if by == By::Repo { String::new() } else { format!("{},", q(r[g].as_str().unwrap_or(""))) };
                s += &format!("{},{},{gv}{}\n", q(r["period"].as_str().unwrap_or("")), q(r["repo"].as_str().unwrap_or("")),
                    COLS.iter().map(|c| r[c.0].to_string()).collect::<Vec<_>>().join(","));
            }
            s
        }
        _ => {
            if rows.is_empty() { return "gitta: nothing in the audit trail for this period yet\n".into(); }
            let w = |k: &str, min: usize| rows.iter().map(|r| r[k].as_str().unwrap_or("").chars().count()).max().unwrap_or(0).max(min);
            let (wp, wr) = (w("period", 6), w("repo", 5));
            let wg = if by == By::Repo { 0 } else { w(g, g.len()) };
            let cell = |v: &str| if by == By::Repo { String::new() } else { format!("{v:<wg$}  ") };
            let mut s = format!("{:<wp$}  {:<wr$}  {}{}\n", "period", "repo", cell(g), COLS.iter().map(|c| c.1).collect::<Vec<_>>().join("  "));
            for r in rows {
                let (repo, grp) = if r["repo"] == "all" { ("total", "") } else { (r["repo"].as_str().unwrap_or(""), r[g].as_str().unwrap_or("")) };
                s += &format!("{:<wp$}  {:<wr$}  {}{}\n", r["period"].as_str().unwrap_or(""), repo, cell(grp),
                    COLS.iter().map(|c| format!("{:>w$}", r[c.0], w = c.1.len())).collect::<Vec<_>>().join("  "));
            }
            s
        }
    }
}

pub struct Opts { pub period: Period, pub by: By, pub since: Option<String>, pub until: Option<String>, pub fmt: String, pub team: bool }

pub fn parse_args(args: &[String]) -> Result<Opts, String> {
    let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let period = match get("--period") { Some(p) => Period::parse(&p).ok_or(format!("--period is day, week, month or all, not {p}"))?, None => Period::Week };
    let by = match get("--by") { Some(b) => By::parse(&b).ok_or(format!("--by is agent, person or repo, not {b}"))?, None => By::Agent };
    let fmt = if args.iter().any(|a| a == "--json") { "json" } else if args.iter().any(|a| a == "--csv") { "csv" } else { "text" };
    Ok(Opts { period, by, since: get("--since"), until: get("--until"), fmt: fmt.into(), team: args.iter().any(|a| a == "--team") })
}

/// Keeps the events from `since` (inclusive) up to `until` (inclusive, a whole day when only a date is given).
pub fn within(events: Vec<Value>, since: Option<&str>, until: Option<&str>) -> Vec<Value> {
    events.into_iter().filter(|e| {
        let t = e["time"].as_str().unwrap_or("");
        since.is_none_or(|s| t >= s) && until.is_none_or(|u| t.get(..u.len()).unwrap_or(t) <= u)
    }).collect()
}

/// The recorded failure packages (`.gitta/failures/*.json`), for the integration report.
pub fn read_failures(root: &std::path::Path) -> Vec<Value> {
    std::fs::read_dir(root.join(".gitta/failures")).into_iter().flatten().flatten()
        .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok()).filter_map(|s| serde_json::from_str(&s).ok()).collect()
}

/// Follow work through to correct integration, including repair. Every figure is MEASURED from this
/// repository's audit trail and failure records; nothing here is estimated. Comparison with a team's existing
/// Git/CI workflow is left to their own baseline, which this does not invent.
pub fn integration(events: &[Value], failures: &[Value]) -> Value {
    let kind = |e: &Value| e["event"].as_str().unwrap_or("").to_string();
    let f64of = |e: &Value, path: &[&str]| { let mut v = e; for k in path { v = &v[k]; } v.as_f64() };
    let (mut integrated, mut stopped, mut validations, mut reused, mut gate_seconds) = (0u64, 0u64, 0u64, 0u64, 0f64);
    let (mut env_failures, mut skipped, mut warnings, mut approvals) = (0u64, 0u64, 0u64, 0u64);
    for e in events {
        match kind(e).as_str() {
            "landed" | "reached_main" => integrated += 1,
            "merge_stopped" => stopped += 1,
            "gate_environment" => env_failures += 1,
            "checks_skipped" => skipped += 1,
            "warning" => warnings += 1,
            "approved" => approvals += 1,
            _ => {}
        }
        if kind(e) == "merge_checked" {
            validations += 1;
            if e["data"]["tests"]["tested_at_commit"].as_bool() == Some(true) { reused += 1; }
            if let Some(s) = f64of(e, &["data", "tests", "seconds"]) { gate_seconds += s; }
        }
    }
    let recorded = failures.len() as u64;
    let rerun_attempts: u64 = failures.iter().map(|f| f["attempts"].as_array().map(|a| a.len() as u64).unwrap_or(0)).sum();
    let reproduced = failures.iter().filter(|f| f["reproduced"].as_bool() == Some(true)).count() as u64;
    let env_mismatches = failures.iter().filter(|f| f["status"].as_str() == Some("environment mismatch")).count() as u64;
    json!({
        "measured": true,
        "integration": {"integrated": integrated, "merges_stopped_before_main": stopped},
        "validation": {"gate_runs_recorded": validations, "results_reused": reused, "gate_seconds": (gate_seconds * 10.0).round() / 10.0},
        "repair": {"failures_recorded": recorded, "rerun_attempts": rerun_attempts, "reproduced": reproduced, "environment_mismatches": env_mismatches},
        "checks": {"environment_failures": env_failures, "skipped": skipped, "warnings": warnings, "approvals": approvals},
        "note": "All figures are measured from this repository's audit trail and failure records; none are estimated. Compare against your own Git/CI baseline."
    })
}

pub fn render_integration(v: &Value, fmt: &str) -> String {
    if fmt == "json" { return serde_json::to_string_pretty(v).unwrap() + "\n"; }
    let g = |a: &str, b: &str| v[a][b].as_f64().map(|x| if x.fract() == 0.0 { format!("{}", x as i64) } else { format!("{x}") }).unwrap_or_else(|| "0".into());
    let (i, val, rep, ch) = (&v["integration"], &v["validation"], &v["repair"], &v["checks"]);
    let n = |x: &Value, k: &str| x[k].as_u64().unwrap_or(0);
    format!("gitta integration report (measured, not estimated)\n\n\
        Integration\n  integrated into main        {}\n  merges stopped before main  {}\n\n\
        Validation\n  gate runs recorded          {}\n  results reused (saved runs) {}\n  gate seconds                {}\n\n\
        Repair\n  failures recorded           {}\n  rerun attempts              {}\n  reproduced                  {}\n  environment mismatches      {}\n\n\
        Checks\n  environment failures        {}\n  checks skipped              {}\n  warnings                    {}\n  approvals                   {}\n\n{}\n",
        n(i, "integrated"), n(i, "merges_stopped_before_main"),
        n(val, "gate_runs_recorded"), n(val, "results_reused"), g("validation", "gate_seconds"),
        n(rep, "failures_recorded"), n(rep, "rerun_attempts"), n(rep, "reproduced"), n(rep, "environment_mismatches"),
        n(ch, "environment_failures"), n(ch, "skipped"), n(ch, "warnings"), n(ch, "approvals"),
        v["note"].as_str().unwrap_or(""))
}

/// `gitta report`: this repository from the local audit trail, or the whole team from a team server.
pub fn cmd(root: &std::path::Path, args: &[String], team: Option<(String, String, String)>) {
    if args.iter().any(|a| a == "--integration") {
        let o = parse_args(args).unwrap_or(Opts { period: Period::All, by: By::Agent, since: None, until: None, fmt: "text".into(), team: false });
        let events = within(crate::audit::read(root), o.since.as_deref(), o.until.as_deref());
        print!("{}", render_integration(&integration(&events, &read_failures(root)), &o.fmt));
        return;
    }
    let o = match parse_args(args) { Ok(o) => o, Err(e) => { eprintln!("gitta: {e}"); std::process::exit(2) } };
    if o.team {
        let Some((urls, token, _)) = team else { eprintln!("gitta: no team server configured (gitta init --server URL --token T)"); std::process::exit(1) };
        for url in urls.split(',').map(|u| u.trim().trim_end_matches('/')).filter(|u| !u.is_empty()) {
            let mut curl = std::process::Command::new("curl");
            if let Ok(ca) = std::env::var("GITTA_SERVER_CA") { curl.args(["--cacert", &ca]); }
            curl.args(["-fsS", "--connect-timeout", "2", "--max-time", "60", "-H", &format!("Authorization: Bearer {token}"), "--get"]);
            for (k, v) in [("period", Some(format!("{:?}", o.period).to_lowercase())), ("by", Some(o.by.name().to_string())), ("since", o.since.clone()), ("until", o.until.clone())] {
                if let Some(v) = v { curl.args(["--data-urlencode", &format!("{k}={v}")]); }
            }
            let out = curl.arg(format!("{url}/v1/usage")).output();
            if let Some(v) = out.ok().filter(|x| x.status.success()).and_then(|x| serde_json::from_slice::<Value>(&x.stdout).ok()) {
                print!("{}", render(v["rows"].as_array().map(Vec::as_slice).unwrap_or(&[]), o.by, &o.fmt));
                return;
            }
        }
        eprintln!("gitta: no team server answered"); std::process::exit(1)
    }
    let name = root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let events = within(crate::audit::read(root), o.since.as_deref(), o.until.as_deref());
    print!("{}", render(&rows(&events, &name, o.period, o.by), o.by, &o.fmt));
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ev(time: &str, event: &str, agent: &str, data: Value) -> Value { json!({"time": time, "event": event, "agent": agent, "actor": {"name": "Anna"}, "data": data}) }

    fn trail() -> Vec<Value> {
        vec![
            ev("2026-10-19T09:00:00Z", "agent_started", "a1", json!({})),
            ev("2026-10-19T09:00:01Z", "agent_started", "a2", json!({})),
            ev("2026-10-19T09:10:00Z", "warning", "a1", json!({"kind": "CHANGED_MEANING"})),
            ev("2026-10-19T09:11:00Z", "warning", "a1", json!({"kind": "SECURITY_NOTE"})),
            ev("2026-10-19T10:00:00Z", "merge_stopped", "a2", json!({"failing": ["t"]})),
            ev("2026-10-19T10:05:00Z", "land_refused", "a2", json!({"check": "credentials"})),
            ev("2026-10-19T10:06:00Z", "land_refused", "a2", json!({"check": "tests"})),
            ev("2026-10-19T10:07:00Z", "land_refused", "a2", json!({"check": "tests", "base_fails": true})),
            ev("2026-10-19T10:08:00Z", "merge_stopped", "a2", json!({"failing": ["t"], "base_fails": true})),
            ev("2026-10-20T11:00:00Z", "landed", "a1", json!({})),
            ev("2026-10-20T11:00:00Z", "merge_checked", "a2", json!({})),
            ev("2026-10-26T08:00:00Z", "reached_main", "", json!({})),
        ]
    }

    #[test]
    fn counts_per_agent_and_week() {
        let r = rows(&trail(), "shop", Period::Week, By::Agent);
        let find = |p: &str, repo: &str, a: &str| r.iter().find(|x| x["period"] == p && x["repo"] == repo && x["agent"] == a).cloned().unwrap_or_else(|| panic!("{p} {a}: {r:?}"));
        let a1 = find("2026-W43", "shop", "a1");
        assert_eq!((a1["changes"].as_u64(), a1["clashes_found"].as_u64(), a1["agents_run"].as_u64()), (Some(1), Some(1), Some(1)));
        let a2 = find("2026-W43", "shop", "a2");
        assert_eq!((a2["merges_stopped"].as_u64(), a2["broken_mains_prevented"].as_u64(), a2["changes"].as_u64()), (Some(5), Some(2), Some(1)));
        let tot = find("2026-W43", "all", "all");
        assert_eq!((tot["agents_run"].as_u64(), tot["changes"].as_u64(), tot["merges_stopped"].as_u64()), (Some(2), Some(2), Some(5)));
        let w44 = find("2026-W44", "shop", "(plain git)");
        assert_eq!((w44["changes"].as_u64(), w44["agents_run"].as_u64()), (Some(1), Some(0)));
        // the total row comes last in its period
        assert_eq!(r.iter().filter(|x| x["period"] == "2026-W43").last().unwrap()["repo"], "all");
    }

    #[test]
    fn periods_people_and_repos() {
        assert_eq!(Period::Week.of("2026-01-01T00:00:00Z"), "2026-W01");
        assert_eq!(Period::Week.of("2027-01-01T00:00:00Z"), "2026-W53");
        assert_eq!(Period::Week.of("2024-12-30T00:00:00Z"), "2025-W01");
        assert_eq!(Period::Month.of("2026-10-22T10:00:00Z"), "2026-10");
        let mut t = trail();
        t[0]["repo"] = json!("api");
        t[0]["sent_by"] = json!({"name": "Bea"});
        let r = rows(&t, "shop", Period::Month, By::Person);
        assert!(r.iter().any(|x| x["person"] == "Bea" && x["repo"] == "api" && x["agents_run"] == 1));
        let r = rows(&t, "shop", Period::All, By::Repo);
        assert_eq!(r.iter().filter(|x| x["repo"] != "all").count(), 2);
        assert_eq!(within(trail(), Some("2026-10-20"), Some("2026-10-20")).len(), 2);
    }

    #[test]
    fn integration_report_counts_from_trail_and_failures() {
        let events = vec![
            ev("2026-10-20T10:00:00Z", "merge_checked", "a1", json!({"tests": {"result": "passed", "seconds": 3.0, "tested_at_commit": false}})),
            ev("2026-10-20T10:01:00Z", "merge_checked", "a1", json!({"tests": {"result": "passed", "seconds": 0.0, "tested_at_commit": true}})),
            ev("2026-10-20T10:02:00Z", "landed", "a1", json!({})),
            ev("2026-10-20T10:03:00Z", "merge_stopped", "a2", json!({"failing": ["t"]})),
            ev("2026-10-20T10:04:00Z", "gate_environment", "a2", json!({"missing": "tsc"})),
            ev("2026-10-20T10:05:00Z", "warning", "a1", json!({})),
        ];
        let failures = vec![
            json!({"status": "reproduced", "reproduced": true, "attempts": [{"result": "reproduced"}]}),
            json!({"status": "environment mismatch", "reproduced": false, "attempts": [{"result": "environment mismatch"}, {"result": "environment mismatch"}]}),
        ];
        let v = integration(&events, &failures);
        assert_eq!(v["integration"]["integrated"], 1);
        assert_eq!(v["integration"]["merges_stopped_before_main"], 1);
        assert_eq!(v["validation"]["gate_runs_recorded"], 2);
        assert_eq!(v["validation"]["results_reused"], 1);
        assert_eq!(v["validation"]["gate_seconds"], 3.0);
        assert_eq!(v["repair"]["failures_recorded"], 2);
        assert_eq!(v["repair"]["rerun_attempts"], 3);
        assert_eq!(v["repair"]["reproduced"], 1);
        assert_eq!(v["repair"]["environment_mismatches"], 1);
        assert_eq!(v["checks"]["environment_failures"], 1);
        assert_eq!(v["checks"]["warnings"], 1);
        // text render carries the "measured" framing
        assert!(render_integration(&v, "text").contains("measured, not estimated"));
    }

    #[test]
    fn three_formats() {
        let r = rows(&trail(), "shop", Period::Month, By::Agent);
        let text = render(&r, By::Agent, "text");
        assert!(text.starts_with("period") && text.contains("total") && text.contains("broken mains prevented"), "{text}");
        let csv = render(&r, By::Agent, "csv");
        assert_eq!(csv.lines().next().unwrap(), "period,repo,agent,agents_run,changes,clashes_found,merges_stopped,broken_mains_prevented");
        assert!(csv.contains("2026-10,all,all,2,3,1,5,2"), "{csv}");
        let j: Value = serde_json::from_str(&render(&r, By::Agent, "json")).unwrap();
        assert_eq!(j["rows"].as_array().unwrap().len(), r.len());
        assert!(render(&[], By::Agent, "text").contains("nothing"));
        assert!(parse_args(&["--period".into(), "year".into()]).is_err());
    }
}
