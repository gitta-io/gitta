//! Security layer 1: risky code that one agent's own change newly adds.
//!
//! Text rules on the lines an agent added (a line counts as added when the new file has it more often than the
//! base, ignoring indentation), with the rest of the new file as context. A pattern that already existed is never
//! reported again; a line that was only moved or re-indented is not new. No parse, no repository scan: a few
//! string searches per added line of the agent's changed files.
//!
//!   sql-text            SQL text built with formatting or concatenation, passed to an execute or query call
//!   shell-text          a shell command built from variables (system calls, shell=True, exec with templates ...)
//!   eval-text           eval / exec of a string that is not a literal
//!   unsafe-deserialise  loaders that can run code (pickle, yaml without a safe loader, Java and .NET object streams)
//!   tls-off             certificate or host name checks turned off
//!   cors-credentials    any origin allowed together with credentials
//!   csrf-off            CSRF protection turned off
//!   weak-password-hash  MD5 or SHA-1 on a password
//!   hardcoded-secret    a secret-named variable set to a literal (credentials the safety net finds are left to it)
//!   secret-in-log       a secret-named variable written to a log, the console or stdout
//!   unsafe-c-buffer     C and C++: a copy with no size limit (strcpy, strcat, sprintf, gets, scanf `%s`), or memcpy /
//!                       memmove with a length that is not a sizeof, a constant or a minimum, nor compared, set from a sizeof
//!                       or used to allocate in the lines before
//!   format-string       C and C++: a variable passed as the format of printf, fprintf, sprintf, snprintf, syslog ...
//!
//! Python, TypeScript / JavaScript, Go, Java, Kotlin, C#, Rust, C and C++. For C and C++, shell-text covers system(),
//! popen() and the exec family with a command that is not a literal, tls-off covers OpenSSL / wolfSSL / mbed TLS
//! `VERIFY_NONE` and curl's `CURLOPT_SSL_VERIFYPEER` / `VERIFYHOST` set to 0. Mainframe code has its own rules
//! (mainframe_security.rs, including dynamic SQL in COBOL). Weakened controls between agents are layer 2 (security.rs).
//!
//! A reviewed line is allowed with `gitta: allow <rule-id> <reason>` in a comment on that line, or with an entry
//! `<rule-id> <path glob> <reason>` in the `[allow]` section of `.gitta-security`. An allow that the change itself
//! adds still shows in the land output and the audit trail, and does not clear a finding that needs a person.

use crate::Snapshot;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;

/// The rules, in the words of the warning: (id, severity, what the line does, the safer pattern).
pub const RULES: &[(&str, &str, &str, &str)] = &[
    ("sql-text", "medium", "builds SQL from text and variables.", "Use a parameterised query."),
    ("shell-text", "high", "runs a shell command built from text and variables.", "Pass the program and its arguments as a list, without a shell."),
    ("eval-text", "medium", "runs code from a string built at run time.", "Parse the data, or call the code directly."),
    ("unsafe-deserialise", "high", "loads objects with a loader that can run code from the data.", "Use a safe loader or a plain data format such as JSON."),
    ("tls-off", "high", "turns off certificate checks.", "Keep verification on; add an internal CA to the trust store if needed."),
    ("cors-credentials", "medium", "lets any website send requests with the user's cookies.", "List the origins that may call it."),
    ("csrf-off", "medium", "turns off CSRF protection.", "Keep it on and send the token from the client."),
    ("weak-password-hash", "medium", "hashes a password with a fast hash.", "Use bcrypt, scrypt or Argon2."),
    ("hardcoded-secret", "medium", "puts a secret in the code.", "Read it from configuration or a secret store."),
    ("secret-in-log", "medium", "writes a secret to a log or the console.", "Log that it is set, never its value."),
    ("unsafe-c-buffer", "medium", "copies into a buffer without a size limit.", "Use a bounded call (snprintf, strlcpy, fgets) or check the length first."),
    ("format-string", "medium", "uses a variable as the format string.", "Write the format as a literal, such as printf(\"%s\", text)."),
];

/// One finding on an added line.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Note {
    pub path: String,
    pub line: usize,
    pub rule: String,
    pub severity: String,
    /// What the line does: "builds SQL from text and variables."
    pub text: String,
    /// The safer pattern: "Use a parameterised query."
    pub safer: String,
    /// The reason of an allow that covers this line.
    #[serde(default, skip_serializing_if = "Option::is_none")] pub allowed: Option<String>,
    /// The allow comes from this change (an inline marker, or an `[allow]` entry the change adds).
    #[serde(default)] pub allow_new: bool,
    /// Stable across line shifts: the path, the rule and the line's text.
    pub key: String,
}

#[derive(Clone)]
struct Allow { rule: String, glob: String, reason: String }
static ALLOWS: RwLock<Vec<Allow>> = RwLock::new(Vec::new());

fn parse_allows(text: &str) -> Vec<Allow> {
    crate::security::entries(text).iter().filter_map(|e| e.strip_prefix("[allow] ")).filter_map(|e| {
        let mut w = e.splitn(3, char::is_whitespace);
        Some(Allow { rule: w.next()?.into(), glob: w.next()?.into(), reason: w.next().unwrap_or("").trim().into() })
    }).collect()
}

/// The `[allow]` entries of the repository's committed `.gitta-security` (the main checkout's copy).
pub fn load(text: &str) { if let Ok(mut g) = ALLOWS.write() { *g = parse_allows(text); } }

#[derive(Clone, Copy, PartialEq)]
enum L { Py, Js, Go, Java, Kotlin, Cs, Rs, C }

fn lang(path: &str) -> Option<L> {
    let e = path.rsplit_once('.').map(|x| x.1.to_ascii_lowercase()).unwrap_or_default();
    Some(match e.as_str() {
        "py" | "pyw" => L::Py, "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts" => L::Js, "go" => L::Go,
        "java" => L::Java, "kt" | "kts" => L::Kotlin, "cs" => L::Cs, "rs" => L::Rs,
        "c" | "h" | "cc" | "cpp" | "cxx" | "c++" | "hpp" | "hh" | "hxx" | "h++" | "ipp" | "inl" | "tpp" => L::C, _ => return None,
    })
}

pub fn applies(path: &str) -> bool { lang(path).is_some() }

fn fnv(s: &str) -> String { let mut h: u64 = 0xcbf29ce484222325; for b in s.bytes() { h ^= b as u64; h = h.wrapping_mul(0x100000001b3); } format!("{h:016x}") }
fn ident(c: char) -> bool { c.is_ascii_alphanumeric() || c == '_' }
fn squash(s: &str) -> String { s.chars().filter(|c| !c.is_whitespace()).collect() }
/// Lower case, no white space, one kind of quote: for matching settings like `origin: '*'`.
fn flat(s: &str) -> String { squash(s).to_ascii_lowercase().replace('\'', "\"") }

fn comment(t: &str, l: L) -> bool {
    if l == L::Py { t.starts_with('#') } else { t.starts_with("//") || t.starts_with("/*") || t.starts_with("* ") || t == "*" }
}

/// `needle` at a word start: not right after a letter, digit, `_`, or (with `no_dot`) a `.`.
fn at_word(line: &str, needle: &str, no_dot: bool) -> Option<usize> {
    let mut from = 0;
    while let Some(i) = line[from..].find(needle) {
        let at = from + i;
        let prev = line[..at].chars().next_back();
        if !prev.map(|c| ident(c) || (no_dot && c == '.')).unwrap_or(false) { return Some(at); }
        from = at + needle.len();
    }
    None
}

/// The first argument after the call at `at` starts with a plain string literal (no template or interpolation).
fn literal_arg(line: &str, at: usize) -> bool {
    let Some(p) = line[at..].find('(') else { return false };
    let rest = line[at + p + 1..].trim_start();
    let rest = rest.strip_prefix('r').or_else(|| rest.strip_prefix('b')).filter(|r| r.starts_with(['"', '\''])).unwrap_or(rest);
    match rest.chars().next() {
        Some('"') | Some('\'') => true,
        Some('`') => !rest.contains("${"),
        _ => false,
    }
}

/// The line builds a string from text and variables: formatting, interpolation or concatenation.
fn dynamic(line: &str, l: L) -> bool {
    let sq = squash(line);
    let concat = sq.contains("\"+") || sq.contains("+\"") || (l != L::Rs && (sq.contains("'+") || sq.contains("+'")));
    concat || match l {
        L::Py => line.contains(".format(") || line.contains("\" % ") || line.contains("' % ") || line.contains("\" %(") || line.contains("' %(")
            || ["f\"", "f'", "rf\"", "fr\"", "F\""].iter().any(|p| at_word(line, p, false).is_some()) && line.contains('{'),
        L::Js => line.contains('`') && line.contains("${"),
        L::Go => line.contains("Sprintf("),
        L::Java => line.contains("String.format(") || line.contains(".formatted("),
        L::Kotlin => line.contains("String.format(") || line.contains("${") || line.contains("\"$") || line.contains(" $"),
        L::Cs => line.contains("$\"") || line.contains("$@\"") || line.contains("@$\"") || line.contains("string.Format(") || line.contains("String.Format("),
        L::Rs => line.contains("format!("),
        L::C => ["sprintf(", "snprintf(", "asprintf(", "std::format(", "fmt::format(", "absl::StrCat(", "StrCat("].iter().any(|c| line.contains(c)) || (line.contains("<<") && line.contains('"')),
    }
}

/// Identifiers in code, and inside `{...}`, `${...}` and Kotlin `$name` in strings; plain string text is skipped.
fn code_idents(line: &str, l: L) -> Vec<(String, usize)> {
    let b: Vec<char> = line.chars().collect();
    // Byte offset of each char, so the positions returned are byte offsets into `line` (the caller slices `line`).
    let byte_at: Vec<usize> = { let mut a = Vec::with_capacity(b.len() + 1); let mut p = 0; for c in &b { a.push(p); p += c.len_utf8(); } a.push(p); a };
    let mut out = vec![];
    let (mut i, mut quote, mut depth) = (0usize, None::<char>, 0usize);
    let mut cur = String::new();
    let mut start = 0;
    let flush = |cur: &mut String, start: usize, out: &mut Vec<(String, usize)>| { if !cur.is_empty() { out.push((std::mem::take(cur), start)); } };
    while i < b.len() {
        let c = b[i];
        let in_code = quote.is_none() || depth > 0;
        if in_code && ident(c) { if cur.is_empty() { start = i; } cur.push(c); i += 1; continue; }
        flush(&mut cur, byte_at[start], &mut out);
        match quote {
            None => {
                if c == '"' || c == '`' || (c == '\'' && l != L::Rs) { quote = Some(c); }
                if l == L::Py && c == '#' { break; }
            }
            Some(q) if depth == 0 => {
                if c == '\\' { i += 1; }
                else if c == q { quote = None; }
                else if c == '{' { depth = 1; }
                else if c == '$' && b.get(i + 1).map(|x| x.is_ascii_alphabetic()).unwrap_or(false) && l == L::Kotlin { let j = i + 1; let mut k = j; while k < b.len() && ident(b[k]) { k += 1; } out.push((b[j..k].iter().collect(), byte_at[j])); i = k; continue; }
            }
            Some(_) => { if c == '{' { depth += 1; } else if c == '}' { depth -= 1; } }
        }
        i += 1;
    }
    flush(&mut cur, byte_at[start], &mut out);
    out
}

const SECRET_WORDS: &[&str] = &["password", "passwd", "secret", "apikey", "api_key", "privatekey", "private_key", "accesskey", "access_key", "credential", "authtoken", "auth_token"];
const NOT_SECRET: &[&str] = &["count", "len", "length", "size", "url", "uri", "type", "name", "id", "field", "header", "path", "file", "env", "var", "prefix",
    "expiry", "expires", "expiresat", "expiresin", "policy", "hash", "hashed", "digest", "salt", "label", "hint", "reset", "required", "set", "valid", "ok", "error", "regex", "pattern", "manager", "store"];

/// Names like `password`, `api_key`, `accessToken` or `token`; not `token_count`, `has_password`, `tokenizer`.
fn secret_name(n: &str) -> bool {
    let lo = n.to_ascii_lowercase();
    if lo.starts_with("is_") || lo.starts_with("has_") || (lo.starts_with("is") && n.chars().nth(2).map(|c| c.is_uppercase()).unwrap_or(false)) { return false; }
    let flatn = lo.replace('_', "");
    if NOT_SECRET.iter().any(|w| flatn.ends_with(w) && !SECRET_WORDS.iter().any(|s| flatn == s.replace('_', ""))) { return false; }
    SECRET_WORDS.iter().any(|w| lo.contains(w)) || flatn == "token" || flatn.ends_with("token") || flatn == "pwd"
}

fn password_name(n: &str) -> bool { let lo = n.to_ascii_lowercase(); lo.contains("password") || lo.contains("passwd") || lo == "pwd" || lo.ends_with("_pwd") }

const SQL_CALLS: &[&str] = &[".execute(", ".executemany(", ".executescript(", ".raw(", ".query(", ".Query(", ".QueryRow(", ".QueryContext(", ".QueryRowContext(",
    ".Exec(", ".ExecContext(", "executeQuery(", "executeUpdate(", ".createQuery(", ".createNativeQuery(", "prepareStatement(", "SqlCommand(", "FromSqlRaw(",
    "ExecuteSqlRaw(", "sqlx::query(", "query_as(", ".batch_execute(", "text(", ".sql(", "$queryRawUnsafe(", "$executeRawUnsafe(", ".unsafe(", ".Raw(", ".exec(", ".prepare(", ".queryForList(", ".queryForObject(", ".queryForMap(", ".queryForRowSet("];

fn sql_text(lo: &str) -> bool {
    let q = lo.find(['"', '\'', '`']).unwrap_or(usize::MAX);
    let at = |k: &str| lo.find(k).filter(|i| *i > q);
    (at("select ").is_some() && lo.contains(" from")) || at("insert into").is_some() || (at("update ").is_some() && lo.contains(" set ")) || at("delete from").is_some()
}

fn sql_call(line: &str) -> bool { SQL_CALLS.iter().any(|c| line.contains(c)) }

/// `name = ...`, `const name = ...`, `String name = ...`, `name := ...`: the assigned name.
fn assigned(line: &str) -> Option<String> {
    let t = line.trim_start();
    let plain = |i: &usize| !matches!(t[i + 1..].chars().next(), Some('=') | Some('>')) && !matches!(t[..*i].chars().next_back(), Some('!') | Some('<') | Some('>') | Some('=') | Some('+'));
    let i = t.find(":=").or_else(|| t.find('=').filter(plain))?;
    let left = t[..i].trim_end().trim_end_matches(|c: char| c == ':' || c.is_whitespace());
    let left = left.split(':').next().unwrap_or(left).trim_end();   // `query: str = ...`
    let name: String = left.chars().rev().take_while(|c| ident(*c)).collect::<Vec<_>>().into_iter().rev().collect();
    (!name.is_empty() && !name.chars().next().unwrap().is_ascii_digit()).then_some(name)
}

fn has_word(line: &str, w: &str) -> bool {
    let mut from = 0;
    while let Some(i) = line[from..].find(w) {
        let at = from + i; let end = at + w.len();
        if !line[..at].chars().next_back().map(ident).unwrap_or(false) && !line[end..].chars().next().map(ident).unwrap_or(false) { return true; }
        from = end;
    }
    false
}

fn shell(line: &str, ctx: &str, file: &str, l: L) -> bool {
    let dynl = dynamic(ctx, l);
    match l {
        L::Py => {
            for c in ["os.system(", "os.popen(", "subprocess.getoutput(", "subprocess.getstatusoutput(", "commands.getoutput("] {
                if let Some(at) = line.find(c) { if dynl || !literal_arg(line, at) { return true; } }
            }
            ["subprocess.run(", "subprocess.call(", "subprocess.Popen(", "subprocess.check_output(", "subprocess.check_call(", "Popen("].iter()
                .filter_map(|c| line.find(c)).next().map(|at| squash(ctx).contains("shell=True") && (dynl || !literal_arg(line, at))).unwrap_or(false)
        }
        L::Js => {
            if !file.contains("child_process") { return false; }
            for c in ["exec(", "execSync("] {
                let mut from = 0;
                while let Some(i) = line[from..].find(c) {
                    let at = from + i; from = at + c.len();
                    let before = &line[..at];
                    let prev = before.chars().next_back();
                    let ok = match prev { Some('.') => ["child_process.", "cp.", "childProcess.", "proc."].iter().any(|r| before.ends_with(r)), Some(p) => !ident(p), None => true };
                    if ok && (dynl || !literal_arg(line, at)) { return true; }
                }
            }
            flat(ctx).contains("shell:true") && (line.contains("spawn(") || line.contains("spawnSync(")) && dynl
        }
        L::Go => {
            if !(line.contains("exec.Command(") || line.contains("exec.CommandContext(")) { return false; }
            let s = squash(ctx);
            let Some(i) = ["\"-c\",", "\"/c\",", "\"/C\","].iter().filter_map(|m| s.find(m).map(|i| i + m.len())).next() else { return false };
            let rest = &s[i..];
            !(rest.starts_with('"') || rest.starts_with('`')) || rest.split(')').next().unwrap_or("").contains('+') || rest.starts_with("fmt.Sprintf")
        }
        L::Java | L::Kotlin => (line.contains(".exec(") && (ctx.contains("Runtime") || ctx.contains("getRuntime")) && dynl)
            || (line.contains("ProcessBuilder(") && squash(ctx).contains("\"-c\"") && dynl),
        L::Cs => (line.contains("Process.Start(") && dynl)
            || (squash(line).contains("Arguments=") && dynl && ["\"cmd.exe\"", "\"cmd\"", "\"/bin/sh\"", "\"/bin/bash\"", "\"bash\"", "\"sh\"", "\"powershell\"", "\"pwsh\""].iter().any(|s| file.contains(s))),
        L::C => {
            for c in ["system(", "popen(", "_popen(", "_wsystem(", "_wpopen("] {
                if let Some(at) = at_word(line, c, true) { if !c_literal_arg(line, at) { return true; } }
            }
            for c in ["execl(", "execlp(", "execle(", "execv(", "execvp(", "execvpe(", "execve(", "posix_spawn(", "posix_spawnp("] {
                let Some(at) = at_word(line, c, true) else { continue };
                let args = c_args(&ctx[ctx.find(&line[at..]).unwrap_or(0)..]).unwrap_or_default();
                // posix_spawn names the program in its second argument.
                let prog = if c.starts_with("posix_spawn") { args.get(1) } else { args.first() };
                let via_shell = args.iter().position(|a| a == "\"-c\"").and_then(|i| args.get(i + 1)).map(|a| !a.starts_with('"')).unwrap_or(false);
                if via_shell || prog.map(|p| !p.starts_with('"') && !p.starts_with("L\"")).unwrap_or(false) { return true; }
            }
            false
        }
        L::Rs => {
            let s = squash(ctx);
            let shell_cmd = ["Command::new(\"sh\")", "Command::new(\"bash\")", "Command::new(\"/bin/sh\")", "Command::new(\"/bin/bash\")", "Command::new(\"cmd\")"].iter().any(|c| s.contains(c) || squash(file).contains(c));
            shell_cmd && (s.contains("\"-c\"") || s.contains("\"/C\"")) && (line.contains("format!(") || line.contains("&format!(")) && (line.contains(".arg(") || line.contains(".args("))
        }
    }
}

fn evals(line: &str, l: L) -> bool {
    let calls: &[&str] = match l { L::Py => &["eval(", "exec("], L::Js => &["eval(", "new Function("], L::Cs => &["CSharpScript.EvaluateAsync(", "CSharpScript.RunAsync("], _ => &[] };
    if l == L::Py && line.trim_start().starts_with("def ") { return false; }
    calls.iter().any(|c| at_word(line, c, !c.contains('.')).map(|at| !literal_arg(line, at) || dynamic(&line[at..], l)).unwrap_or(false))
}

fn deserialise(line: &str, ctx: &str, l: L) -> bool {
    let has = |xs: &[&str]| xs.iter().any(|x| at_word(line, x, false).is_some());
    match l {
        L::Py => has(&["pickle.loads(", "pickle.load(", "cPickle.load", "dill.load", "marshal.loads(", "marshal.load(", "jsonpickle.decode(", "yaml.unsafe_load(", "shelve.open("])
            || (line.contains("yaml.load(") && !["SafeLoader", "CSafeLoader", "BaseLoader"].iter().any(|s| ctx.contains(s))),
        L::Js => has(&["serialize.unserialize(", ".unserialize(", "funcster.deepDeserialize"]),
        L::Java | L::Kotlin => has(&["ObjectInputStream(", "XMLDecoder(", "enableDefaultTyping(", "activateDefaultTyping(", "SerializationUtils.deserialize("]),
        L::Cs => has(&["BinaryFormatter(", "NetDataContractSerializer(", "SoapFormatter(", "LosFormatter(", "ObjectStateFormatter(", "TypeNameHandling.All", "TypeNameHandling.Auto", "TypeNameHandling.Objects"]),
        L::Go | L::Rs | L::C => false,
    }
}

fn tls_off(line: &str, ctx: &str, l: L) -> bool {
    let s = squash(line);
    let c = squash(ctx);
    let has = |xs: &[&str]| xs.iter().any(|x| s.contains(x));
    match l {
        L::Py => has(&["verify=False", "_create_unverified_context", "CERT_NONE", "check_hostname=False", "_create_default_https_context=", "verify_ssl=False", "ssl_verify=False"]),
        L::Js => { let f = flat(line); f.contains("rejectunauthorized:false") || (f.contains("node_tls_reject_unauthorized") && (f.contains("\"0\"") || f.contains("=0"))) || f.contains("strictssl:false") || f.contains("checkserveridentity:()=>undefined") }
        L::Go => s.contains("InsecureSkipVerify:true"),
        L::Java | L::Kotlin => has(&["ALLOW_ALL_HOSTNAME_VERIFIER", "NoopHostnameVerifier", "TrustAllStrategy"]) || line.to_ascii_lowercase().contains("trustall")
            || (s.to_ascii_lowercase().contains("hostnameverifier") && (c.contains("->true") || c.contains("returntrue")))
            || c.find("checkServerTrusted(").map(|i| c[i..].split_once('{').map(|(_, b)| b.starts_with('}')).unwrap_or(false)).unwrap_or(false) && s.contains("checkServerTrusted("),
        L::Cs => has(&["DangerousAcceptAnyServerCertificateValidator"]) || (["ServerCertificateValidationCallback", "ServerCertificateCustomValidationCallback", "RemoteCertificateValidationCallback"].iter().any(|x| s.contains(x)) && (c.contains("=>true") || c.contains("returntrue"))),
        L::C => {
            let curl_off = ["CURLOPT_SSL_VERIFYPEER,", "CURLOPT_SSL_VERIFYHOST,", "CURLOPT_PROXY_SSL_VERIFYPEER,", "CURLOPT_PROXY_SSL_VERIFYHOST,"].iter()
                .any(|o| s.find(o).map(|i| { let v = &s[i + o.len()..]; ["0)", "0L)", "FALSE)", "false)", "0L,", "0,"].iter().any(|z| v.starts_with(z)) }).unwrap_or(false));
            // A choice between NONE and PEER is an option the program offers, not checks switched off.
            curl_off || has(&["MBEDTLS_SSL_VERIFY_NONE"]) || (has(&["SSL_VERIFY_NONE"]) && !has(&["SSL_VERIFY_PEER"]) && s.contains("set_verify("))
        }
        L::Rs => has(&["danger_accept_invalid_certs(true)", "danger_accept_invalid_hostnames(true)", "SslVerifyMode::NONE", "NoCertificateVerification", "dangerous().set_certificate_verifier("]),
    }
}

const CORS_ANY: &[&str] = &["origin:\"*\"", "origins=\"*\"", "origins=[\"*\"]", "allow_origins=[\"*\"]", "allowedorigins(\"*\")", "allowedoriginpatterns(\"*\")",
    "alloworigins:[]string{\"*\"}", "allowedorigins:[]string{\"*\"}", "allowallorigins:true", "allowanyorigin()", "cors_allow_all_origins=true", "cors_origin_allow_all=true",
    "access-control-allow-origin\",\"*\"", "access-control-allow-origin\":\"*\"", "access-control-allow-origin:*", "setisoriginallowed(_=>true)",
    "setisoriginallowed(origin=>true)", "origin:true", "@crossorigin(origins=\"*\"", "@crossorigin(\"*\"", "allow_any_origin()", "allow_origin(any)", "alloworiginfunc:func("];
const CORS_CRED: &[&str] = &["credentials:true", "supports_credentials=true", "allow_credentials=true", "allowcredentials:true", "allowcredentials(true)",
    "allowcredentials()", "cors_allow_credentials=true", "access-control-allow-credentials\",\"true\"", "access-control-allow-credentials\":\"true\"",
    "access-control-allow-credentials:true", "allowcredentials=\"true\"", "allow_credentials(true)"];

fn csrf_off(line: &str) -> bool {
    let f = flat(line);
    ["@csrf_exempt", "csrf.exempt", "wtf_csrf_enabled=false", "csrf_enabled=false", "csrf().disable()", "csrf(csrf->csrf.disable())", "csrf{disable()}",
     ".csrf(abstracthttpconfigurer::disable)", "csrf{it.disable()}", "csrf(it.disable())", "[ignoreantiforgerytoken", "ignoreantiforgerytokenattribute", "csrfprotection:false", "csrf:false"]
        .iter().any(|x| f.contains(x))
}

fn weak_hash(line: &str, ctx: &str, l: L) -> bool {
    let lo = line.to_ascii_lowercase();
    (lo.contains("md5") || lo.contains("sha1") || lo.contains("sha-1") || lo.contains("sha_1")) && code_idents(ctx, l).iter().any(|(n, _)| password_name(n))
}

/// A secret-named name set to a literal that looks like a real value. The safety net's credential patterns are
/// its own (and refuse the landing); this rule covers what they leave out.
fn hardcoded(line: &str) -> bool {
    if crate::guard::find_secret(line).is_some() { return false; }
    let t = line.trim();
    let Some(i) = t.find(['=', ':']) else { return false };
    if t[i + 1..].starts_with(['=', '>', ':']) || t[..i].ends_with(['!', '<', '>', '=']) { return false; }
    let name: String = t[..i].trim_end().trim_end_matches(['"', '\'']).chars().rev().take_while(|c| ident(*c)).collect::<Vec<_>>().into_iter().rev().collect();
    if !secret_name(&name) { return false; }
    let r = t[i + 1..].trim_start();
    // `password: str = "..."`: the value comes after the type.
    let r = if r.starts_with(['"', '\'', '`']) { r } else { match r.find('=') { Some(j) if !r[j + 1..].starts_with('=') => r[j + 1..].trim_start(), _ => return false } };
    let Some(q) = r.chars().next().filter(|c| matches!(c, '"' | '\'' | '`')) else { return false };
    let v: String = r[1..].chars().take_while(|c| *c != q).collect();
    v.len() >= 8 && !v.contains([' ', '/', '.', '$', '{', '<', '%']) && v.chars().any(|c| c.is_ascii_digit()) && !v.chars().all(|c| c.is_ascii_digit())
        && !["example", "xxx", "placeholder", "changeme", "your", "dummy", "test", "fake", "sample"].iter().any(|p| v.to_ascii_lowercase().contains(p))
}

const LOG_CALLS: &[&str] = &["print(", "println(", "printf(", "logging.", "logger.", "log.", "LOG.", "_log.", "_logger.", "console.", "fmt.Print", "fmt.Fprint",
    "slog.", "zap.", "System.out.print", "System.err.print", "Log.d(", "Log.i(", "Log.w(", "Log.e(", "Log.v(", "Console.Write", "Debug.WriteLine", "Trace.Write",
    "println!(", "eprintln!(", "print!(", "eprint!(", "info!(", "debug!(", "warn!(", "error!(", "trace!(", "dbg!(", "logrus.",
    "fprintf(", "syslog(", "printk(", "pr_info(", "pr_err(", "pr_warn(", "pr_debug(", "puts(", "fputs(", "std::cout", "std::cerr", "std::clog", "spdlog::", "LOG(", "qDebug(", "fmt::print("];

fn secret_in_log(line: &str, l: L) -> bool {
    let Some(at) = LOG_CALLS.iter().filter_map(|c| at_word(line, c, true)).min() else { return false };
    let args = &line[at..];
    let off = line.len() - args.len();
    code_idents(args, l).iter().any(|(n, i)| {
        if !secret_name(n) { return false; }
        let (before, after) = (&line[..off + i], &line[off + i + n.len()..]);
        let after = after.trim_start();
        // Its length or a check on it is fine; a call on the logger named like a secret is not an argument.
        !(before.ends_with("len(") || [".length", ".len()", ".size", ".count", ".is", ".has", "?.length"].iter().any(|m| after.starts_with(m))
            || after.starts_with('(') || after.starts_with("!=") || after.starts_with("==") || after.starts_with("is "))
    })
}

/// The first argument of the C call at `at` is a string literal (`"..."`, `L"..."`, `u8"..."`).
fn c_literal_arg(line: &str, at: usize) -> bool {
    let Some(p) = line[at..].find('(') else { return false };
    let r = line[at + p + 1..].trim_start();
    let r = ["L", "u8", "u", "U"].iter().find_map(|x| r.strip_prefix(x).filter(|r| r.starts_with('"'))).unwrap_or(r);
    r.starts_with('"')
}

/// Top-level arguments of the first call in `t` (`f(a, g(b, c), "x,y")` -> [a, g(b, c), "x,y"]), white space removed.
/// None when the call does not close within `t`.
fn c_args(t: &str) -> Option<Vec<String>> {
    let open = t.find('(')?;
    let (mut out, mut cur, mut depth, mut quote) = (vec![], String::new(), 0i32, None::<char>);
    let mut chars = t[open + 1..].chars();
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            cur.push(c);
            if c == '\\' { if let Some(n) = chars.next() { cur.push(n); } } else if c == q { quote = None; }
            continue;
        }
        match c {
            '"' | '\'' => { quote = Some(c); cur.push(c); }
            '(' | '[' | '{' => { depth += 1; cur.push(c); }
            ')' | ']' | '}' if depth == 0 => { out.push(squash(&cur)); return Some(out.into_iter().filter(|a| !a.is_empty()).collect()); }
            ')' | ']' | '}' => { depth -= 1; cur.push(c); }
            ',' if depth == 0 => out.push(squash(&std::mem::take(&mut cur))),
            _ => cur.push(c),
        }
    }
    None
}

/// C and C++: copies with no size limit, and memcpy / memmove with a length nobody checked.
fn c_unbounded(line: &str, ctx: &str, before: &[&str]) -> bool {
    for c in ["strcpy(", "strcat(", "wcscpy(", "wcscat(", "sprintf(", "vsprintf(", "gets(", "_mbscpy(", "lstrcpy(", "lstrcat("] {
        let Some(at) = at_word(line, c, true) else { continue };
        let args = c_args(&line[at..]).unwrap_or_default();
        // Copying a literal into a buffer is bounded by what the code says.
        if matches!(c, "strcpy(" | "strcat(" | "wcscpy(" | "wcscat(") && args.get(1).map(|a| a.starts_with('"')).unwrap_or(false) { continue; }
        return true;
    }
    for c in ["scanf(", "fscanf(", "sscanf(", "wscanf(", "swscanf("] {
        if let Some(at) = at_word(line, c, true) { if line[at..].contains("%s") || line[at..].contains("%[") { return true; } }
    }
    for c in ["memcpy(", "memmove(", "wmemcpy(", "CopyMemory(", "RtlCopyMemory("] {
        let Some(at) = at_word(line, c, true) else { continue };
        let Some(args) = c_args(&ctx[ctx.find(&line[at..]).unwrap_or(0)..]) else { continue };
        let Some(len) = args.get(2) else { continue };
        if len.contains("sizeof") || len.chars().all(|ch| ch.is_ascii_digit() || ch == 'u' || ch == 'U' || ch == 'L')
            || (len.chars().any(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit()) && len.chars().all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || "_ +-*()".contains(ch))) || len.starts_with("MIN(") || len.starts_with("min(") || len.starts_with("std::min(") { continue; }
        // `a <= b ? a : b` picks the smaller one.
        if len.contains('?') && (len.contains('<') || len.contains('>')) { continue; }
        let idents = |s: &str| -> Vec<String> { code_idents(s, L::C).into_iter().map(|x| x.0).filter(|n| !n.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(true)).collect() };
        let names = idents(len);
        let checked = before.iter().any(|b| names.iter().any(|n| has_word(b, n) && {
            let b2 = b.replace("->", ".");
            // The length was compared with something in the lines just before.
            b2.contains('<') || b2.contains('>') || b.contains("MIN(") || b.contains("min(")
            // ... or set from sizeof or a minimum.
            || (b.contains(&format!("{n} =")) || b.contains(&format!("{n}="))) && b.contains("sizeof")
            // ... or the buffer it goes into was just allocated with it.
            || b.contains("alloc")
        }));
        if !checked { return true; }
    }
    false
}

/// C and C++: the format argument of a printf-like call is a variable, and nothing follows it.
fn c_format_var(line: &str, ctx: &str) -> bool {
    for (c, idx) in [("printf(", 0usize), ("fprintf(", 1), ("dprintf(", 1), ("sprintf(", 1), ("snprintf(", 2), ("syslog(", 1), ("printk(", 0), ("wprintf(", 0), ("fwprintf(", 1), ("err(", 1), ("warn(", 0), ("errx(", 1), ("warnx(", 0)] {
        let Some(at) = at_word(line, c, true) else { continue };
        let Some(args) = c_args(&ctx[ctx.find(&line[at..]).unwrap_or(0)..]) else { continue };
        if args.len() != idx + 1 { continue; }
        let f = &args[idx];
        // `cond ? "true" : "false"` picks between literals.
        let picks = f.split_once('?').and_then(|(_, r)| r.split_once(':')).map(|(a, b)| a.starts_with('"') && b.starts_with('"')).unwrap_or(false);
        let literal = picks || f.starts_with('"') || f.starts_with("L\"") || f.starts_with("u8\"") || f.starts_with("_(") || f.starts_with("gettext(") || f.starts_with("N_(")
            || f.chars().all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_') || f.starts_with("KERN_");
        if !literal { return true; }
    }
    false
}

fn rule(id: &str) -> (&'static str, &'static str, &'static str, &'static str) { *RULES.iter().find(|r| r.0 == id).expect("known rule") }

/// The inline allow on a line: `gitta: allow <rule-id> <reason>`.
fn inline_allow(line: &str, id: &str) -> Option<String> {
    let i = line.find("gitta: allow ")?;
    let mut w = line[i + 13..].splitn(2, char::is_whitespace);
    (w.next()? == id).then(|| w.next().unwrap_or("").trim().trim_end_matches("*/").trim().to_string())
}

/// Findings on the lines `new` adds to `old`. `allows`: `[allow]` entries as (entry, added by this change).
fn check_file(path: &str, old: Option<&[u8]>, new: &[u8], allows: &[(Allow, bool)]) -> Vec<Note> {
    let Some(l) = lang(path) else { return vec![] };
    let new = String::from_utf8_lossy(new);
    let old = old.map(String::from_utf8_lossy).unwrap_or_default();
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for x in old.lines() { *seen.entry(x.trim()).or_default() += 1; }
    let lines: Vec<&str> = new.lines().collect();
    let test = crate::guard::is_test(path);
    let mut out: Vec<Note> = vec![];
    for (n, raw) in lines.iter().enumerate() {
        let t = raw.trim();
        // Added: the new file has this line more often than the old one.
        match seen.get_mut(t) { Some(c) if *c > 0 => { *c -= 1; continue; } _ => {} }
        if t.is_empty() || comment(t, l) { continue; }
        // A statement may run over a few lines: the line and the next two are its context.
        let ctx = lines[n..(n + 3).min(lines.len())].join("\n");
        let mut hits: Vec<&str> = vec![];
        let lo = t.to_ascii_lowercase();
        if sql_text(&lo) && dynamic(t, l) {
            let used_later = || assigned(t).map(|v| lines[n + 1..(n + 16).min(lines.len())].iter().any(|x| sql_call(x) && has_word(x, &v))).unwrap_or(false);
            if sql_call(t) || used_later() { hits.push("sql-text"); }
        }
        if shell(t, &ctx, &new, l) { hits.push("shell-text"); }
        if evals(t, l) { hits.push("eval-text"); }
        if deserialise(t, &ctx, l) { hits.push("unsafe-deserialise"); }
        if tls_off(t, &ctx, l) { hits.push("tls-off"); }
        let f = flat(t);
        if CORS_ANY.iter().chain(CORS_CRED).any(|m| f.contains(m)) {
            let win = flat(&lines[n.saturating_sub(8)..(n + 9).min(lines.len())].join("\n"));
            if CORS_ANY.iter().any(|m| win.contains(m)) && CORS_CRED.iter().any(|m| win.contains(m))
                && !out.iter().any(|o| o.rule == "cors-credentials" && o.line + 8 > n) { hits.push("cors-credentials"); }
        }
        if csrf_off(t) { hits.push("csrf-off"); }
        if weak_hash(t, &ctx, l) { hits.push("weak-password-hash"); }
        if !test && hardcoded(t) { hits.push("hardcoded-secret"); }
        if secret_in_log(t, l) { hits.push("secret-in-log"); }
        if l == L::C {
            if c_unbounded(t, &ctx, &lines[n.saturating_sub(12)..n]) { hits.push("unsafe-c-buffer"); }
            if c_format_var(t, &ctx) { hits.push("format-string"); }
        }
        for id in hits {
            let (_, sev, text, safer) = rule(id);
            // Test code does not run in production: nothing in it needs a person.
            let sev = if test { "medium" } else { sev };
            let (mut allowed, mut allow_new) = (inline_allow(raw, id), true);
            if allowed.is_none() {
                if let Some((a, added)) = allows.iter().find(|(a, _)| a.rule == id && crate::security::glob(&a.glob, path)) { allowed = Some(a.reason.clone()); allow_new = *added; }
            }
            out.push(Note { path: path.into(), line: n + 1, rule: id.into(), severity: sev.into(), text: text.into(), safer: safer.into(),
                            allow_new: allowed.is_some() && allow_new, allowed, key: fnv(&format!("{path}|{id}|{t}")) });
        }
    }
    out
}

/// Layer 1 findings for one agent's changed files, base against new. The `[allow]` entries are the committed ones
/// (see `load`) plus any the change adds to its own `.gitta-security`, which count as added by the change.
pub fn findings(base: &Snapshot, new: &Snapshot, paths: &[String]) -> Vec<Note> {
    let mut allows: Vec<(Allow, bool)> = ALLOWS.read().map(|g| g.iter().cloned().map(|a| (a, false)).collect()).unwrap_or_default();
    if let Some(n) = new.get(".gitta-security") {
        let txt = |s: Option<&Vec<u8>>| String::from_utf8_lossy(s.map(|v| v.as_slice()).unwrap_or(&[])).to_string();
        let old: Vec<String> = parse_allows(&txt(base.get(".gitta-security"))).iter().map(|a| format!("{} {}", a.rule, a.glob)).collect();
        allows.extend(parse_allows(&txt(Some(n))).into_iter().filter(|a| !old.contains(&format!("{} {}", a.rule, a.glob))).map(|a| (a, true)));
    }
    paths.iter().filter(|p| applies(p)).filter_map(|p| new.get(p).map(|n| (p, n)))
        .filter(|(p, n)| base.get(*p) != Some(*n))
        .flat_map(|(p, n)| check_file(p, base.get(p).map(|v| v.as_slice()), n, &allows)).collect()
}

/// Severity rank for the land threshold: none < medium < high.
pub fn rank(s: &str) -> u8 { match s { "high" => 2, "medium" => 1, _ => 0 } }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn helpers() {
        assert!(dynamic("q = f\"SELECT {x}\"", L::Py) && !dynamic("cur.execute(\"SELECT %s\", (x,))", L::Py));
        assert_eq!(assigned("    query: str = f\"x\""), Some("query".into()));
        assert_eq!(assigned("const sql = `x`"), Some("sql".into()));
        assert!(secret_name("password") && secret_name("accessToken") && secret_name("API_KEY") && !secret_name("token_count") && !secret_name("has_password") && !secret_name("tokenizer"));
        assert!(code_idents("print(\"password: {pw}\", token)", L::Py).iter().any(|x| x.0 == "token"));
        assert!(!code_idents("print(\"password reset\")", L::Py).iter().any(|x| x.0 == "password"));
        // Multibyte text before the identifier: positions are byte offsets, so slicing `line` never splits a char.
        assert!(secret_in_log("log.info(\"😀😀\", password)", L::Py));
        assert!(code_idents("log.info(\"😀\", password)", L::Py).iter().any(|x| x.0 == "password"));
        assert_eq!(inline_allow("x  # gitta: allow tls-off staging only", "tls-off"), Some("staging only".into()));
    }
}
