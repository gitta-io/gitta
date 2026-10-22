//! Security layer 2: changes to security controls between agents.
//!
//! A symbol is security-relevant when
//!   - its text carries a well-known control (CONTROLS: auth decorators, attributes, annotations, guards, middleware),
//!   - its name looks like a check (NAMES: check_permission, authorize, verify_*, sanitize* ...), or
//!   - the repository marks it (`security` in Gitta's settings: symbols like `billing.check_limits` or paths like `app/auth/*`).
//!
//! It is WEAKENED when, between base and new:
//!   - it is removed or renamed (named or marked checks, and code that carried a control),
//!   - a control is removed from it (`@login_required`, `[Authorize]`, `@PreAuthorize`, `requireAuth` ...),
//!   - an opener is added to it (`@csrf_exempt`, `[AllowAnonymous]`, `verify=False`, `InsecureSkipVerify: true` ...),
//!   - a check (by name or mark) gains an early allow (`return True`, `return nil`, `next()` ...) or loses a refusal (`raise`, `throw` ...),
//!   - a parameter that sets strictness (`strict`, `verify`, `secure` ...) defaults to false where it was true.
//!
//! Deterministic text rules on the items the meaning analysis already parsed: no new parse, no repository scan.
//! Comment lines are ignored, so a control that is commented out counts as removed.

use crate::{Item, Key, Lang};
use serde::Serialize;
use std::sync::RwLock;

/// What happened to a security control, in words for the agents and people who read the warning.
#[derive(Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Note {
    /// Verb phrase before the symbol: "removed the login check (`@login_required`) from".
    pub what: String,
    /// What code that relies on it may now do: "Your code may now run without a logged-in user."
    pub risk: String,
}

/// Kinds of control, with what code that relies on one may now do once it is gone.
#[derive(Clone, Copy)]
enum Cat { Login, Permission, Csrf, RateLimit, Validation, Sanitise, Tls, Other }

fn risk(c: Cat) -> &'static str {
    match c {
        Cat::Login => "Your code may now run without a logged-in user.",
        Cat::Permission => "Your code may now run for users who do not have the permission.",
        Cat::Csrf => "Your code may now run on requests forged by another site (no CSRF check).",
        Cat::RateLimit => "Your code may now run without a rate limit.",
        Cat::Validation => "Your code may now run on input that is not validated.",
        Cat::Sanitise => "Your code may now handle input that is not sanitised or escaped.",
        Cat::Tls => "Your code may now connect without checking certificates.",
        Cat::Other => "Your code may now run without this security check.",
    }
}

use Cat::*;

/// Well-known security controls: (text in the code, what it is, kind). Matched on code lines (not comments), at a
/// word start. Add entries here; keep them specific enough that ordinary code does not contain them.
const CONTROLS: &[(&str, &str, Cat)] = &[
    // Python: Django, Flask-Login, Flask-Security, Flask-JWT, FastAPI, Django REST framework, rate limits
    ("@login_required", "login check", Login),
    ("@fresh_login_required", "login check", Login),
    // Flask-HTTPAuth and friends: `@auth.login_required`, `@token_auth.login_required(role="admin")`
    (".login_required", "login check", Login),
    ("@permission_classes(", "permission check", Permission),
    ("@staff_member_required", "staff check", Permission),
    ("@permission_required", "permission check", Permission),
    ("@user_passes_test", "user check", Permission),
    ("@roles_required", "role check", Permission),
    ("@roles_accepted", "role check", Permission),
    ("@auth_required", "login check", Login),
    ("@requires_auth", "login check", Login),
    ("@jwt_required", "token check", Login),
    ("@csrf_protect", "CSRF check", Csrf),
    ("@ratelimit", "rate limit", RateLimit),
    ("@limiter.limit", "rate limit", RateLimit),
    ("LoginRequiredMixin", "login check", Login),
    ("PermissionRequiredMixin", "permission check", Permission),
    ("Depends(get_current_user", "login dependency", Login),
    ("Depends(get_current_active_user", "login dependency", Login),
    ("Depends(require_", "security dependency", Permission),
    ("Depends(verify_", "security dependency", Permission),
    ("Security(", "security dependency", Login),
    ("IsAuthenticated", "login permission class", Login),
    ("IsAdminUser", "admin permission class", Permission),
    ("DjangoModelPermissions", "permission class", Permission),
    // TypeScript / JavaScript: NestJS guards, Express middleware, Passport, csurf, rate limits
    ("@UseGuards(", "guard", Permission),
    ("AuthGuard(", "login guard", Login),
    ("@Roles(", "role check", Permission),
    ("@Authorized(", "login check", Login),
    ("requireAuth", "login middleware", Login),
    ("requireLogin", "login middleware", Login),
    ("requireRole", "role middleware", Permission),
    ("requireAdmin", "admin middleware", Permission),
    ("ensureAuthenticated", "login middleware", Login),
    ("ensureLoggedIn", "login middleware", Login),
    ("isAuthenticated", "login middleware", Login),
    ("passport.authenticate(", "login middleware", Login),
    ("csrfProtection", "CSRF middleware", Csrf),
    ("csurf(", "CSRF middleware", Csrf),
    ("rateLimit(", "rate limit", RateLimit),
    ("@Throttle(", "rate limit", RateLimit),
    // Java / Kotlin: Spring Security, Jakarta, Bean Validation, Ktor
    ("@PreAuthorize", "permission check", Permission),
    ("@PostAuthorize", "permission check", Permission),
    ("@Secured", "permission check", Permission),
    ("@RolesAllowed", "role check", Permission),
    ("@Validated", "input validation", Validation),
    ("@Valid", "input validation", Validation),
    ("authenticate(", "login check", Login),
    // C#: ASP.NET Core
    ("[Authorize", "login check", Login),
    ("[ValidateAntiForgeryToken", "CSRF check", Csrf),
    ("[AutoValidateAntiforgeryToken", "CSRF check", Csrf),
    ("[EnableRateLimiting", "rate limit", RateLimit),
    (".RequireAuthorization(", "login check", Login),
    // Go: common middleware names (chi, gorilla, echo, gin)
    ("RequireAuth", "login middleware", Login),
    ("AuthMiddleware", "login middleware", Login),
    ("authMiddleware", "login middleware", Login),
    ("jwtauth.Verifier", "token check", Login),
    ("jwtauth.Authenticator", "token check", Login),
    ("csrf.Protect", "CSRF middleware", Csrf),
    ("middleware.BasicAuth", "login middleware", Login),
    ("httprate.", "rate limit", RateLimit),
    // Rust: axum / tower / actix
    ("RequireAuthorizationLayer", "login check", Login),
    ("from_fn(auth", "login middleware", Login),
    ("from_fn(require_", "security middleware", Permission),
    // C and C++: certificate checks switched on (OpenSSL, wolfSSL, mbed TLS)
    ("SSL_VERIFY_PEER", "certificate check", Tls),
    ("MBEDTLS_SSL_VERIFY_REQUIRED", "certificate check", Tls),
];

/// Openers: adding one switches a control off for that code.
const OPENERS: &[(&str, &str, Cat)] = &[
    ("@csrf_exempt", "added a CSRF exemption (`@csrf_exempt`) to", Csrf),
    ("AllowAny", "opened to everyone (`AllowAny`)", Login),
    ("[AllowAnonymous", "allowed anonymous access (`[AllowAnonymous]`) to", Login),
    ("@AllowAnonymous", "allowed anonymous access (`@AllowAnonymous`) to", Login),
    ("@PermitAll", "opened to everyone (`@PermitAll`)", Login),
    ("@Public(", "made public (`@Public()`)", Login),
    ("@SkipThrottle", "removed the rate limit (`@SkipThrottle`) from", RateLimit),
    ("[DisableRateLimiting", "removed the rate limit (`[DisableRateLimiting]`) from", RateLimit),
    ("[IgnoreAntiforgeryToken", "added a CSRF exemption (`[IgnoreAntiforgeryToken]`) to", Csrf),
    (".permitAll()", "opened to everyone (`permitAll()`)", Login),
    ("verify=False", "switched off certificate checks (`verify=False`) in", Tls),
    ("CERT_NONE", "switched off certificate checks (`CERT_NONE`) in", Tls),
    ("check_hostname = False", "switched off host name checks in", Tls),
    ("InsecureSkipVerify: true", "switched off certificate checks (`InsecureSkipVerify`) in", Tls),
    ("rejectUnauthorized: false", "switched off certificate checks (`rejectUnauthorized: false`) in", Tls),
    ("NODE_TLS_REJECT_UNAUTHORIZED", "switched off certificate checks in", Tls),
    ("danger_accept_invalid_certs(true)", "switched off certificate checks in", Tls),
    ("ServerCertificateCustomValidationCallback", "replaced the certificate check in", Tls),
    ("mark_safe(", "turned off HTML escaping (`mark_safe`) in", Sanitise),
    ("dangerouslySetInnerHTML", "turned off HTML escaping (`dangerouslySetInnerHTML`) in", Sanitise),
    ("|safe", "turned off HTML escaping (`|safe`) in", Sanitise),
    ("SSL_VERIFY_NONE", "switched off certificate checks (`SSL_VERIFY_NONE`) in", Tls),
    ("MBEDTLS_SSL_VERIFY_NONE", "switched off certificate checks (`MBEDTLS_SSL_VERIFY_NONE`) in", Tls),
    ("CURLOPT_SSL_VERIFYPEER, 0", "switched off certificate checks (`CURLOPT_SSL_VERIFYPEER`) in", Tls),
    ("CURLOPT_SSL_VERIFYHOST, 0", "switched off host name checks (`CURLOPT_SSL_VERIFYHOST`) in", Tls),
];

/// Names of checks: the name, lower case without `_`, starts with one of these.
const NAMES: &[(&str, Cat)] = &[
    ("checkperm", Permission), ("haspermission", Permission), ("hasperm", Permission), ("hasrole", Permission), ("hasaccess", Permission),
    ("checkaccess", Permission), ("canaccess", Permission), ("isadmin", Permission), ("isallowed", Permission), ("isauthorized", Permission),
    ("isauthorised", Permission), ("authorize", Permission), ("authorise", Permission), ("requirerole", Permission), ("requirepermission", Permission),
    ("requireadmin", Permission), ("isauthenticated", Login), ("authenticate", Login), ("checkauth", Login), ("requireauth", Login),
    ("requirelogin", Login), ("ensureauth", Login), ("loginrequired", Login), ("permissionrequired", Permission),
    ("verify", Other), ("validate", Validation), ("sanitize", Sanitise), ("sanitise", Sanitise), ("escape", Sanitise),
    ("csrf", Csrf), ("ratelimit", RateLimit), ("throttle", RateLimit), ("checklimit", RateLimit),
    // Short forms (`AUTHCHK`, `SECCHK`, `CHECK-AUTHORITY`): `-` is dropped like `_`.
    ("authchk", Permission), ("chkauth", Permission), ("chkperm", Permission), ("chkaccess", Permission), ("secchk", Permission),
    ("chksec", Permission), ("checksec", Permission), ("validuser", Login), ("checkuser", Login), ("chkuser", Login),
];

/// Lines that let the caller through at once.
const ALLOWS: &[&str] = &["return True", "return true", "return nil", "return Ok(())", "return Ok(true)", "return null", "return;", "next()",
    "return next(", "next.ServeHTTP(", "c.Next()", "ctx.Next()", "await next(", "return await next("];
/// Lines that refuse.
const DENIES: &[&str] = &["raise ", "throw ", "abort(", "panic!(", "Err(", "PermissionDenied", "Forbidden", "Unauthorized", "http.Error(",
    "return False", "return false", "status(401", "status(403", "StatusUnauthorized", "StatusForbidden", "HttpResponseForbidden",
    // C and C++: an error code or an exception
    "return -1", "-EPERM", "-EACCES", "EACCES", "EPERM", "std::runtime_error", "abort()"];
/// Parameter names that set how strict a check is.
const STRICT: &[&str] = &["strict", "verify", "secure", "enforce", "required", "validate", "check", "safe", "csrf", "auth"];

/// Symbols and paths the repository marks as security-relevant (set once per run from Gitta's settings).
static MARKED: RwLock<Vec<String>> = RwLock::new(Vec::new());

/// Set the repository's own security symbols (`billing.check_limits`, `shop.auth.*`) and paths (`app/auth/*`).
pub fn set_marked(patterns: Vec<String>) { if let Ok(mut m) = MARKED.write() { *m = patterns; } }

/// `*` matches any text, `?` one character.
pub(crate) fn glob(p: &str, s: &str) -> bool {
    let (p, s) = (p.as_bytes(), s.as_bytes());
    let (mut i, mut j, mut star, mut mark) = (0, 0, None, 0);
    while j < s.len() {
        if i < p.len() && (p[i] == b'?' || p[i] == s[j]) { i += 1; j += 1; }
        else if i < p.len() && p[i] == b'*' { star = Some(i); mark = j; i += 1; }
        else if let Some(st) = star { i = st + 1; mark += 1; j = mark; }
        else { return false; }
    }
    while i < p.len() && p[i] == b'*' { i += 1; }
    i == p.len()
}

fn is_path_pattern(p: &str) -> bool { p.contains('/') || crate::lang_of(p.trim_end_matches('*')).is_some() }

/// The full names of a key in `module`: `shop.views.export_orders`, `shop.views.OrderView.get` and its class.
fn full_names(module: &str, key: &Key) -> Vec<String> {
    // C and C++ symbols are named as written (`ssl_verify_peer`, `net::Tls::check`), without their shared module.
    let c = crate::c_cpp::display(module, "").is_some();
    let q = |n: &str| if module.is_empty() || c { n.to_string() } else { format!("{module}.{n}") };
    match key {
        Key::Member(c, inner) => inner.bound().map(|n| vec![q(&format!("{c}.{n}")), q(c)]).unwrap_or_default(),
        k => k.bound().map(|n| vec![q(n)]).unwrap_or_default(),
    }
}

fn name_of(key: &Key) -> Option<&str> { match key { Key::Member(_, inner) => inner.bound(), k => k.bound() } }

fn name_cat(name: &str) -> Option<Cat> {
    // A C++ name is judged by its last part: `net::Tls::verify_peer` is a check named `verify_peer`.
    let name = name.rsplit("::").next().unwrap_or(name);
    let n: String = name.trim_start_matches('_').chars().filter(|c| *c != '_' && *c != '-').flat_map(char::to_lowercase).collect();
    NAMES.iter().find(|(p, _)| n.starts_with(p)).map(|(_, c)| *c)
}

/// Marked by the repository, or named like a check: the body rules and removal apply.
fn named(path: &str, module: &str, key: &Key) -> Option<Cat> {
    let m = MARKED.read().ok();
    let marked = m.iter().flat_map(|v| v.iter()).any(|p| if is_path_pattern(p) { glob(p, path) } else { full_names(module, key).iter().any(|f| glob(p, f)) });
    let mut by_name = name_of(key).and_then(name_cat);
    // C names carry their library's prefix (`ossl_verify_peer`, `Curl_auth_allowed_to_host`): the check is named after it.
    if by_name.is_none() && crate::c_cpp::display(module, "").is_some() {
        if let Some(n) = name_of(key) { by_name = n.match_indices('_').take(2).find_map(|(i, _)| name_cat(&n[i + 1..])); }
    }
    if marked { Some(by_name.unwrap_or(Other)) } else { by_name }
}

/// Code lines of an item: comment lines left out (`#` is a comment in Python only; elsewhere it starts an attribute).
fn code(text: &str, lang: Lang) -> Vec<&str> {
    text.lines().map(str::trim).filter(|l| !(l.starts_with("//") || l.starts_with("/*") || l.starts_with('*') || (lang == Lang::Python && l.starts_with('#')))).collect()
}

/// Occurrences of `m` in the lines, at a word start (so `Security(` does not match `NetworkSecurity(`).
fn count(lines: &[&str], m: &str) -> usize {
    let word = m.chars().next().map(|c| c.is_alphanumeric()).unwrap_or(false);
    // One searcher for all lines: setting one up per line cost more than the search on large changes.
    let f = memchr::memmem::Finder::new(m);
    lines.iter().map(|l| f.find_iter(l.as_bytes()).filter(|i| !word || !l[..*i].chars().next_back().map(|c| c.is_alphanumeric() || c == '_').unwrap_or(false)).count()).sum()
}

fn lines_with(lines: &[&str], needles: &[&str]) -> usize { lines.iter().filter(|l| needles.iter().any(|n| l.contains(n))).count() }

/// Parameter name -> default value, from a normalised signature `(a, verify=True) -> x`.
fn defaults(sig: &str) -> Vec<(String, String)> {
    let params = sig.split(" -> ").next().unwrap_or("");
    params.trim_matches(|c| c == '(' || c == ')').split(',').filter_map(|p| {
        let (l, v) = p.split_once('=')?;
        let name = l.split(':').next().unwrap_or("").trim().trim_start_matches(['*', '$']).rsplit(' ').next().unwrap_or("").to_lowercase();
        Some((name, v.trim().to_lowercase()))
    }).collect()
}

/// Does the item carry a well-known control (so changes to it matter for security)?
pub(crate) fn has_control(item: &Item, lang: Lang) -> bool {
    let l = code(&item.text, lang);
    CONTROLS.iter().any(|(m, _, _)| count(&l, m) > 0)
}

/// Security-relevant: named or marked as a check, or carrying a control.
pub(crate) fn relevant(path: &str, module: &str, key: &Key, item: &Item, lang: Lang) -> bool {
    named(path, module, key).is_some() || has_control(item, lang)
}

/// How the change from `old` to `new` (None = removed or renamed) weakens a security control, if it does.
pub(crate) fn weakening(path: &str, module: &str, key: &Key, old: &Item, new: Option<&Item>, lang: Lang) -> Option<Note> {
    let cat = named(path, module, key);
    let ol = code(&old.text, lang);
    let note = |what: String, c: Cat| Some(Note { what, risk: risk(c).into() });
    let Some(new) = new else {
        let c = cat.or_else(|| CONTROLS.iter().find(|(m, _, _)| count(&ol, m) > 0).map(|(_, _, c)| *c))?;
        return note("removed".into(), c);
    };
    if new.text == old.text { return None; }
    let nl = code(&new.text, lang);
    for (m, what, c) in CONTROLS {
        if count(&nl, m) < count(&ol, m) { return note(format!("removed the {what} (`{}`) from", m.trim_end_matches('(')), *c); }
    }
    if let (Some(a), Some(b)) = (&old.sig, &new.sig) {
        let (da, db) = (defaults(a), defaults(b));
        for (n, v) in &da {
            if !STRICT.iter().any(|s| n.contains(s)) || !matches!(v.as_str(), "true" | "1") { continue; }
            if db.iter().any(|(m, w)| m == n && matches!(w.as_str(), "false" | "0" | "none" | "nil" | "null")) {
                return note(format!("made `{n}` default to off in"), cat.unwrap_or(Other));
            }
        }
    }
    for (m, what, c) in OPENERS {
        if count(&nl, m) > count(&ol, m) { return note(what.to_string(), *c); }
    }
    let c = cat?;
    // The first line is the signature.
    if lines_with(nl.get(1..).unwrap_or(&[]), ALLOWS) > lines_with(ol.get(1..).unwrap_or(&[]), ALLOWS) { return note("made it let callers through early, before the check, in".into(), c); }
    if lines_with(&nl, DENIES) < lines_with(&ol, DENIES) { return note("removed a refusal (an error, raise or throw) from".into(), c); }
    None
}

/// One agent's own weakenings, for the safety-net guard: a person approves these before they land, like weakened
/// tests. Removing or renaming a check is left to the cross-agent hazard (a refactor is not a weakening by itself).
pub fn weakened(base: &crate::Snapshot, new: &crate::Snapshot, paths: &[String]) -> Vec<crate::guard::Finding> {
    let mut out = vec![];
    for p in paths {
        let (Some(b), Some(n)) = (base.get(p), new.get(p)) else { continue };
        if b == n { continue; }
        let (Some(bi), Some(ni)) = (crate::analyze_file(p, b), crate::analyze_file(p, n)) else { continue };
        let Some(lang) = bi.lang else { continue };
        let mut found: Vec<(Key, String)> = vec![];
        for (k, item) in &bi.items {
            let Some(x) = ni.items.get(k) else { continue };
            let Some(note) = weakening(p, &bi.module, k, item, Some(x), lang) else { continue };
            let Some(sym) = full_names(&bi.module, k).into_iter().next() else { continue };
            found.push((k.clone(), format!("{} `{sym}`. {}", note.what, note.risk.replace("Your code", "Code that calls it"))));
        }
        // A class is reported through its members when one of them is.
        let members: Vec<String> = found.iter().filter_map(|(k, _)| if let Key::Member(c, _) = k { Some(c.clone()) } else { None }).collect();
        for (k, detail) in found {
            if matches!(&k, Key::Class(c) if members.contains(c)) { continue; }
            out.push(crate::guard::Finding { kind: "SECURITY_WEAKENED".into(), path: p.clone(), detail });
        }
    }
    out
}

/// Entries of a `.gitta-security` file as `section: entry` (marked checks have no section), for the guard.
pub fn entries(text: &str) -> Vec<String> {
    let mut sec = String::new();
    text.lines().filter_map(|line| {
        let t = line.split('#').next().unwrap_or("").trim();
        if t.starts_with('[') && t.ends_with(']') { sec = t[1..t.len() - 1].trim().to_ascii_lowercase(); return None; }
        (!t.is_empty()).then(|| if sec.is_empty() { t.to_string() } else { format!("[{sec}] {t}") })
    }).collect()
}

/// Only the marked checks of a `.gitta-security` file: the lines before the first section.
pub fn marks(text: &str) -> Vec<String> { entries(text).into_iter().filter(|e| !e.starts_with('[')).collect() }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn globs_and_names() {
        assert!(glob("app/auth/*", "app/auth/views.py") && glob("billing.check_limits", "billing.check_limits") && !glob("billing.x", "billing.y"));
        assert!(glob("shop.auth.*", "shop.auth.Policy.allows"));
        assert!(name_cat("check_permission").is_some() && name_cat("isAdmin").is_some() && name_cat("sanitizeHtml").is_some() && name_cat("total").is_none());
        assert_eq!(count(&["x = NetworkSecurity(a)"], "Security("), 0);
        assert_eq!(count(&["@login_required"], "@login_required"), 1);
        assert_eq!(defaults("(url, verify=True) -> Response"), vec![("url".to_string(), String::new()), ("verify".to_string(), "true".to_string())].into_iter().filter(|(_, v)| !v.is_empty()).collect::<Vec<_>>());
    }
}
