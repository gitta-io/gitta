//! gitta-sandbox: run a coding agent with default-deny filesystem and network rights, enforced by the
//! Linux kernel (Landlock). Nothing sits in the I/O path, so file access runs at native speed.
//!
//!   gitta-sandbox --workspace DIR [--read PATH]... [--write PATH]... [--net-port N]... [--no-net] -- CMD ARGS...
//!
//! Rights granted (everything else is denied by the kernel, for the agent and every process it starts):
//!   * the workspace:            read, write, create, delete, execute
//!   * --write PATHS:            the same (e.g. a private cache directory)
//!   * system and --read PATHS:  read and execute only (/usr, /bin, /lib*, /etc, /opt, /proc, /sys, /dev, toolchains)
//!   * network (ABI 4+):         with --no-net no TCP connect/bind at all; with --net-port only those ports.
//!     A kernel without network rules makes these options fail instead of running open. UDP and raw sockets are
//!     not covered by Landlock; use a network namespace for a full network ban.
//! Not granted, therefore unreachable: the user's home (~/.ssh, ~/.aws, browser profiles ...), the main
//! checkout and its .git, other agents' workspaces, and anything else not listed.
//! Secrets inside the workspace are handled by not putting them in the view at all (Gitta hides them).

use landlock::{Access, AccessFs, AccessNet, CompatLevel, Compatible, NetPort, PathBeneath, PathFd, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus, ABI};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;

fn die(m: &str) -> ! { eprintln!("gitta-sandbox: {m}"); std::process::exit(2) }

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let split = args.iter().position(|a| a == "--").unwrap_or_else(|| die("usage: gitta-sandbox --workspace DIR [--read P]... [--write P]... [--net-port N]... [--no-net] -- CMD..."));
    let (opts, cmd) = (&args[..split], &args[split + 1..]);
    if cmd.is_empty() { die("no command") }
    let (mut ws, mut reads, mut writes, mut ports, mut no_net) = (None, vec![], vec![], vec![], false);
    let mut it = opts.iter();
    while let Some(a) = it.next() {
        let mut v = || it.next().cloned().unwrap_or_else(|| die(&format!("{a} needs a value")));
        match a.as_str() {
            "--workspace" => ws = Some(PathBuf::from(v())), "--read" => reads.push(PathBuf::from(v())), "--write" => writes.push(PathBuf::from(v())),
            "--net-port" => ports.push(v().parse::<u16>().unwrap_or_else(|_| die("bad port"))), "--no-net" => no_net = true,
            _ => die(&format!("unknown option {a}")),
        }
    }
    let ws = ws.unwrap_or_else(|| die("--workspace is required"));
    let abi = ABI::V5;
    let rw = AccessFs::from_all(abi);
    let ro = AccessFs::from_read(abi);
    let mut ruleset = Ruleset::default().handle_access(rw).unwrap_or_else(|e| die(&e.to_string()));
    let net = no_net || !ports.is_empty();
    // Network rules are a hard requirement: on a kernel without them (before Linux 6.7, Landlock ABI 4) refuse to
    // start, never run with the network open while the user asked for it closed. They cover TCP connect and bind;
    // other protocols (UDP, raw sockets) need a network namespace (container, `unshare -n`).
    if net {
        ruleset = ruleset.set_compatibility(CompatLevel::HardRequirement).handle_access(AccessNet::from_all(ABI::V4))
            .unwrap_or_else(|_| die("network rules need Linux 6.7 or later (Landlock ABI 4); refusing to run with --no-net or --net-port on this kernel"))
            .set_compatibility(CompatLevel::BestEffort);
    }
    let mut rs = ruleset.create().unwrap_or_else(|e| die(&e.to_string()));
    let add = |rs: landlock::RulesetCreated, p: &PathBuf, acc| -> landlock::RulesetCreated {
        match PathFd::new(p) { Ok(fd) => rs.add_rule(PathBeneath::new(fd, acc)).unwrap_or_else(|e| die(&e.to_string())), Err(_) => rs }  // missing paths are skipped
    };
    for p in std::iter::once(&ws).chain(writes.iter()) { rs = add(rs, p, rw); }
    let system: Vec<PathBuf> = ["/usr", "/bin", "/sbin", "/lib", "/lib64", "/lib32", "/etc", "/opt", "/proc", "/sys", "/dev", "/run", "/nix", "/snap", "/var/lib"]
        .iter().map(PathBuf::from).chain(reads.into_iter()).collect();
    for p in &system { rs = add(rs, p, ro); }
    // A few device files programs write to.
    for d in ["/dev/null", "/dev/tty", "/dev/zero", "/dev/full", "/dev/ptmx", "/dev/pts", "/dev/shm"] { rs = add(rs, &PathBuf::from(d), rw); }
    for port in &ports { rs = rs.add_rule(NetPort::new(*port, AccessNet::ConnectTcp)).unwrap_or_else(|e| die(&e.to_string())); }
    let status = rs.restrict_self().unwrap_or_else(|e| die(&e.to_string()));
    if status.ruleset == RulesetStatus::NotEnforced { die("this kernel does not enforce Landlock; refusing to run unconfined") }
    // Private temp directory inside the workspace (the system /tmp is not writable).
    let tmp = ws.join(".gitta-tmp");
    let _ = std::fs::create_dir_all(&tmp);
    let err = std::process::Command::new(&cmd[0]).args(&cmd[1..]).current_dir(&ws).env("TMPDIR", &tmp).env("TMP", &tmp).env("TEMP", &tmp).exec();
    die(&format!("exec {}: {err}", cmd[0]))
}
