//! `gitta update` and the once-a-day update notice.
//! The release source is GitHub releases of gitta-io/gitta; assets are named as dist/install.sh expects
//! (gitta-<version>-<target>.tar.gz plus .sha256). GITTA_UPDATE_API and GITTA_UPDATE_DOWNLOAD point at another
//! source (tests use file:// URLs), and then gitta.io is not asked. Without them, gitta asks https://gitta.io first
//! (it counts version checks and downloads without personal data) and falls back to GitHub when gitta.io fails.
//! Downloads go through curl, as in the install script.
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const API: &str = "https://api.github.com/repos/gitta-io/gitta/releases/latest";
const DOWNLOAD: &str = "https://github.com/gitta-io/gitta/releases/download";
const GITTA_LATEST: &str = "https://gitta.io/latest";
const GITTA_DL: &str = "https://gitta.io/dl";
const NO_CURL: &str = "gitta update needs the curl command, and it is not installed. Install curl, then try again";
const DAY: u64 = 24 * 60 * 60;

/// The release workflow sets GITTA_RELEASE_VERSION from the tag, so a release candidate knows its own version.
pub fn current() -> &'static str { match option_env!("GITTA_RELEASE_VERSION") { Some(v) if !v.is_empty() => v, _ => env!("CARGO_PKG_VERSION") } }

/// Release target triple of this build, as the install script names it.
pub fn target() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-musl"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-musl"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        ("windows", "aarch64") => Some("aarch64-pc-windows-msvc"),
        _ => None,
    }
}

fn parse(v: &str) -> (Vec<u64>, Option<String>) {
    let v = v.trim().trim_start_matches('v');
    let (core, pre) = match v.split_once('-') { Some((c, p)) => (c, Some(p.to_string())), None => (v, None) };
    let core = core.split_once('+').map(|x| x.0).unwrap_or(core);
    (core.split('.').map(|p| p.parse().unwrap_or(0)).collect(), pre)
}

/// True when `latest` is a newer version than `have`. A pre-release sorts before its release.
pub fn newer(latest: &str, have: &str) -> bool {
    let ((mut a, ap), (mut b, bp)) = (parse(latest), parse(have));
    let n = a.len().max(b.len()); a.resize(n, 0); b.resize(n, 0);
    match a.cmp(&b) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => match (ap, bp) { (None, Some(_)) => true, (Some(x), Some(y)) => x > y, _ => false },
    }
}

#[derive(Debug, PartialEq)]
pub enum Method { Homebrew, Cargo, Direct }

/// How this binary was installed, from its resolved path.
pub fn install_method(exe: &Path, home: Option<&Path>, cargo_home: Option<&Path>, brew_prefix: Option<&Path>) -> Method {
    let s = exe.to_string_lossy();
    if s.contains("/Cellar/") || s.starts_with("/opt/homebrew/") || s.starts_with("/home/linuxbrew/.linuxbrew/")
        || brew_prefix.is_some_and(|p| !p.as_os_str().is_empty() && exe.starts_with(p.join("Cellar"))) { return Method::Homebrew; }
    let cargo_bin = cargo_home.map(|c| c.join("bin")).or_else(|| home.map(|h| h.join(".cargo/bin")));
    if cargo_bin.is_some_and(|b| exe.starts_with(&b)) || home.is_some_and(|h| exe.starts_with(h.join(".cargo/bin"))) { return Method::Cargo; }
    Method::Direct
}

fn env_path(k: &str) -> Option<PathBuf> { std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from) }
fn real(p: Option<PathBuf>) -> Option<PathBuf> { p.map(|p| std::fs::canonicalize(&p).unwrap_or(p)) }

fn this_method(exe: &Path) -> Method {
    install_method(exe, real(env_path("HOME")).as_deref(), real(env_path("CARGO_HOME")).as_deref(), real(env_path("HOMEBREW_PREFIX")).as_deref())
}

/// The command that removes this gitta program, for the last line of `gitta uninstall --everywhere`.
pub fn remove_command() -> String {
    let Some(exe) = std::env::current_exe().ok().map(|p| plain(std::fs::canonicalize(&p).unwrap_or(p))) else { return "delete the gitta binary".into() };
    match this_method(&exe) {
        Method::Homebrew => "brew uninstall gitta".into(),
        Method::Cargo => "cargo uninstall gitta".into(),
        Method::Direct => {
            let dir = exe.parent().unwrap_or(Path::new("."));
            let extra: Vec<String> = ["gitta-fs", "gitta-sandbox"].iter().filter(|f| dir.join(f).exists()).map(|f| dir.join(f).display().to_string()).collect();
            format!("{} {}{}", if cfg!(windows) { "Remove-Item" } else { "rm" }, exe.display(), extra.iter().map(|x| format!(" {x}")).collect::<String>())
        }
    }
}

fn fetch(url: &str, out: Option<&Path>, timeout: u32) -> Result<Vec<u8>, String> {
    let mut c = Command::new("curl");
    c.args(["-fsSL", "--max-time", &timeout.to_string(), "-H", "Accept: application/vnd.github+json", url]);
    if let Some(o) = out { c.arg("-o").arg(o); }
    match c.stdin(Stdio::null()).stderr(Stdio::piped()).output() {
        Ok(o) if o.status.success() => Ok(o.stdout),
        Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().trim_start_matches("curl: ").to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(NO_CURL.into()),
        Err(e) => Err(format!("cannot run curl: {e}")),
    }
}

/// The version-check URLs to try in order. The gitta.io one carries only this version, the OS and the CPU.
pub fn latest_urls(api_override: Option<String>) -> Vec<String> {
    match api_override {
        Some(a) => vec![a],
        None => vec![format!("{GITTA_LATEST}?v={}&os={}&arch={}", current(), std::env::consts::OS, std::env::consts::ARCH), API.into()],
    }
}

/// The release tag in a version-check answer: GitHub sends {"tag_name"}, gitta.io sends {"version", "tag"}.
pub fn tag_from(body: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    if let Some(t) = v["tag_name"].as_str().or(v["tag"].as_str()).filter(|t| !t.is_empty()) { return Some(t.into()); }
    v["version"].as_str().filter(|t| !t.is_empty()).map(|t| format!("v{}", t.trim_start_matches('v')))
}

/// Latest release tag, for example "v0.2.0".
pub fn latest(timeout: u32) -> Result<String, String> {
    let mut err = String::new();
    for url in latest_urls(std::env::var("GITTA_UPDATE_API").ok()) {
        match fetch(&url, None, timeout) {
            Ok(body) => match tag_from(&body) { Some(t) => return Ok(t), None => err = "the release source sent no version".into() },
            Err(e) if e == NO_CURL => return Err(e),
            Err(e) => err = e,
        }
    }
    Err(err)
}

/// Download bases to try in order, each with the query to add to a file URL.
pub fn download_bases(dl_override: Option<String>) -> Vec<(String, &'static str)> {
    match dl_override {
        Some(d) => vec![(d, "")],
        None => vec![(GITTA_DL.into(), "?via=update"), (DOWNLOAD.into(), "")],
    }
}

/// Release archive extension: Windows builds ship as .zip, the others as .tar.gz.
pub fn archive_ext(target: &str) -> &'static str { if target.contains("-windows-") { "zip" } else { "tar.gz" } }

/// Windows paths from canonicalize start with \\?\; show and compare them without it.
fn plain(p: PathBuf) -> PathBuf { match p.to_str().and_then(|s| s.strip_prefix(r"\\?\")) { Some(s) if !s.starts_with("UNC") => PathBuf::from(s), _ => p } }

/// tar from System32 on Windows (bsdtar, which also unpacks zip files); `tar` from PATH elsewhere.
fn tar() -> Command {
    if cfg!(windows) {
        if let Some(t) = env_path("SystemRoot").map(|r| r.join(r"System32\tar.exe")).filter(|t| t.exists()) { return Command::new(t); }
    }
    Command::new("tar")
}

/// Remove the old binaries a Windows update renamed aside (a running .exe can be renamed, not overwritten or deleted).
pub fn remove_old(bin_dir: &Path) {
    if let Ok(rd) = std::fs::read_dir(bin_dir) {
        for e in rd.flatten() { if e.file_name().to_string_lossy().contains(".old-") && e.file_name().to_string_lossy().starts_with('.') { let _ = std::fs::remove_file(e.path()); } }
    }
}

fn sha256_hex(b: &[u8]) -> String { ring::digest::digest(&ring::digest::SHA256, b).as_ref().iter().map(|x| format!("{x:02x}")).collect() }

/// Download, verify and unpack a release into `work` (in the binary's folder), then rename each file over.
/// Returns the names that were replaced.
pub fn install_release(tag: &str, bin_dir: &Path, target: &str) -> Result<Vec<String>, String> {
    let bases = download_bases(std::env::var("GITTA_UPDATE_DOWNLOAD").ok());
    let name = format!("gitta-{}-{target}", tag.trim_start_matches('v'));
    let ext = archive_ext(target);
    let main = if target.contains("-windows-") { "gitta.exe" } else { "gitta" };
    let work = bin_dir.join(format!(".gitta-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| format!("cannot write in {}: {e}", bin_dir.display()))?;
    let r = (|| {
        let tgz = work.join(format!("{name}.{ext}"));
        let mut got = Err(String::new());
        for (dl, q) in &bases {
            got = fetch(&format!("{dl}/{tag}/{name}.{ext}{q}"), Some(&tgz), 300)
                .and_then(|_| fetch(&format!("{dl}/{tag}/{name}.{ext}.sha256{q}"), None, 30));
            if got.is_ok() || got.as_ref().is_err_and(|e| e == NO_CURL) { break; }
        }
        let sum = got?;
        let want = String::from_utf8_lossy(&sum).split_whitespace().next().unwrap_or("").to_lowercase();
        let have = sha256_hex(&std::fs::read(&tgz).map_err(|e| e.to_string())?);
        if want.is_empty() || want != have { return Err("checksum mismatch; nothing was changed".to_string()); }
        let ok = tar().arg(if ext == "zip" { "-xf" } else { "-xzf" }).arg(&tgz).arg("-C").arg(&work).status().map(|s| s.success()).unwrap_or(false);
        if !ok { return Err("cannot unpack the release; nothing was changed".into()); }
        let dir = work.join(&name);
        let mut files: Vec<String> = std::fs::read_dir(&dir).map_err(|_| "the release has no files for this system".to_string())?
            .flatten().map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|f| f == main || bin_dir.join(f).exists()).collect();
        files.sort();
        if !files.iter().any(|f| f == main) { return Err("the release has no gitta binary; nothing was changed".into()); }
        // Write each new file next to the old one, then rename over it: the running binary is never written in place.
        for f in &files {
            let tmp = bin_dir.join(format!(".{f}.new-{}", std::process::id()));
            std::fs::copy(dir.join(f), &tmp).map_err(|e| e.to_string())?;
            #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)); }
        }
        for f in &files {
            let tmp = bin_dir.join(format!(".{f}.new-{}", std::process::id()));
            let dest = bin_dir.join(f);
            // Windows cannot replace a running .exe, but it can rename it: move the old file aside first.
            // The next `gitta update` removes it.
            let old = bin_dir.join(format!(".{f}.old-{}", std::process::id()));
            let aside = cfg!(windows) && dest.exists();
            if aside { std::fs::rename(&dest, &old).map_err(|e| format!("cannot replace {f}: {e}"))?; }
            if let Err(e) = std::fs::rename(&tmp, &dest) {
                if aside { let _ = std::fs::rename(&old, &dest); }
                return Err(format!("cannot replace {f}: {e}"));
            }
        }
        Ok(files)
    })();
    let _ = std::fs::remove_dir_all(&work);
    if r.is_err() { if let Ok(rd) = std::fs::read_dir(bin_dir) { for e in rd.flatten() { if e.file_name().to_string_lossy().contains(&format!(".new-{}", std::process::id())) { let _ = std::fs::remove_file(e.path()); } } } }
    r
}

pub fn cmd(args: &[String]) {
    let check = args.iter().any(|a| a == "--check");
    // Old binaries an earlier update moved aside are no longer running: remove them first, also when no update follows.
    if let Some(d) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) { remove_old(&d); }
    let tag = match latest(15) {
        Ok(t) => t,
        Err(e) if e == NO_CURL => { eprintln!("gitta: {e}."); std::process::exit(1) }
        Err(e) => { eprintln!("gitta: cannot reach the release server ({e}). Check the network connection and try again."); std::process::exit(1) }
    };
    write_cache(&tag);
    let v = tag.trim_start_matches('v');
    if !newer(v, current()) { println!("gitta: {} is the latest version", current()); return; }
    let exe = std::env::current_exe().ok().map(|p| plain(std::fs::canonicalize(&p).unwrap_or(p)));
    let method = exe.as_deref().map(this_method).unwrap_or(Method::Direct);
    let how = match method { Method::Homebrew => Some("brew upgrade gitta"), Method::Cargo => Some("cargo install --force --git https://github.com/gitta-io/gitta gitta"), Method::Direct => None };
    if check {
        println!("gitta: {v} is available (you have {}). Run `{}` to update.", current(), how.unwrap_or("gitta update"));
        return;
    }
    if let Some(h) = how {
        let by = if method == Method::Homebrew { "Homebrew" } else { "cargo" };
        println!("gitta: {v} is available (you have {}). {by} installed this gitta, so update it with:\n  {h}", current());
        return;
    }
    let Some(t) = target() else { eprintln!("gitta: there is no release build for this system; build {v} from source"); std::process::exit(1) };
    let exe = exe.unwrap_or_else(|| { eprintln!("gitta: cannot find the running gitta binary"); std::process::exit(1) });
    let bin_dir = exe.parent().unwrap_or(Path::new(".")).to_path_buf();
    println!("gitta: downloading {v} for {t}");
    match install_release(&tag, &bin_dir, t) {
        Ok(files) => println!("gitta: updated {} in {} to {v}", files.join(", "), bin_dir.display()),
        Err(e) => { eprintln!("gitta: update failed: {e}"); std::process::exit(1) }
    }
}

// ---- once-a-day notice ----

fn cache_dir() -> Option<PathBuf> {
    if let Some(d) = env_path("GITTA_CACHE_DIR") { return Some(d); }
    if cfg!(target_os = "macos") { return env_path("HOME").map(|h| h.join("Library/Caches/gitta")); }
    if cfg!(windows) { return env_path("LOCALAPPDATA").map(|d| d.join(r"gitta\cache")); }
    env_path("XDG_CACHE_HOME").or_else(|| env_path("HOME").map(|h| h.join(".cache"))).map(|d| d.join("gitta"))
}

/// User settings file: ~/.config/gitta/config.json ({"update_check": false} turns the notice off).
pub fn user_config() -> Option<PathBuf> {
    if cfg!(windows) && env_path("XDG_CONFIG_HOME").is_none() && env_path("HOME").is_none() { return env_path("APPDATA").map(|d| d.join(r"gitta\config.json")); }
    env_path("XDG_CONFIG_HOME").or_else(|| env_path("HOME").map(|h| h.join(".config"))).map(|d| d.join("gitta/config.json"))
}

fn now() -> u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) }

fn read_cache() -> serde_json::Value {
    cache_dir().and_then(|d| std::fs::read_to_string(d.join("update-check.json")).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(serde_json::json!({}))
}

fn save_cache(v: &serde_json::Value) {
    if let Some(d) = cache_dir() {
        let _ = std::fs::create_dir_all(&d);
        let tmp = d.join(format!("update-check.json.{}", std::process::id()));
        if std::fs::write(&tmp, v.to_string()).is_ok() { let _ = std::fs::rename(&tmp, d.join("update-check.json")); }
    }
}

fn write_cache(tag: &str) { let mut c = read_cache(); c["checked"] = now().into(); c["latest"] = tag.into(); save_cache(&c); }

/// Whether the notice may run for this command at all.
pub fn notice_allowed(cmd: &str, args: &[String]) -> bool {
    if matches!(cmd, "hook" | "mcp" | "update" | "__update-refresh" | "server") { return false; }
    if args.iter().any(|a| a == "--json" || a == "--csv" || a == "--help" || a == "-h") { return false; }
    if std::env::var_os("CI").is_some_and(|v| !v.is_empty()) || std::env::var("GITTA_NO_UPDATE_CHECK").is_ok_and(|v| v == "1") { return false; }
    if std::env::var_os("CLAUDECODE").is_some() { return false; }
    let off = user_config().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .is_some_and(|v| v["update_check"] == serde_json::json!(false));
    if off { return false; }
    std::io::stdout().is_terminal() && std::io::stderr().is_terminal()
}

/// Print the notice from the cache (at most once a day) and start a background refresh when the cache is a day old.
/// Never waits on the network.
pub fn notice(cmd: &str, args: &[String]) {
    if !notice_allowed(cmd, args) { return; }
    let mut c = read_cache();
    let t = now();
    if let Some(tag) = c["latest"].as_str() {
        let v = tag.trim_start_matches('v').to_string();
        if newer(&v, current()) && c["notified"].as_u64().is_none_or(|n| t.saturating_sub(n) >= DAY) {
            eprintln!("gitta: {v} is available (you have {}). Run `gitta update`, or see `gitta update --check`.", current());
            c["notified"] = t.into(); save_cache(&c);
        }
    }
    if c["checked"].as_u64().is_none_or(|n| t.saturating_sub(n) >= DAY) {
        c["checked"] = t.into(); save_cache(&c); // one attempt a day, also when offline
        if let Ok(exe) = std::env::current_exe() {
            let _ = Command::new(exe).arg("__update-refresh").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
        }
    }
}

/// Background child: ask for the latest version with a short timeout and remember it.
pub fn refresh() { if let Ok(tag) = latest(5) { write_cache(&tag); } }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn versions_compare() {
        assert!(newer("v0.2.0", "0.1.0"));
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("1.0", "0.9.9"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("v0.1.0", "0.2.0"));
        assert!(newer("0.2.0", "0.2.0-rc.1"));
        assert!(!newer("0.2.0-rc.1", "0.2.0"));
        assert!(newer("0.2.0-rc.2", "0.2.0-rc.1"));
        assert!(!newer("0.1.0+build5", "0.1.0"));
    }
    #[test]
    fn release_sources() {
        let u = latest_urls(None);
        assert!(u[0].starts_with("https://gitta.io/latest?v=") && u[0].contains("&os=") && u[0].contains("&arch=") && u[1] == API, "{u:?}");
        assert_eq!(latest_urls(Some("file:///x".into())), vec!["file:///x".to_string()]);
        assert_eq!(download_bases(None), vec![(GITTA_DL.to_string(), "?via=update"), (DOWNLOAD.to_string(), "")]);
        assert_eq!(download_bases(Some("file:///d".into())), vec![("file:///d".to_string(), "")]);
        assert_eq!(tag_from(br#"{"tag_name":"v0.3.0"}"#).as_deref(), Some("v0.3.0"));
        assert_eq!(tag_from(br#"{"version":"0.3.0","tag":"v0.3.0"}"#).as_deref(), Some("v0.3.0"));
        assert_eq!(tag_from(br#"{"version":"0.3.0"}"#).as_deref(), Some("v0.3.0"));
        assert_eq!(tag_from(br#"{"error":"x"}"#), None);
        assert_eq!(archive_ext("x86_64-pc-windows-msvc"), "zip");
        assert_eq!(archive_ext("aarch64-apple-darwin"), "tar.gz");
    }
    #[test]
    fn install_methods() {
        let h = Path::new("/Users/u");
        assert_eq!(install_method(Path::new("/opt/homebrew/Cellar/gitta/0.1.0/bin/gitta"), Some(h), None, None), Method::Homebrew);
        assert_eq!(install_method(Path::new("/usr/local/Cellar/gitta/0.1.0/bin/gitta"), Some(h), None, None), Method::Homebrew);
        assert_eq!(install_method(Path::new("/home/linuxbrew/.linuxbrew/bin/gitta"), Some(h), None, None), Method::Homebrew);
        assert_eq!(install_method(Path::new("/brew/Cellar2/x/gitta"), Some(h), None, Some(Path::new("/brew"))), Method::Direct);
        assert_eq!(install_method(Path::new("/Users/u/.cargo/bin/gitta"), Some(h), None, None), Method::Cargo);
        assert_eq!(install_method(Path::new("/c/bin/gitta"), Some(h), Some(Path::new("/c")), None), Method::Cargo);
        assert_eq!(install_method(Path::new("/Users/u/.local/bin/gitta"), Some(h), None, None), Method::Direct);
        assert_eq!(install_method(Path::new("/usr/local/bin/gitta"), Some(h), None, None), Method::Direct);
    }
}
