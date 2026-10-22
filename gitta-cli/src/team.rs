//! `gitta team`: personal tokens on the team servers, run by someone holding an admin token.
//!   gitta team token create --name "Anna Berg" [--email anna@example.com] [--runner] [--days 365]
//!   gitta team token revoke <id>
//!   gitta team token list
//!   gitta team whoami
//!   gitta login        sign in with the company account (single sign-on) and get your own token
//! The admin token comes from GITTA_ADMIN_TOKEN, or else the token this repository was set up with.
use serde_json::{json, Value};
use std::io::Write;
use std::process::{Command, Stdio};

fn call(url: &str, token: &str, method: &str, path: &str, body: Option<&Value>) -> Result<Value, String> {
    let mut c = Command::new("curl");
    if let Ok(ca) = std::env::var("GITTA_SERVER_CA") { c.args(["--cacert", &ca]); }
    c.args(["-sS", "--proto", "=http,https", "--connect-timeout", "2", "--max-time", "10", "-X", method, "-H", &format!("Authorization: Bearer {token}"),
        "-w", "\n%{http_code}", &format!("{url}{path}")]);
    if body.is_some() { c.args(["-H", "Content-Type: application/json", "--data-binary", "@-"]); }
    let out = c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()
        .and_then(|mut ch| { if let Some(b) = body { ch.stdin.take().unwrap().write_all(b.to_string().as_bytes())?; } else { drop(ch.stdin.take()); } ch.wait_with_output() })
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let (resp, code) = text.rsplit_once('\n').unwrap_or(("", "000"));
    match code { "200" => serde_json::from_str(resp).map_err(|e| e.to_string()), "000" => Err("did not answer".into()), c => Err(format!("{c}: {}", resp.trim())) }
}

pub fn cmd(args: &[String], team: Option<(String, String, String)>) {
    let Some((urls, token, _)) = team else { eprintln!("gitta: no team server configured (gitta init --server URL --token T)"); std::process::exit(1) };
    let token = std::env::var("GITTA_ADMIN_TOKEN").unwrap_or(token);
    let urls: Vec<&str> = urls.split(',').map(|u| u.trim().trim_end_matches('/')).filter(|u| !u.is_empty()).collect();
    let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let fail = |m: String| -> ! { eprintln!("gitta: {m}"); std::process::exit(1) };
    match (args.first().map(String::as_str), args.get(1).map(String::as_str)) {
        (Some("whoami"), _) => {
            let v = urls.iter().find_map(|u| call(u, &token, "GET", "/v1/whoami", None).ok()).unwrap_or_else(|| fail("no team server answered".into()));
            println!("{} ({}{})", v["name"].as_str().unwrap_or(""), v["kind"].as_str().unwrap_or(""), v["email"].as_str().filter(|e| !e.is_empty()).map(|e| format!(", {e}")).unwrap_or_default());
        }
        (Some("token"), Some("create")) => {
            let name = get("--name").unwrap_or_else(|| fail("usage: gitta team token create --name NAME [--email E] [--runner] [--days N]".into()));
            let body = json!({"name": name, "email": get("--email").unwrap_or_default(), "kind": if args.iter().any(|a| a == "--runner") { "runner" } else { "person" },
                "days": get("--days").and_then(|d| d.parse::<u64>().ok()).unwrap_or(365)});
            // Any server can issue it: they share the signing key, so the token works on all of them.
            let mut last = String::new();
            for u in &urls {
                match call(u, &token, "POST", "/v1/tokens", Some(&body)) {
                    Ok(v) => {
                        let id = &v["identity"];
                        println!("Token for {} ({}), id {}, valid {} days. Shown once; give it to them privately:\n\n  {}\n\nThey run, in each repository:\n  gitta init --server {} --token <token>",
                            id["name"].as_str().unwrap_or(""), id["kind"].as_str().unwrap_or(""), id["id"].as_str().unwrap_or(""), body["days"], v["token"].as_str().unwrap_or(""), urls.join(","));
                        return;
                    }
                    Err(e) => last = format!("{u}: {e}"),
                }
            }
            fail(format!("no token issued ({last})"))
        }
        (Some("token"), Some("revoke")) => {
            let id = args.get(2).cloned().unwrap_or_else(|| fail("usage: gitta team token revoke <id>".into()));
            // Every server keeps its own list of revoked tokens: tell all of them.
            let mut missed = vec![];
            for u in &urls { if let Err(e) = call(u, &token, "POST", "/v1/revoke", Some(&json!({"id": id}))) { missed.push(format!("{u} ({e})")); } }
            if missed.is_empty() { println!("gitta: token {id} revoked on {} server(s)", urls.len()); }
            else { fail(format!("token {id} revoked on {} of {} server(s); NOT on: {}. Run this again when they are up.", urls.len() - missed.len(), urls.len(), missed.join(", "))) }
        }
        (Some("token"), Some("list")) => {
            let mut issued: std::collections::BTreeMap<String, Value> = Default::default();
            let mut revoked = std::collections::BTreeSet::new();
            for u in &urls {
                if let Ok(v) = call(u, &token, "GET", "/v1/tokens", None) {
                    for x in v["issued"].as_array().cloned().unwrap_or_default() { issued.insert(x["identity"]["id"].as_str().unwrap_or("").to_string(), x); }
                    for r in v["revoked"].as_array().cloned().unwrap_or_default() { revoked.insert(r.as_str().unwrap_or("").to_string()); }
                }
            }
            if issued.is_empty() { println!("gitta: no personal tokens issued yet"); return; }
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            for (id, x) in issued {
                let i = &x["identity"];
                let state = if revoked.contains(&id) { "revoked".to_string() } else if i["exp"].as_u64().unwrap_or(0) < now { "expired".into() } else { format!("valid {} more days", (i["exp"].as_u64().unwrap_or(0) - now) / 86400) };
                println!("{id}  {:<7} {:<24} {:<28} {}", i["kind"].as_str().unwrap_or(""), i["name"].as_str().unwrap_or(""), i["email"].as_str().unwrap_or(""), state);
            }
        }
        _ => fail("usage: gitta team token create|revoke|list, gitta team whoami".into()),
    }
}

fn form(url: &str, fields: &[(&str, &str)]) -> Result<Value, String> {
    let mut c = Command::new("curl");
    c.args(["-sS", "--proto", "=https,http", "--max-time", "15", "-X", "POST", "-H", "Accept: application/json"]);
    for (k, v) in fields { c.args(["--data-urlencode", &format!("{k}={v}")]); }
    let out = c.arg(url).output().map_err(|e| e.to_string())?;
    serde_json::from_slice(&out.stdout).map_err(|_| format!("{url}: {}", String::from_utf8_lossy(&out.stdout).trim()))
}

/// Single sign-on with the device flow; returns the personal token the team server issued.
pub fn login(urls: &str) -> String {
    let fail = |m: String| -> ! { eprintln!("gitta: {m}"); std::process::exit(1) };
    let urls: Vec<&str> = urls.split(',').map(|u| u.trim().trim_end_matches('/')).filter(|u| !u.is_empty()).collect();
    let (server, sso) = urls.iter().find_map(|u| call(u, "", "GET", "/v1/sso", None).ok().map(|v| (*u, v)))
        .unwrap_or_else(|| fail("no team server offers single sign-on (ask your admin to set GITTA_OIDC_ISSUER and GITTA_OIDC_CLIENT_ID)".into()));
    let (issuer, client, secret) = (sso["issuer"].as_str().unwrap_or(""), sso["client_id"].as_str().unwrap_or(""), sso["client_secret"].as_str().unwrap_or(""));
    let disc: Value = Command::new("curl").args(["-fsS", "--max-time", "10", &format!("{issuer}/.well-known/openid-configuration")]).output().ok()
        .and_then(|o| serde_json::from_slice(&o.stdout).ok()).unwrap_or_else(|| fail(format!("cannot read {issuer}")));
    let dev_ep = disc["device_authorization_endpoint"].as_str().unwrap_or_else(|| fail(format!("{issuer} does not offer device sign-in")));
    let tok_ep = disc["token_endpoint"].as_str().unwrap_or_else(|| fail(format!("{issuer} has no token endpoint")));
    let mut base = vec![("client_id", client)];
    if !secret.is_empty() { base.push(("client_secret", secret)); }
    let mut f = base.clone(); f.push(("scope", "openid email profile"));
    let dev = form(dev_ep, &f).unwrap_or_else(|e| fail(e));
    let code = dev["device_code"].as_str().unwrap_or_else(|| fail(format!("sign-in did not start: {dev}"))).to_string();
    let link = dev["verification_uri_complete"].as_str().or(dev["verification_uri"].as_str()).or(dev["verification_url"].as_str()).unwrap_or("");
    println!("Sign in with your company account:\n\n  {link}\n  code: {}\n", dev["user_code"].as_str().unwrap_or(""));
    if std::env::var("GITTA_NO_BROWSER").is_err() {
        let _ = Command::new(if cfg!(target_os = "macos") { "open" } else { "xdg-open" }).arg(link).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    }
    let mut wait = dev["interval"].as_u64().unwrap_or(5);
    let until = std::time::Instant::now() + std::time::Duration::from_secs(dev["expires_in"].as_u64().unwrap_or(600));
    let id_token = loop {
        if std::time::Instant::now() > until { fail("sign-in timed out".into()) }
        std::thread::sleep(std::time::Duration::from_secs(wait));
        let mut f = base.clone(); f.push(("grant_type", "urn:ietf:params:oauth:grant-type:device_code")); f.push(("device_code", &code));
        let r = form(tok_ep, &f).unwrap_or_else(|e| fail(e));
        if let Some(t) = r["id_token"].as_str() { break t.to_string(); }
        match r["error"].as_str() {
            Some("authorization_pending") => {}
            Some("slow_down") => wait += 5,
            _ => fail(format!("sign-in failed: {}", r["error_description"].as_str().or(r["error"].as_str()).unwrap_or("unknown"))),
        }
    };
    let v = call(server, "", "POST", "/v1/login", Some(&json!({"id_token": id_token}))).unwrap_or_else(|e| fail(format!("the team server refused the sign-in: {e}")));
    println!("gitta: signed in as {} ({})", v["identity"]["name"].as_str().unwrap_or(""), v["identity"]["email"].as_str().unwrap_or(""));
    v["token"].as_str().unwrap_or("").to_string()
}
