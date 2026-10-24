//! gitta-sandbox: run a coding agent in a world built from only its worktree and the toolchain, using
//! unprivileged Linux namespaces (bubblewrap). Deny-by-construction: the host home, the environment, host
//! sockets (the Docker socket, the ssh-agent socket) and other processes are not present in the sandbox, so
//! there is nothing to exclude and nothing to leak. The agent can read and write its worktree and the paths
//! given with --write, read the toolchain, and reach the network for its API.
//!
//!   gitta-sandbox --workspace DIR [--read PATH]... [--write PATH]... [--no-net] [--env NAME]... -- CMD...
//!
//! Requires bubblewrap (bwrap) and unprivileged user namespaces enabled on the host (the default on modern
//! Linux). --no-net puts the agent in an empty network namespace with no connectivity at all.

use std::path::PathBuf;
use std::process::Command;

fn die(m: &str) -> ! { eprintln!("gitta-sandbox: {m}"); std::process::exit(2) }

fn which(bin: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| std::env::split_paths(&paths).map(|d| d.join(bin)).find(|p| p.is_file()))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let split = args.iter().position(|a| a == "--").unwrap_or_else(|| die("usage: gitta-sandbox --workspace DIR [--read P]... [--write P]... [--no-net] [--env NAME]... -- CMD..."));
    let (opts, cmd) = (&args[..split], &args[split + 1..]);
    if cmd.is_empty() { die("no command") }
    let (mut ws, mut reads, mut writes, mut no_net, mut envs) = (None, vec![], vec![], false, vec![]);
    let mut it = opts.iter();
    while let Some(a) = it.next() {
        let mut v = || it.next().cloned().unwrap_or_else(|| die(&format!("{a} needs a value")));
        match a.as_str() {
            "--workspace" => ws = Some(v()), "--read" => reads.push(v()), "--write" => writes.push(v()),
            "--no-net" => no_net = true,
            "--net-port" => { let _ = v(); }   // accepted for compatibility; network is on unless --no-net
            "--env" => envs.push(v()),
            _ => die(&format!("unknown option {a}")),
        }
    }
    let ws = ws.unwrap_or_else(|| die("--workspace is required"));
    if which("bwrap").is_none() { die("bubblewrap (bwrap) is not installed; the sandbox needs it. Install the 'bubblewrap' package.") }

    let mut b: Vec<String> = ["--die-with-parent", "--unshare-user", "--unshare-pid", "--unshare-ipc", "--unshare-uts"]
        .iter().map(|s| s.to_string()).collect();
    if no_net { b.push("--unshare-net".into()); }
    // The toolchain, read-only (missing paths are skipped).
    for p in ["/usr", "/bin", "/sbin", "/lib", "/lib64", "/lib32", "/opt", "/nix"] { b.extend(["--ro-bind-try".into(), p.into(), p.into()]); }
    // A minimal /etc, not the host's whole /etc: CA certs, name resolution, passwd.
    for p in ["/etc/ssl", "/etc/ca-certificates", "/etc/pki", "/etc/nsswitch.conf", "/etc/passwd", "/etc/alternatives"] {
        b.extend(["--ro-bind-try".into(), p.into(), p.into()]);
    }
    // DNS for the agent's API: bind the host's resolver, following symlinks (systemd-resolved stub).
    if !no_net {
        if let Ok(real) = std::fs::canonicalize("/etc/resolv.conf") {
            b.extend(["--ro-bind-try".into(), real.to_string_lossy().into(), "/etc/resolv.conf".into()]);
        }
    }
    for p in &reads { b.extend(["--ro-bind-try".into(), p.clone(), p.clone()]); }
    // Virtual filesystems first, so a workspace or --write path under /tmp is bound on top of the tmpfs, not hidden by it.
    b.extend(["--proc".into(), "/proc".into(), "--dev".into(), "/dev".into(), "--tmpfs".into(), "/tmp".into()]);
    b.extend(["--bind".into(), ws.clone(), ws.clone()]);
    for p in &writes { b.extend(["--bind".into(), p.clone(), p.clone()]); }
    b.extend(["--chdir".into(), ws.clone()]);
    b.push("--clearenv".into());
    b.extend(["--setenv".into(), "PATH".into(), "/usr/local/bin:/usr/bin:/bin".into(), "--setenv".into(), "TMPDIR".into(), "/tmp".into()]);
    for k in &envs { if let Ok(val) = std::env::var(k) { b.extend(["--setenv".into(), k.clone(), val]); } }
    b.push("--".into());
    b.extend(cmd.iter().cloned());

    let code = Command::new("bwrap").args(&b).status().map(|s| s.code().unwrap_or(-1)).unwrap_or_else(|e| die(&format!("start bwrap: {e}")));
    std::process::exit(code);
}
