//! gitta-fs: copy-on-write FUSE workspace for one coding agent.
//!
//! View = upper (the agent's own changes) over lower (shared read-only base).
//! * Reads come from upper if the path exists there, else from lower.
//! * Any modification copies the file up first; the lower tree is never written.
//! * Deleting a lower path records a whiteout (kept in memory and in `<upper>/.gitta-whiteouts`).
//! * A write policy (globs) refuses modifications at the moment they are attempted (EACCES).
//! * Every modification and every refusal is appended as one JSON line to the event log,
//!   so a watcher sees each edit as it happens, without scanning.
//!
//! Speed: the session runs several worker threads, and opened files use FUSE passthrough when
//! the kernel offers it: after the policy check and copy-up, the real upper/lower file is
//! registered as the backing file, so the kernel serves read/write/mmap directly from it.
//! Set GITTA_NO_PASSTHROUGH=1 to serve all I/O in userspace.
//!
//! Every path in the view is still mediated here: the kernel only caches answers this process
//! gave (attributes, entries, missing names, directory listings), and every change to the view
//! goes through this process, so the caches stay exact. Specifically:
//! * Missing names are answered with a negative entry, so the kernel caches ENOENT (module
//!   resolution probes thousands of names that do not exist). A create/mkdir/rename through the
//!   mount replaces the negative dentry.
//! * Which paths exist in upper is kept in memory (upper only changes through this process), so a
//!   lookup of a lower file costs one stat instead of three.
//! * Directory listings are snapshotted once per opendir (not once per readdir chunk), may be
//!   cached by the kernel (FOPEN_CACHE_DIR, dropped when the directory changes), and are answered
//!   with attributes (READDIRPLUS).
//!
//! Tuning switches for measurement (all default on): GITTA_NO_NEG, GITTA_NO_UPCACHE,
//! GITTA_NO_RDPLUS, GITTA_NO_DIRCACHE. GITTA_FS_STATS=1 prints per-op counts/latency at unmount.
//!
//! Events and passthrough: with passthrough the kernel no longer sends write() to this process,
//! so a 'write' event cannot be emitted per write call. Instead, a passthrough handle opened for
//! writing emits one 'write' event when it is released, if the file's size or mtime changed while
//! it was open. Handles that fall back to userspace I/O keep the per-write() event.
//!
//! Usage: gitta-fs --lower DIR --upper DIR --mount DIR [--deny GLOB]... [--deny-read GLOB]... [--events FILE] [--agent NAME]
//! --deny refuses modifications; --deny-read also refuses reading (secrets), and implies --deny.

use fuser::{
    BackingId, BsdFileFlags, Config, Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo,
    InitFlags, KernelConfig, LockOwner, MountOption, OpenFlags, RenameFlags, ReplyAttr, ReplyCreate, ReplyData,
    ReplyDirectory, ReplyDirectoryPlus, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyStatfs, ReplyWrite, Request, TimeOrNow,
    WriteFlags,
};
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// Every change to the view goes through this process and the lower tree is read-only,
// so the kernel may cache attributes, entries and file pages for a long time.
const TTL: Duration = Duration::from_secs(30);
// Missing names: the same argument holds (a name can only appear through this process, and the
// kernel replaces the negative dentry when it is created through the mount).
const NEG_TTL: Duration = Duration::from_secs(3600);
const THREADS: usize = 8;
const WHITEOUTS: &str = ".gitta-whiteouts";

// ---- per-op statistics (GITTA_FS_STATS=1) ----
const N_OPS: usize = 21;
const OPS: [&str; N_OPS] = [
    "lookup", "lookup_neg", "getattr", "setattr", "readlink", "mkdir", "unlink", "rmdir", "symlink", "rename", "open",
    "read", "write", "release", "fsync", "opendir", "readdir", "readdirplus", "releasedir", "create", "statfs",
];
#[derive(Clone, Copy)]
enum Op { Lookup, LookupNeg, Getattr, Setattr, Readlink, Mkdir, Unlink, Rmdir, Symlink, Rename, Open, Read, Write, Release, Fsync, Opendir, Readdir, Readdirplus, Releasedir, Create, Statfs }

struct Stats { on: bool, count: [AtomicU64; N_OPS], nanos: [AtomicU64; N_OPS] }
struct Timer<'a> { s: &'a Stats, op: Op, t: Option<Instant> }
impl Drop for Timer<'_> {
    fn drop(&mut self) {
        if let Some(t) = self.t {
            self.s.count[self.op as usize].fetch_add(1, Ordering::Relaxed);
            self.s.nanos[self.op as usize].fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
        }
    }
}
impl Stats {
    fn t(&self, op: Op) -> Timer<'_> { Timer { s: self, op, t: if self.on { Some(Instant::now()) } else { None } } }
    fn bump(&self, op: Op) { if self.on { self.count[op as usize].fetch_add(1, Ordering::Relaxed); } }
    fn print(&self) {
        if !self.on { return; }
        let mut total = 0u64;
        for (i, name) in OPS.iter().enumerate() {
            let c = self.count[i].load(Ordering::Relaxed);
            if c == 0 { continue; }
            let n = self.nanos[i].load(Ordering::Relaxed);
            if i != Op::LookupNeg as usize { total += c; }
            eprintln!("gitta-fs-stats {{\"op\":\"{name}\",\"count\":{c},\"total_ms\":{:.1},\"avg_us\":{:.1}}}", n as f64 / 1e6, n as f64 / 1e3 / c as f64);
        }
        eprintln!("gitta-fs-stats {{\"op\":\"TOTAL\",\"count\":{total}}}");
    }
}

/// Path <-> inode mapping, whiteouts, and the set of paths present in upper.
/// Read-locked for lookups, write-locked for changes.
struct Meta {
    paths: HashMap<u64, PathBuf>, // inode -> path relative to the view root ("" = root)
    inodes: HashMap<PathBuf, u64>,
    next_ino: u64,
    whiteouts: HashSet<PathBuf>,
    /// Every path that exists in upper (files, dirs, symlinks; "" = root). Upper only changes
    /// through this process, so this set is exact.
    up: HashSet<PathBuf>,
}

struct Handle {
    ino: u64,
    file: Arc<File>,
    rel: PathBuf,
    /// (size, mtime) at open of a writable passthrough handle, compared at release.
    watch: Option<(u64, i64, i64)>,
    _backing: Option<Arc<BackingId>>,
}

type Snapshot = Arc<(PathBuf, Vec<(u64, FileType, OsString)>)>;

struct Tuning { neg: bool, upcache: bool, rdplus: bool, dircache: bool }

struct Overlay {
    lower: PathBuf,
    upper: PathBuf,
    deny: GlobSet,
    deny_read: GlobSet,
    agent: String,
    events: Mutex<Option<BufWriter<File>>>,
    meta: RwLock<Meta>,
    handles: Mutex<HashMap<u64, Handle>>,
    /// Directory snapshots taken at opendir: (rel, entries with their inodes).
    dirs: Mutex<HashMap<u64, Snapshot>>,
    next_fh: AtomicU64,
    passthrough: AtomicBool,
    /// FUSE inode -> (real dev, real ino, backing). The kernel requires one backing file per inode.
    /// Kept registered after the last close, so reopening a file costs no backing ioctl.
    backing: Mutex<HashMap<u64, (u64, u64, Arc<BackingId>)>>,
    tune: Tuning,
    stats: Stats,
}

fn eno(e: &std::io::Error) -> Errno { Errno::from_i32(e.raw_os_error().unwrap_or(libc::EIO)) }

fn ts(secs: i64, nsecs: i64) -> SystemTime {
    if secs >= 0 { UNIX_EPOCH + Duration::new(secs as u64, nsecs as u32) } else { UNIX_EPOCH }
}

/// Entry for a negative dentry: node id 0 tells the kernel "does not exist, cache that".
fn neg_attr() -> FileAttr {
    FileAttr {
        ino: INodeNo(0), size: 0, blocks: 0, atime: UNIX_EPOCH, mtime: UNIX_EPOCH, ctime: UNIX_EPOCH, crtime: UNIX_EPOCH,
        kind: FileType::RegularFile, perm: 0, nlink: 0, uid: 0, gid: 0, rdev: 0, blksize: 4096, flags: 0,
    }
}

fn under<'a>(it: impl Iterator<Item = &'a PathBuf>, root: &Path) -> Vec<PathBuf> {
    it.filter(|p| p.starts_with(root)).cloned().collect()
}

fn rebase(p: &Path, from: &Path, to: &Path) -> PathBuf {
    let rest = p.strip_prefix(from).unwrap_or(Path::new(""));
    if rest.as_os_str().is_empty() { to.to_path_buf() } else { to.join(rest) } // join("") would add a trailing slash
}

impl Meta {
    fn ino_for(&mut self, rel: &Path) -> u64 {
        if let Some(i) = self.inodes.get(rel) { return *i; }
        let i = self.next_ino; self.next_ino += 1;
        self.paths.insert(i, rel.to_path_buf()); self.inodes.insert(rel.to_path_buf(), i);
        i
    }
    fn rel(&self, ino: u64) -> Option<&PathBuf> { self.paths.get(&ino) }
    fn child(&self, parent: u64, name: &OsStr) -> Option<PathBuf> { self.rel(parent).map(|p| p.join(name)) }
    fn hidden(&self, rel: &Path) -> bool {
        if rel.as_os_str() == WHITEOUTS { return true; }
        if self.whiteouts.is_empty() { return false; }
        rel.ancestors().any(|a| !a.as_os_str().is_empty() && self.whiteouts.contains(a))
    }
    fn move_tree(&mut self, from: &Path, to: &Path) {
        for p in under(self.inodes.keys(), from) {
            let i = self.inodes.remove(&p).unwrap();
            let np = rebase(&p, from, to);
            self.inodes.insert(np.clone(), i);
            self.paths.insert(i, np);
        }
        for p in under(self.up.iter(), from) { self.up.remove(&p); self.up.insert(rebase(&p, from, to)); }
    }
    /// Forget cached inode mappings for a path and everything below it (after rename/delete).
    /// `dir`: rel may have children in the map (a file never does, so it skips the scan).
    fn forget_tree(&mut self, rel: &Path, dir: bool) -> Vec<u64> {
        let gone = if dir { under(self.inodes.keys(), rel) } else { vec![rel.to_path_buf()] };
        let mut inos = vec![];
        for p in gone { if let Some(i) = self.inodes.remove(&p) { self.paths.remove(&i); inos.push(i); } }
        inos
    }
    fn up_remove_tree(&mut self, rel: &Path) {
        for p in under(self.up.iter(), rel) { self.up.remove(&p); }
    }
}

fn to_attr(ino: u64, m: &fs::Metadata) -> FileAttr {
    let kind = if m.file_type().is_symlink() { FileType::Symlink } else if m.is_dir() { FileType::Directory } else { FileType::RegularFile };
    FileAttr {
        ino: INodeNo(ino), size: m.size(), blocks: m.blocks(), atime: ts(m.atime(), m.atime_nsec()), mtime: ts(m.mtime(), m.mtime_nsec()),
        ctime: ts(m.ctime(), m.ctime_nsec()), crtime: ts(m.ctime(), m.ctime_nsec()), kind, perm: (m.mode() & 0o7777) as u16,
        nlink: m.nlink() as u32, uid: m.uid(), gid: m.gid(), rdev: m.rdev() as u32, blksize: 4096, flags: 0,
    }
}

fn watch_of(f: &File) -> Option<(u64, i64, i64)> { f.metadata().ok().map(|m| (m.size(), m.mtime(), m.mtime_nsec())) }

fn scan_upper(root: &Path, rel: &Path, out: &mut HashSet<PathBuf>) {
    let Ok(rd) = fs::read_dir(root.join(rel)) else { return };
    for e in rd.flatten() {
        let r = rel.join(e.file_name());
        if r.as_os_str() == WHITEOUTS { continue; }
        let dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        out.insert(r.clone());
        if dir { scan_upper(root, &r, out); }
    }
}

impl Overlay {
    fn new(lower: PathBuf, upper: PathBuf, deny: GlobSet, deny_read: GlobSet, agent: String, events: Option<File>) -> Self {
        let mut meta = Meta { paths: HashMap::new(), inodes: HashMap::new(), next_ino: 2, whiteouts: HashSet::new(), up: HashSet::new() };
        meta.paths.insert(1, PathBuf::new());
        meta.inodes.insert(PathBuf::new(), 1);
        meta.up.insert(PathBuf::new());
        scan_upper(&upper, Path::new(""), &mut meta.up);
        if let Ok(text) = fs::read_to_string(upper.join(WHITEOUTS)) {
            meta.whiteouts = text.lines().filter(|l| !l.is_empty()).map(PathBuf::from).collect();
        }
        let off = |k: &str| std::env::var_os(k).is_some();
        Overlay {
            lower, upper, deny, deny_read, agent, events: Mutex::new(events.map(BufWriter::new)), meta: RwLock::new(meta),
            handles: Mutex::new(HashMap::new()), dirs: Mutex::new(HashMap::new()), next_fh: AtomicU64::new(1),
            passthrough: AtomicBool::new(false), backing: Mutex::new(HashMap::new()),
            tune: Tuning { neg: !off("GITTA_NO_NEG"), upcache: !off("GITTA_NO_UPCACHE"), rdplus: !off("GITTA_NO_RDPLUS"), dircache: !off("GITTA_NO_DIRCACHE") },
            stats: Stats { on: off("GITTA_FS_STATS"), count: Default::default(), nanos: Default::default() },
        }
    }

    fn event(&self, op: &str, path: &Path, extra: Option<(&str, &str)>) {
        let mut g = self.events.lock().unwrap();
        if let Some(w) = g.as_mut() {
            let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
            let mut v = serde_json::json!({"t": t, "agent": self.agent, "op": op, "path": path.to_string_lossy()});
            if let Some((k, val)) = extra { v[k] = serde_json::Value::String(val.to_string()); }
            let _ = writeln!(w, "{}", v);
            let _ = w.flush();
        }
    }

    /// The real path currently backing `rel` and its metadata, upper first.
    fn resolve(&self, m: &Meta, rel: &Path) -> Option<(PathBuf, fs::Metadata)> {
        if rel.as_os_str() == WHITEOUTS { return None; }
        if !self.tune.upcache || m.up.contains(rel) {
            let u = self.upper.join(rel);
            if let Ok(md) = fs::symlink_metadata(&u) { return Some((u, md)); }
        }
        // A whiteout hides the lower path for good: a later file or directory of the same name lives
        // only in upper, and a recreated directory is opaque (no lower entries show through it).
        if m.hidden(rel) && rel.as_os_str() != "" { return None; }
        let l = self.lower.join(rel);
        fs::symlink_metadata(&l).ok().map(|md| (l, md))
    }
    fn real(&self, m: &Meta, rel: &Path) -> Option<PathBuf> { self.resolve(m, rel).map(|r| r.0) }
    fn in_upper(&self, m: &Meta, rel: &Path) -> bool {
        if self.tune.upcache { m.up.contains(rel) } else { fs::symlink_metadata(self.upper.join(rel)).is_ok() }
    }

    fn denied(&self, rel: &Path, op: &str) -> bool {
        if self.deny.is_match(rel) { self.event("BLOCKED", rel, Some(("attempted", op))); return true; }
        false
    }

    fn save_whiteouts(&self, m: &Meta) {
        let mut v: Vec<String> = m.whiteouts.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        v.sort();
        let _ = fs::write(self.upper.join(WHITEOUTS), v.join("\n") + "\n");
    }

    /// Ensure parent directories exist in upper (copying modes from lower).
    fn copy_up_dirs(&self, m: &mut Meta, rel: &Path) -> std::io::Result<()> {
        let mut acc = PathBuf::new();
        for comp in rel.parent().unwrap_or(Path::new("")).components() {
            acc.push(comp);
            if self.tune.upcache && m.up.contains(&acc) { continue; }
            let u = self.upper.join(&acc);
            if !u.exists() {
                match fs::create_dir(&u) { Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(e), _ => {} }
                if let Ok(md) = fs::metadata(self.lower.join(&acc)) { let _ = fs::set_permissions(&u, md.permissions()); }
            }
            m.up.insert(acc.clone());
        }
        Ok(())
    }

    /// Copy a lower file into upper before it is modified. Returns the upper path.
    fn copy_up(&self, m: &mut Meta, rel: &Path) -> std::io::Result<PathBuf> {
        let u = self.upper.join(rel);
        if self.in_upper(m, rel) { return Ok(u); }
        self.copy_up_dirs(m, rel)?;
        let l = self.lower.join(rel);
        let md = fs::symlink_metadata(&l)?;
        if md.file_type().is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(&l)?, &u)?;
        } else if md.is_dir() {
            fs::create_dir(&u)?; fs::set_permissions(&u, md.permissions())?;
        } else {
            fs::copy(&l, &u)?;
        }
        m.up.insert(rel.to_path_buf());
        Ok(u)
    }

    /// Attributes under a held write lock (mutating ops).
    fn attr_locked(&self, m: &mut Meta, rel: &Path) -> Result<FileAttr, Errno> {
        let (_, md) = self.resolve(m, rel).ok_or(Errno::ENOENT)?;
        Ok(to_attr(m.ino_for(rel), &md))
    }

    /// Attributes for lookups: stat under the read lock, take the write lock only to mint a new inode.
    fn attr_of(&self, m: RwLockReadGuard<'_, Meta>, rel: &Path) -> Result<FileAttr, Errno> {
        let (_, md) = self.resolve(&m, rel).ok_or(Errno::ENOENT)?;
        let known = m.inodes.get(rel).copied();
        drop(m);
        let ino = match known { Some(i) => i, None => self.meta.write().unwrap().ino_for(rel) };
        Ok(to_attr(ino, &md))
    }
    fn attr(&self, rel: &Path) -> Result<FileAttr, Errno> { self.attr_of(self.meta.read().unwrap(), rel) }

    fn rel(&self, ino: INodeNo) -> Option<PathBuf> { self.meta.read().unwrap().rel(ino.0).cloned() }

    fn add_handle(&self, ino: u64, file: File, rel: PathBuf, watch: Option<(u64, i64, i64)>, backing: Option<Arc<BackingId>>) -> u64 {
        let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);
        self.handles.lock().unwrap().insert(fh, Handle { ino, file: Arc::new(file), rel, watch, _backing: backing });
        fh
    }

    /// Finds or registers the backing file for a FUSE inode. None means: serve this handle in userspace.
    fn backing_for(&self, ino: u64, file: &File, open_backing: impl FnOnce(&File) -> std::io::Result<BackingId>) -> Option<Arc<BackingId>> {
        if !self.passthrough.load(Ordering::Relaxed) { return None; }
        let md = file.metadata().ok()?;
        let mut b = self.backing.lock().unwrap();
        if let Some((dev, rino, id)) = b.get(&ino) {
            if *dev == md.dev() && *rino == md.ino() { return Some(id.clone()); }
            // Still open on another real file (e.g. lower before a copy-up): the kernel would refuse it.
            if Arc::strong_count(id) > 1 { return None; }
        }
        match open_backing(file) {
            Ok(id) => { let id = Arc::new(id); b.insert(ino, (md.dev(), md.ino(), id.clone())); Some(id) }
            Err(e) => { eprintln!("gitta-fs: passthrough unavailable for inode {ino}: {e}"); None }
        }
    }

    /// Release cached backing registrations of inodes that left the view.
    fn drop_backing(&self, inos: &[u64]) {
        if inos.is_empty() { return; }
        let mut b = self.backing.lock().unwrap();
        for i in inos { b.remove(i); }
    }

    /// Flags for a handle served in userspace. With passthrough active the inode may be in
    /// passthrough mode in the kernel, which refuses cached opens; direct I/O needs no io mode.
    fn fallback_flags(&self) -> FopenFlags {
        if self.passthrough.load(Ordering::Relaxed) { FopenFlags::FOPEN_DIRECT_IO } else { FopenFlags::FOPEN_KEEP_CACHE }
    }

    /// Merged directory listing: upper entries, then lower entries not shadowed or whited out.
    fn list(&self, m: &Meta, rel: &Path) -> Vec<(OsString, FileType)> {
        let kind_of = |t: std::io::Result<fs::FileType>| match t { Ok(t) if t.is_symlink() => FileType::Symlink, Ok(t) if t.is_dir() => FileType::Directory, _ => FileType::RegularFile };
        let mut out: BTreeMap<OsString, FileType> = BTreeMap::new();
        if self.in_upper(m, rel) {
            if let Ok(rd) = fs::read_dir(self.upper.join(rel)) {
                for e in rd.flatten() {
                    if rel.as_os_str().is_empty() && e.file_name() == WHITEOUTS { continue; }
                    out.insert(e.file_name(), kind_of(e.file_type()));
                }
            }
        }
        if !m.hidden(rel) || rel.as_os_str().is_empty() {
            if let Ok(rd) = fs::read_dir(self.lower.join(rel)) {
                for e in rd.flatten() {
                    let name = e.file_name();
                    if out.contains_key(&name) || m.hidden(&rel.join(&name)) { continue; }
                    out.insert(name, kind_of(e.file_type()));
                }
            }
        }
        out.into_iter().collect()
    }

    /// Listing with inode numbers, "." and ".." first.
    fn snapshot(&self, ino: u64) -> Result<(PathBuf, Vec<(u64, FileType, OsString)>), Errno> {
        let (rel, listing) = {
            let m = self.meta.read().unwrap();
            let Some(rel) = m.rel(ino).cloned() else { return Err(Errno::ENOENT) };
            let l = self.list(&m, &rel);
            (rel, l)
        };
        let mut entries: Vec<(u64, FileType, OsString)> = Vec::with_capacity(listing.len() + 2);
        entries.push((ino, FileType::Directory, ".".into()));
        entries.push((1, FileType::Directory, "..".into()));
        {
            let mut m = self.meta.write().unwrap();
            for (name, kind) in listing {
                let cino = m.ino_for(&rel.join(&name));
                entries.push((cino, kind, name));
            }
        }
        Ok((rel, entries))
    }

    fn dir_snapshot(&self, ino: INodeNo, fh: FileHandle) -> Result<Snapshot, Errno> {
        if let Some(s) = self.dirs.lock().unwrap().get(&fh.0) { return Ok(s.clone()); }
        self.snapshot(ino.0).map(Arc::new)
    }
}

impl Drop for Overlay {
    fn drop(&mut self) { self.stats.print(); }
}

impl Filesystem for Overlay {
    fn init(&mut self, _r: &Request, config: &mut KernelConfig) -> std::io::Result<()> {
        let _ = config.add_capabilities(InitFlags::FUSE_ASYNC_READ | InitFlags::FUSE_PARALLEL_DIROPS | InitFlags::FUSE_CACHE_SYMLINKS);
        if self.tune.rdplus {
            let _ = config.add_capabilities(InitFlags::FUSE_DO_READDIRPLUS);
            if std::env::var_os("GITTA_RDPLUS_AUTO").is_some() { let _ = config.add_capabilities(InitFlags::FUSE_READDIRPLUS_AUTO); }
        }
        let _ = config.set_max_readahead(1 << 20);
        // Passthrough needs the capability and a stack depth >= 1; 2 also lets backing files live
        // on overlayfs (e.g. a container's root filesystem).
        if std::env::var_os("GITTA_NO_PASSTHROUGH").is_none()
            && config.add_capabilities(InitFlags::FUSE_PASSTHROUGH).is_ok()
            && config.set_max_stack_depth(2).is_ok()
        {
            self.passthrough.store(true, Ordering::Relaxed);
        }
        eprintln!("gitta-fs: passthrough {}", if self.passthrough.load(Ordering::Relaxed) { "on" } else { "off" });
        Ok(())
    }

    fn destroy(&mut self) { self.stats.print(); self.stats.on = false; }

    fn lookup(&self, _r: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let _t = self.stats.t(Op::Lookup);
        let m = self.meta.read().unwrap();
        let Some(c) = m.child(parent.0, name) else { return reply.error(Errno::ENOENT) };
        match self.attr_of(m, &c) {
            Ok(a) => reply.entry(&TTL, &a, Generation(0)),
            Err(Errno::ENOENT) if self.tune.neg => { self.stats.bump(Op::LookupNeg); reply.entry(&NEG_TTL, &neg_attr(), Generation(0)) }
            Err(e) => reply.error(e),
        }
    }

    fn getattr(&self, _r: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        let _t = self.stats.t(Op::Getattr);
        // fstat on an open handle: answer from the open file, which stays valid after its name was
        // unlinked or renamed over.
        let m = self.meta.read().unwrap();
        let Some(p) = m.rel(ino.0).cloned() else {
            drop(m);
            // The kernel does not pass the handle for fstat, so find any open handle of this inode.
            let f = self.handles.lock().unwrap().values().find(|h| h.ino == ino.0).map(|h| h.file.clone());
            return match f.map(|f| f.metadata()) { Some(Ok(md)) => reply.attr(&TTL, &to_attr(ino.0, &md)), _ => reply.error(Errno::ENOENT) };
        };
        match self.attr_of(m, &p) { Ok(a) => reply.attr(&TTL, &a), Err(e) => reply.error(e) }
    }

    fn setattr(&self, _r: &Request, ino: INodeNo, mode: Option<u32>, _uid: Option<u32>, _gid: Option<u32>, size: Option<u64>,
               atime: Option<TimeOrNow>, mtime: Option<TimeOrNow>, _ctime: Option<SystemTime>, fh: Option<FileHandle>,
               _crtime: Option<SystemTime>, _chgtime: Option<SystemTime>, _bkuptime: Option<SystemTime>, _flags: Option<BsdFileFlags>, reply: ReplyAttr) {
        let _t = self.stats.t(Op::Setattr);
        let Some(rel) = self.rel(ino) else { return reply.error(Errno::ENOENT) };
        if mode.is_none() && size.is_none() && atime.is_none() && mtime.is_none() {
            return match self.attr(&rel) { Ok(a) => reply.attr(&TTL, &a), Err(e) => reply.error(e) };
        }
        if self.denied(&rel, "setattr") { return reply.error(Errno::EACCES); }
        let mut m = self.meta.write().unwrap();
        let u = match self.copy_up(&mut m, &rel) { Ok(u) => u, Err(e) => return reply.error(eno(&e)) };
        if let Some(md) = mode { if let Err(e) = fs::set_permissions(&u, fs::Permissions::from_mode(md)) { return reply.error(eno(&e)); } }
        if let Some(sz) = size {
            let f = fh.and_then(|h| self.handles.lock().unwrap().get(&h.0).map(|h| h.file.clone()));
            let r = match f { Some(f) => f.set_len(sz), None => OpenOptions::new().write(true).open(&u).and_then(|f| f.set_len(sz)) };
            if let Err(e) = r { return reply.error(eno(&e)); }
            self.event("truncate", &rel, None);
        }
        if atime.is_some() || mtime.is_some() {
            let conv = |t: Option<TimeOrNow>| match t { Some(TimeOrNow::SpecificTime(s)) => s, _ => SystemTime::now() };
            if let Ok(f) = OpenOptions::new().write(true).open(&u).or_else(|_| File::open(&u)) {
                let _ = f.set_times(fs::FileTimes::new().set_accessed(conv(atime)).set_modified(conv(mtime)));
            }
        }
        match self.attr_locked(&mut m, &rel) { Ok(a) => reply.attr(&TTL, &a), Err(e) => reply.error(e) }
    }

    fn readlink(&self, _r: &Request, ino: INodeNo, reply: ReplyData) {
        let _t = self.stats.t(Op::Readlink);
        let real = { let m = self.meta.read().unwrap(); m.rel(ino.0).and_then(|p| self.real(&m, p)) };
        match real.map(fs::read_link) {
            Some(Ok(t)) => reply.data(t.as_os_str().as_bytes()),
            Some(Err(e)) => reply.error(eno(&e)),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn mkdir(&self, _r: &Request, parent: INodeNo, name: &OsStr, mode: u32, umask: u32, reply: ReplyEntry) {
        let _t = self.stats.t(Op::Mkdir);
        let mut m = self.meta.write().unwrap();
        let Some(rel) = m.child(parent.0, name) else { return reply.error(Errno::ENOENT) };
        if self.denied(&rel, "mkdir") { return reply.error(Errno::EACCES); }
        if self.real(&m, &rel).is_some() { return reply.error(Errno::EEXIST); }
        if let Err(e) = self.copy_up_dirs(&mut m, &rel).and_then(|_| fs::create_dir(self.upper.join(&rel))) { return reply.error(eno(&e)); }
        m.up.insert(rel.clone());
        let _ = fs::set_permissions(self.upper.join(&rel), fs::Permissions::from_mode(mode & !umask));
        self.event("mkdir", &rel, None);
        match self.attr_locked(&mut m, &rel) { Ok(a) => reply.entry(&TTL, &a, Generation(0)), Err(e) => reply.error(e) }
    }

    fn unlink(&self, _r: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let _t = self.stats.t(Op::Unlink);
        let mut m = self.meta.write().unwrap();
        let Some(rel) = m.child(parent.0, name) else { return reply.error(Errno::ENOENT) };
        if self.denied(&rel, "unlink") { return reply.error(Errno::EACCES); }
        if self.real(&m, &rel).is_none() { return reply.error(Errno::ENOENT); }
        if self.in_upper(&m, &rel) {
            if let Err(e) = fs::remove_file(self.upper.join(&rel)) { return reply.error(eno(&e)); }
            m.up.remove(&rel);
        }
        if fs::symlink_metadata(self.lower.join(&rel)).is_ok() { m.whiteouts.insert(rel.clone()); self.save_whiteouts(&m); }
        let gone = m.forget_tree(&rel, false);
        drop(m);
        self.drop_backing(&gone);
        self.event("delete", &rel, None);
        reply.ok()
    }

    fn rmdir(&self, _r: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let _t = self.stats.t(Op::Rmdir);
        let mut m = self.meta.write().unwrap();
        let Some(rel) = m.child(parent.0, name) else { return reply.error(Errno::ENOENT) };
        if self.denied(&rel, "rmdir") { return reply.error(Errno::EACCES); }
        if self.real(&m, &rel).is_none() { return reply.error(Errno::ENOENT); }
        if !self.list(&m, &rel).is_empty() { return reply.error(Errno::ENOTEMPTY); }
        if self.in_upper(&m, &rel) {
            if let Err(e) = fs::remove_dir_all(self.upper.join(&rel)) { return reply.error(eno(&e)); }
            m.up_remove_tree(&rel);
        }
        if self.lower.join(&rel).exists() { m.whiteouts.insert(rel.clone()); self.save_whiteouts(&m); }
        m.forget_tree(&rel, true);
        self.event("rmdir", &rel, None);
        reply.ok()
    }

    fn symlink(&self, _r: &Request, parent: INodeNo, link_name: &OsStr, target: &Path, reply: ReplyEntry) {
        let _t = self.stats.t(Op::Symlink);
        let mut m = self.meta.write().unwrap();
        let Some(rel) = m.child(parent.0, link_name) else { return reply.error(Errno::ENOENT) };
        if self.denied(&rel, "symlink") { return reply.error(Errno::EACCES); }
        if let Err(e) = self.copy_up_dirs(&mut m, &rel).and_then(|_| std::os::unix::fs::symlink(target, self.upper.join(&rel))) { return reply.error(eno(&e)); }
        m.up.insert(rel.clone());
        self.event("symlink", &rel, None);
        match self.attr_locked(&mut m, &rel) { Ok(a) => reply.entry(&TTL, &a, Generation(0)), Err(e) => reply.error(e) }
    }

    fn rename(&self, _r: &Request, parent: INodeNo, name: &OsStr, newparent: INodeNo, newname: &OsStr, _flags: RenameFlags, reply: ReplyEmpty) {
        let _t = self.stats.t(Op::Rename);
        let mut m = self.meta.write().unwrap();
        let (Some(from), Some(to)) = (m.child(parent.0, name), m.child(newparent.0, newname)) else { return reply.error(Errno::ENOENT) };
        if self.denied(&from, "rename") || self.denied(&to, "rename") { return reply.error(Errno::EACCES); }
        let Some((src_real, src_md)) = self.resolve(&m, &from) else { return reply.error(Errno::ENOENT) };
        if src_md.is_dir() && !src_real.starts_with(&self.upper) {
            return reply.error(Errno::EXDEV); // lower directories are not moved; tools fall back to copy+delete
        }
        let r = self.copy_up(&mut m, &from).and_then(|u| { self.copy_up_dirs(&mut m, &to)?; fs::rename(&u, self.upper.join(&to)) });
        if let Err(e) = r { return reply.error(eno(&e)); }
        if fs::symlink_metadata(self.lower.join(&from)).is_ok() && m.whiteouts.insert(from.clone()) { self.save_whiteouts(&m); }
        // The kernel keeps using the moved node's inode number, so its mapping moves with it.
        let dir = src_md.is_dir();
        let gone = m.forget_tree(&to, dir);
        if dir {
            m.up_remove_tree(&to);
            m.move_tree(&from, &to);
        } else {
            m.up.remove(&to);
            if m.up.remove(&from) { m.up.insert(to.clone()); }
            if let Some(i) = m.inodes.remove(&from) { m.inodes.insert(to.clone(), i); m.paths.insert(i, to.clone()); }
        }
        drop(m);
        self.drop_backing(&gone);
        self.event("rename", &from, Some(("to", &to.to_string_lossy())));
        reply.ok()
    }

    fn open(&self, _r: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        let _t = self.stats.t(Op::Open);
        let flags = flags.0;
        let Some(rel) = self.rel(ino) else { return reply.error(Errno::ENOENT) };
        let acc = flags & libc::O_ACCMODE;
        let writing = acc == libc::O_WRONLY || acc == libc::O_RDWR || flags & libc::O_TRUNC != 0;
        let path = if writing {
            if self.denied(&rel, "write") { return reply.error(Errno::EACCES); }
            let mut m = self.meta.write().unwrap(); // copy-up must not race a rename/unlink
            match self.copy_up(&mut m, &rel) { Ok(u) => u, Err(e) => return reply.error(eno(&e)) }
        } else {
            // Secrets: readable by nobody in the view, and every attempt is logged.
            if self.deny_read.is_match(&rel) { self.event("BLOCKED", &rel, Some(("attempted", "read"))); return reply.error(Errno::EACCES); }
            let m = self.meta.read().unwrap();
            match self.real(&m, &rel) { Some(p) => p, None => return reply.error(Errno::ENOENT) }
        };
        let mut o = OpenOptions::new();
        o.read(acc != libc::O_WRONLY).write(writing).custom_flags(flags & libc::O_TRUNC);
        let f = match o.open(&path) { Ok(f) => f, Err(e) => return reply.error(eno(&e)) };
        if flags & libc::O_TRUNC != 0 { self.event("truncate", &rel, None); }
        match self.backing_for(ino.0, &f, |f| reply.open_backing(f)) {
            Some(id) => {
                let watch = if writing { watch_of(&f) } else { None };
                let fh = self.add_handle(ino.0, f, rel, watch, Some(id.clone()));
                // FOPEN_KEEP_CACHE is refused (EIO) with passthrough; the backing file's page cache is used.
                reply.opened_passthrough(FileHandle(fh), FopenFlags::empty(), &id)
            }
            None => { let fh = self.add_handle(ino.0, f, rel, None, None); reply.opened(FileHandle(fh), self.fallback_flags()) }
        }
    }

    fn read(&self, _r: &Request, _ino: INodeNo, fh: FileHandle, offset: u64, size: u32, _f: OpenFlags, _l: Option<LockOwner>, reply: ReplyData) {
        let _t = self.stats.t(Op::Read);
        let Some(f) = self.handles.lock().unwrap().get(&fh.0).map(|h| h.file.clone()) else { return reply.error(Errno::EBADF) };
        let mut buf = vec![0u8; size as usize];
        match f.read_at(&mut buf, offset) { Ok(n) => reply.data(&buf[..n]), Err(e) => reply.error(eno(&e)) }
    }

    fn write(&self, _r: &Request, _ino: INodeNo, fh: FileHandle, offset: u64, data: &[u8], _wf: WriteFlags, _f: OpenFlags, _l: Option<LockOwner>, reply: ReplyWrite) {
        let _t = self.stats.t(Op::Write);
        let Some((f, rel)) = self.handles.lock().unwrap().get(&fh.0).map(|h| (h.file.clone(), h.rel.clone())) else { return reply.error(Errno::EBADF) };
        match f.write_at(data, offset) {
            Ok(n) => { self.event("write", &rel, None); reply.written(n as u32) }
            Err(e) => reply.error(eno(&e)),
        }
    }

    fn release(&self, _r: &Request, _ino: INodeNo, fh: FileHandle, _f: OpenFlags, _l: Option<LockOwner>, _flush: bool, reply: ReplyEmpty) {
        let _t = self.stats.t(Op::Release);
        let h = self.handles.lock().unwrap().remove(&fh.0);
        if let Some(h) = h {
            // Passthrough writes bypass this process: report one 'write' if the file changed while open.
            if let Some(before) = h.watch { if watch_of(&h.file) != Some(before) { self.event("write", &h.rel, None); } }
        }
        reply.ok()
    }

    fn fsync(&self, _r: &Request, _ino: INodeNo, fh: FileHandle, _datasync: bool, reply: ReplyEmpty) {
        let _t = self.stats.t(Op::Fsync);
        let f = self.handles.lock().unwrap().get(&fh.0).map(|h| h.file.clone());
        match f.map(|f| f.sync_all()) { Some(Err(e)) => reply.error(eno(&e)), _ => reply.ok() }
    }

    fn opendir(&self, _r: &Request, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        let _t = self.stats.t(Op::Opendir);
        let snap = match self.snapshot(ino.0) { Ok(s) => s, Err(e) => return reply.error(e) };
        let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);
        self.dirs.lock().unwrap().insert(fh, Arc::new(snap));
        let flags = if self.tune.dircache { FopenFlags::FOPEN_CACHE_DIR } else { FopenFlags::empty() };
        reply.opened(FileHandle(fh), flags)
    }

    fn releasedir(&self, _r: &Request, _ino: INodeNo, fh: FileHandle, _flags: OpenFlags, reply: ReplyEmpty) {
        let _t = self.stats.t(Op::Releasedir);
        self.dirs.lock().unwrap().remove(&fh.0);
        reply.ok()
    }

    fn readdir(&self, _r: &Request, ino: INodeNo, fh: FileHandle, offset: u64, mut reply: ReplyDirectory) {
        let _t = self.stats.t(Op::Readdir);
        let snap = match self.dir_snapshot(ino, fh) { Ok(s) => s, Err(e) => return reply.error(e) };
        for (i, (cino, kind, name)) in snap.1.iter().enumerate().skip(offset as usize) {
            if reply.add(INodeNo(*cino), (i + 1) as u64, *kind, name) { break; }
        }
        reply.ok()
    }

    fn readdirplus(&self, _r: &Request, ino: INodeNo, fh: FileHandle, offset: u64, mut reply: ReplyDirectoryPlus) {
        let _t = self.stats.t(Op::Readdirplus);
        let snap = match self.dir_snapshot(ino, fh) { Ok(s) => s, Err(e) => return reply.error(e) };
        let rel = &snap.0;
        let m = self.meta.read().unwrap();
        for (i, (cino, _kind, name)) in snap.1.iter().enumerate().skip(offset as usize) {
            let path = match name.as_bytes() {
                b"." => rel.clone(),
                b".." => rel.parent().map(Path::to_path_buf).unwrap_or_default(),
                _ => rel.join(name),
            };
            // Entries that vanished since opendir are skipped.
            let Some((_, md)) = self.resolve(&m, &path) else { continue };
            if reply.add(INodeNo(*cino), (i + 1) as u64, name, &TTL, &to_attr(*cino, &md), Generation(0)) { break; }
        }
        drop(m);
        reply.ok()
    }

    fn create(&self, _r: &Request, parent: INodeNo, name: &OsStr, mode: u32, umask: u32, flags: i32, reply: ReplyCreate) {
        let _t = self.stats.t(Op::Create);
        let mut m = self.meta.write().unwrap();
        let Some(rel) = m.child(parent.0, name) else { return reply.error(Errno::ENOENT) };
        if self.denied(&rel, "create") { return reply.error(Errno::EACCES); }
        if let Err(e) = self.copy_up_dirs(&mut m, &rel) { return reply.error(eno(&e)); }
        let u = self.upper.join(&rel);
        let acc = flags & libc::O_ACCMODE;
        let mut o = OpenOptions::new();
        o.write(true).read(acc != libc::O_WRONLY).create(true).truncate(true).mode(mode & !umask);
        let f = match o.open(&u) { Ok(f) => f, Err(e) => return reply.error(eno(&e)) };
        m.up.insert(rel.clone());
        self.event("create", &rel, None);
        let a = match self.attr_locked(&mut m, &rel) { Ok(a) => a, Err(e) => return reply.error(e) };
        drop(m);
        match self.backing_for(a.ino.0, &f, |f| reply.open_backing(f)) {
            Some(id) => {
                let watch = watch_of(&f);
                let fh = self.add_handle(a.ino.0, f, rel, watch, Some(id.clone()));
                reply.created_passthrough(&TTL, &a, Generation(0), FileHandle(fh), FopenFlags::empty(), &id)
            }
            None => { let fh = self.add_handle(a.ino.0, f, rel, None, None); reply.created(&TTL, &a, Generation(0), FileHandle(fh), self.fallback_flags()) }
        }
    }

    fn statfs(&self, _r: &Request, _ino: INodeNo, reply: ReplyStatfs) {
        let _t = self.stats.t(Op::Statfs);
        let c = std::ffi::CString::new(self.upper.as_os_str().as_bytes()).unwrap();
        let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 { return reply.error(Errno::EIO); }
        reply.statfs(s.f_blocks as u64, s.f_bfree as u64, s.f_bavail as u64, s.f_files as u64, s.f_ffree as u64, s.f_bsize as u32, 255, s.f_frsize as u32);
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut lower, mut upper, mut mount, mut events, mut agent) = (None, None, None, None, String::from("agent"));
    let mut deny = GlobSetBuilder::new();
    let mut deny_read = GlobSetBuilder::new();
    while let Some(a) = args.next() {
        let v = args.next().unwrap_or_else(|| { eprintln!("missing value for {a}"); std::process::exit(2) });
        match a.as_str() {
            "--lower" => lower = Some(PathBuf::from(v)),
            "--upper" => upper = Some(PathBuf::from(v)),
            "--mount" => mount = Some(PathBuf::from(v)),
            "--events" => events = Some(v),
            "--agent" => agent = v,
            "--deny" => { deny.add(Glob::new(&v).expect("bad glob")); }
            "--deny-read" => { let g = Glob::new(&v).expect("bad glob"); deny_read.add(g.clone()); deny.add(g); }
            _ => { eprintln!("unknown option {a}"); std::process::exit(2) }
        }
    }
    let (lower, upper, mount) = (lower.expect("--lower"), upper.expect("--upper"), mount.expect("--mount"));
    fs::create_dir_all(&upper).expect("create upper");
    let events = events.map(|p| OpenOptions::new().create(true).append(true).open(p).expect("open events"));
    let fs_ = Overlay::new(lower, upper, deny.build().expect("deny globs"), deny_read.build().expect("deny-read globs"), agent, events);
    let mut cfg = Config::default();
    cfg.mount_options = vec![MountOption::FSName("aggit".into()), MountOption::DefaultPermissions];
    if cfg!(target_os = "linux") { cfg.n_threads = Some(THREADS); cfg.clone_fd = true; }
    fuser::mount(fs_, &mount, &cfg).expect("mount");
}
