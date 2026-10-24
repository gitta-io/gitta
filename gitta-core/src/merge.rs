//! Structural three-way merge for Python, TypeScript/TSX, Go, Rust, Java, C#, Kotlin, C and C++ (port of the Python merge_ts prototype).
//!
//! Order: exact choice -> Git line merge -> structural merge on conflict. Source bytes are copied
//! from original spans, never regenerated. A container (module, class body, function body) is a
//! sequence of keyed items; each item owns the text from the previous item's end through its own
//! last line, so comments and blank lines travel with the item that follows. Per key: three-way
//! choice; both changed -> classes recurse, functions merge statement by statement, anything else
//! falls back to a line merge of that item only. Additions are anchored after the preceding
//! surviving base key of their own side. Reorders, duplicate keys, opaque changes and parse errors
//! are UNSUPPORTED; overlapping edits are CONFLICT. A clean Git merge that defines a name twice is a CONFLICT.

use crate::{body_node, children, is_trivia, key_of, lang_of, line_end, parser, Key, Lang};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::process::{Command, Stdio};
use tree_sitter::Node;

#[derive(Debug, Clone, PartialEq)]
pub enum MergeError { Conflict(String), Unsupported(String) }
use MergeError::*;

pub type MergeResult = Result<Option<Vec<u8>>, MergeError>;

fn choose(b: &Option<Vec<u8>>, h: &Option<Vec<u8>>, s: &Option<Vec<u8>>) -> Option<Option<Vec<u8>>> {
    if h == s { Some(h.clone()) } else if h == b { Some(s.clone()) } else if s == b { Some(h.clone()) } else { None }
}

/// Line-level three-way merge through `git merge-file` (no repository needed).
pub fn line_merge(base: &[u8], head: &[u8], sub: &[u8]) -> Result<Vec<u8>, MergeError> {
    if base.contains(&0) || head.contains(&0) || sub.contains(&0) { return Err(Unsupported("BINARY".into())); }
    // Unique per call: concurrent merges in one process must never share a scratch directory.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("gitta-lm-{}-{}-{n}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&dir).map_err(|e| Unsupported(e.to_string()))?;
    let (pb, ph, ps) = (dir.join("b"), dir.join("h"), dir.join("s"));
    std::fs::write(&pb, base).ok(); std::fs::write(&ph, head).ok(); std::fs::write(&ps, sub).ok();
    let out = Command::new("git").args(["merge-file", "-p"]).arg(&ph).arg(&pb).arg(&ps).stdin(Stdio::null()).output();
    let _ = std::fs::remove_dir_all(&dir);
    let out = out.map_err(|e| Unsupported(format!("git merge-file: {e}")))?;
    match out.status.code() { Some(0) => Ok(out.stdout), Some(1..=127) => Err(Conflict("TEXT_CONFLICT".into())), _ => Err(Unsupported("GIT_MERGE_ERROR".into())) }
}

/// Myers diff over lines: (base start, base end, replacement) hunks. None when the edit distance is too
/// large for the in-process path (the caller then falls back to `git merge-file`).
fn myers<T: PartialEq + Clone>(a: &[T], b: &[T]) -> Option<Vec<(usize, usize, Vec<T>)>> {
    // Common prefix and suffix are never part of a hunk.
    let pre = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let suf = a[pre..].iter().rev().zip(b[pre..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (a2, b2) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    let (n, m) = (a2.len() as isize, b2.len() as isize);
    if n == 0 && m == 0 { return Some(vec![]); }
    let max = n + m; let off = max as usize + 1;
    let mut v = vec![0isize; 2 * off + 2];
    let mut trace: Vec<Vec<isize>> = vec![];
    let mut found = None;
    'outer: for d in 0..=max {
        if d > 4000 { return None; }
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let i = (k + off as isize) as usize;
            let mut x = if k == -d || (k != d && v[i - 1] < v[i + 1]) { v[i + 1] } else { v[i - 1] + 1 };
            let mut y = x - k;
            while x < n && y < m && a2[x as usize] == b2[y as usize] { x += 1; y += 1; }
            v[i] = x;
            if x >= n && y >= m { found = Some(d); break 'outer; }
            k += 2;
        }
    }
    let d_end = found?;
    // Backtrack into a list of (op, a index, b index): 0 = equal, 1 = delete a, 2 = insert b.
    let (mut x, mut y) = (n, m);
    let mut ops: Vec<(u8, isize, isize)> = vec![];
    for d in (0..=d_end).rev() {
        let vv = &trace[d as usize];
        let k = x - y;
        let i = |k: isize| (k + off as isize) as usize;
        let prev_k = if d == 0 { k } else if k == -d || (k != d && vv[i(k - 1)] < vv[i(k + 1)]) { k + 1 } else { k - 1 };
        let prev_x = if d == 0 { 0 } else { vv[i(prev_k)] };
        let prev_y = prev_x - prev_k;
        while x > prev_x.max(0) && y > prev_y.max(0) && x > 0 && y > 0 && a2[(x - 1) as usize] == b2[(y - 1) as usize] { ops.push((0, x - 1, y - 1)); x -= 1; y -= 1; }
        if d > 0 { if x == prev_x { ops.push((2, x, y - 1)); } else { ops.push((1, x - 1, y)); } x = prev_x; y = prev_y; }
    }
    ops.reverse();
    let mut out: Vec<(usize, usize, Vec<T>)> = vec![];
    let mut cur: Option<(usize, usize, Vec<T>)> = None;
    for (op, xa, yb) in ops {
        let xa = xa as usize;
        match op {
            0 => { if let Some(h) = cur.take() { out.push(h); } }
            1 => { let h = cur.get_or_insert((xa, xa, vec![])); h.1 = xa + 1; }
            _ => { let h = cur.get_or_insert((xa, xa, vec![])); h.2.push(b2[yb as usize].clone()); }
        }
    }
    if let Some(h) = cur.take() { out.push(h); }
    Some(out.into_iter().map(|(s, e, r)| (s + pre, e + pre, r)).collect())
}

/// In-process three-way line merge. Edits that overlap or touch conflict (as in Git), unless both sides made
/// the identical edit. With `union_inserts`, two different insertions at the same point are both kept
/// (head first): right for changelogs and other append-style text, never used for code.
pub fn diff3(base: &[u8], head: &[u8], sub: &[u8], union_inserts: bool) -> Result<Vec<u8>, MergeError> {
    diff3_with(base, head, sub, |_, _| union_inserts)
}

/// `diff3` that keeps two different insertions at the same point (head first) when `union` accepts their lines.
fn diff3_with(base: &[u8], head: &[u8], sub: &[u8], union: impl Fn(&[Vec<u8>], &[Vec<u8>]) -> bool) -> Result<Vec<u8>, MergeError> {
    if base.contains(&0) || head.contains(&0) || sub.contains(&0) { return Err(Unsupported("BINARY".into())); }
    let lines = |x: &[u8]| -> Vec<Vec<u8>> { x.split_inclusive(|&c| c == b'\n').map(<[u8]>::to_vec).collect() };
    let (bl, hl, sl) = (lines(base), lines(head), lines(sub));
    let (Some(hh), Some(sh)) = (myers(&bl, &hl), myers(&bl, &sl)) else { return line_merge(base, head, sub) };
    let mut all: Vec<(usize, usize, Vec<Vec<u8>>)> = vec![];
    let mut used_s = vec![false; sh.len()];
    for (a1, a2, ar) in hh {
        let mut merged = (a1, a2, ar.clone());
        for (j, (b1, b2, br)) in sh.iter().enumerate() {
            let touch = a1 <= *b2 && *b1 <= a2;
            if !touch { continue; }
            if a1 == *b1 && a2 == *b2 && ar == *br { used_s[j] = true; continue; }
            if a1 == a2 && *b1 == *b2 && a1 == *b1 && union(&ar, br) { used_s[j] = true; merged.2.extend(br.iter().cloned()); continue; }
            return Err(Conflict("TEXT_CONFLICT".into()));
        }
        all.push(merged);
    }
    for (j, h) in sh.into_iter().enumerate() { if !used_s[j] { all.push(h); } }
    all.sort_by_key(|(a1, a2, _)| (*a1, a1 != a2));
    let (mut out, mut i) = (Vec::with_capacity(head.len().max(sub.len())), 0usize);
    for (a1, a2, r) in all { for x in &bl[i.min(a1)..a1] { out.extend_from_slice(x); } for x in r { out.extend_from_slice(&x); } i = i.max(a2); }
    for x in &bl[i..] { out.extend_from_slice(x); }
    Ok(out)
}

/// Two versions of a new text file with no common base: shared lines once, each side's own lines kept (head first).
fn union_two(h: &[u8], s: &[u8]) -> Result<Vec<u8>, MergeError> {
    // Paragraphs (blocks separated by a blank line), not lines: a section is kept together, never interleaved.
    let lines = |x: &[u8]| -> Vec<Vec<u8>> {
        let t = String::from_utf8_lossy(x);
        let mut out: Vec<Vec<u8>> = vec![];
        for part in t.split_inclusive("\n\n") { out.push(part.as_bytes().to_vec()); }
        if let Some(last) = out.last_mut() { if !last.ends_with(b"\n\n") { if !last.ends_with(b"\n") { last.push(b'\n'); } last.push(b'\n'); } }
        out
    };
    let (hl, sl) = (lines(h), lines(s));
    let hunks = myers(&hl, &sl).ok_or(Conflict("TEXT_CONFLICT".into()))?;
    let (mut out, mut i) = (vec![], 0usize);
    for (a1, a2, r) in hunks {
        for x in &hl[i..a2.max(a1)] { out.extend_from_slice(x); }   // head's lines up to and including its replaced run
        for x in &r { out.extend_from_slice(x); }                     // then the submitted side's lines
        i = a2.max(a1);
    }
    for x in &hl[i..] { out.extend_from_slice(x); }
    Ok(out)
}

/// Text files where both sides adding lines at the same place means "keep both" (changelogs, notes).
fn append_style_text(path: &str) -> bool {
    let f = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    f.starts_with("changelog") || f.starts_with("changes") || f.starts_with("history") || f.starts_with("news") || f.starts_with("release")
        || f.ends_with(".md") || f.ends_with(".rst") || f.ends_with(".txt") || f.ends_with(".adoc")
}

fn ini_style(path: &str) -> bool { [".cfg", ".ini", ".conf", ".properties", ".editorconfig"].iter().any(|e| path.ends_with(e)) || path.ends_with("setup.cfg") }

/// Does any `[section]` set the same key twice?
fn ini_duplicate_key(text: &[u8]) -> bool {
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    let mut section = String::new();
    for line in String::from_utf8_lossy(text).lines() {
        let l = line.trim();
        if l.starts_with('[') && l.ends_with(']') { section = l.to_string(); continue; }
        if line.starts_with([' ', '\t']) || l.is_empty() || l.starts_with(['#', ';']) { continue; }
        if let Some(i) = l.find(['=', ':']) { if !seen.insert((section.clone(), l[..i].trim().to_lowercase())) { return true; } }
    }
    false
}

#[derive(Clone)]
struct SpanItem<'t> { key: Key, start: usize, stop: usize, node: Node<'t> }

fn span_items<'t>(container: Node<'t>, src: &[u8], lang: Lang, start: usize, end: usize, unique: bool) -> Result<(Vec<SpanItem<'t>>, usize), MergeError> {
    let (mut out, mut cursor, mut covered, mut first) = (vec![], start, start, true);
    for child in children(container) {
        if child.start_byte() < covered || is_trivia(child.kind()) { continue; }
        if child.start_byte() < start { continue; }
        let key = key_of(child, src, lang, first); first = false;
        let stop = line_end(src, child.end_byte()).min(end);
        out.push(SpanItem { key, start: cursor, stop, node: child });
        cursor = stop; covered = stop;
    }
    let keys: BTreeSet<&Key> = out.iter().map(|i| &i.key).collect();
    if unique && keys.len() != out.len() { return Err(Unsupported("DUPLICATE_KEY".into())); }
    Ok((out, cursor))
}

/// A container: items plus the trailing text up to `end`.
struct Container<'t> { items: Vec<SpanItem<'t>>, tail_start: usize, end: usize }

fn text<'a>(src: &'a [u8], a: usize, b: usize) -> &'a [u8] { &src[a..b] }

fn merge_container(lang: Lang, bs: &[u8], b: &Container, hs: &[u8], h: &Container, ss: &[u8], s: &Container, tests: bool) -> Result<Vec<u8>, MergeError> {
    let bk: Vec<&Key> = b.items.iter().map(|i| &i.key).collect();
    let map = |c: &Container| -> BTreeMap<Key, usize> { c.items.iter().enumerate().map(|(n, i)| (i.key.clone(), n)).collect() };
    let (bm, hm, sm) = (map(b), map(h), map(s));
    // One side may reorder items (a pure move); the merged result then follows that side's order.
    // Both sides reordering is UNSUPPORTED.
    let mut order: Option<Vec<Key>> = None;
    for side in [h, s] {
        let kept: Vec<&Key> = side.items.iter().map(|i| &i.key).filter(|k| bm.contains_key(*k)).collect();
        let present: BTreeSet<&Key> = side.items.iter().map(|i| &i.key).collect();
        let expect: Vec<&Key> = bk.iter().copied().filter(|k| present.contains(*k)).collect();
        if kept != expect {
            if order.is_some() || kept.iter().any(|k| matches!(k, Key::Opaque(_))) { return Err(Unsupported("REORDER".into())); }
            order = Some(kept.into_iter().cloned().collect());
        }
        // New top-level statements are opaque; in test files appending them (asserts, cases) is safe to combine.
        for i in &side.items { if matches!(i.key, Key::Opaque(_)) && !bm.contains_key(&i.key) && !tests { return Err(Unsupported("OPAQUE_STATEMENT_CHANGED".into())); } }
    }
    for k in &bk { if matches!(k, Key::Opaque(_)) && (!hm.contains_key(*k) || !sm.contains_key(*k)) { return Err(Unsupported("OPAQUE_STATEMENT_CHANGED".into())); } }
    // Additions grouped by the base key they follow on their own side.
    let additions = |c: &Container, src: &[u8]| -> (BTreeMap<Option<Key>, Vec<(Key, Vec<u8>)>>, BTreeMap<Key, (Option<Key>, Vec<u8>)>) {
        let (mut groups, mut anchors, mut anchor): (BTreeMap<Option<Key>, Vec<(Key, Vec<u8>)>>, BTreeMap<Key, (Option<Key>, Vec<u8>)>, Option<Key>) = (BTreeMap::new(), BTreeMap::new(), None);
        for i in &c.items {
            if bm.contains_key(&i.key) { anchor = Some(i.key.clone()); continue; }
            let t = text(src, i.start, i.stop).to_vec();
            groups.entry(anchor.clone()).or_default().push((i.key.clone(), t.clone()));
            anchors.insert(i.key.clone(), (anchor.clone(), t));
        }
        (groups, anchors)
    };
    let ((ha, hanc), (sa, sanc)) = (additions(h, hs), additions(s, ss));
    // Both sides added the same name. Identical: once. Two classes of the same name (two agents creating the same
    // test class, say): one class with the members of both, if no member is defined differently. Else a conflict.
    let mut fixed: BTreeMap<Key, Vec<u8>> = BTreeMap::new();
    for (k, v) in &hanc {
        if let Some(w) = sanc.get(k) {
            if v == w { continue; }
            match (k, h.items.iter().find(|i| &i.key == k).cloned(), s.items.iter().find(|i| &i.key == k).cloned()) {
                (Key::Class(_), Some(hi), Some(si)) => { fixed.insert(k.clone(), class_union(lang, hs, &hi, ss, &si)?); }
                _ => return Err(Conflict("ADD_ADD".into())),
            }
        }
    }
    let mut out: Vec<u8> = vec![];
    let emit = |out: &mut Vec<u8>, anchor: &Option<Key>| {
        for (k, t) in ha.get(anchor).into_iter().flatten() { out.extend_from_slice(fixed.get(k).unwrap_or(t)); }
        for (k, t) in sa.get(anchor).into_iter().flatten() { if !hanc.contains_key(k) { out.extend_from_slice(t); } }
    };
    emit(&mut out, &None);
    let walk: Vec<&Key> = match &order {
        Some(o) => { let mut w: Vec<&Key> = o.iter().collect(); for k in &bk { if !o.contains(*k) { w.push(*k); } } w }
        None => bk.clone(),
    };
    for k in walk {
        let n = bm[k];
        let bi = &b.items[n];
        let bt = Some(text(bs, bi.start, bi.stop).to_vec());
        let hi = hm.get(k).map(|&i| &h.items[i]); let si = sm.get(k).map(|&i| &s.items[i]);
        let ht = hi.map(|i| text(hs, i.start, i.stop).to_vec()); let st = si.map(|i| text(ss, i.start, i.stop).to_vec());
        let v = match choose(&bt, &ht, &st) {
            Some(v) => v,
            None => {
                let (Some(hi), Some(si)) = (hi, si) else { return Err(Conflict("DELETE_MODIFY".into())) };
                let merged = diff3(bt.as_ref().unwrap(), ht.as_ref().unwrap(), st.as_ref().unwrap(), false);
                match merged {
                    Ok(m) => Some(m),
                    Err(_) => Some(match k {
                        Key::Class(_) => merge_block(lang, bs, bi, hs, hi, ss, si, "body")?,
                        Key::Def(_) | Key::Member(_, _) if is_fn(k) => merge_block(lang, bs, bi, hs, hi, ss, si, "body").map_err(|e| { if std::env::var("GITTA_DEBUG").is_ok() { eprintln!("merge_block: {e:?}"); } Conflict("SAME_SYMBOL_EDIT".into()) })?,
                        Key::Import(_) => import_union(bt.as_ref().unwrap(), ht.as_ref().unwrap(), st.as_ref().unwrap()).ok_or(Conflict("SAME_IMPORT_EDIT".into()))?,
                        Key::Assign(_) => literal_union(lang, bt.as_ref().unwrap(), ht.as_ref().unwrap(), st.as_ref().unwrap()).ok_or(Conflict("SAME_SYMBOL_EDIT".into()))?,
                        _ => return Err(Conflict("SAME_SYMBOL_EDIT".into())),
                    }),
                }
            }
        };
        if let Some(v) = v { out.extend_from_slice(&v); }
        emit(&mut out, &Some(k.clone()));
    }
    let (btail, htail, stail) = (text(bs, b.tail_start, b.end).to_vec(), text(hs, h.tail_start, h.end).to_vec(), text(ss, s.tail_start, s.end).to_vec());
    match choose(&Some(btail), &Some(htail), &Some(stail)) { Some(Some(t)) => out.extend_from_slice(&t), _ => return Err(Unsupported("TAIL_CHANGED".into())) }
    Ok(out)
}

/// Where an import statement imports from: `from './x.js'` / `from pkg import` / `import pkg`.
fn import_source(t: &str) -> Option<String> {
    let t = t.trim();
    if let Some(i) = t.rfind(" from ") {
        let rest = t[i + 6..].trim().trim_end_matches(';').trim();
        if rest.starts_with(['\'', '"']) { return Some(rest.trim_matches(['\'', '"']).to_string()); }
    }
    if let Some(r) = t.strip_prefix("from ") { return r.split_whitespace().next().map(String::from); }
    None
}

/// An import line one side changed is the same import as the base one from the same source (not a delete
/// plus an add), so two sides adding different names to one import line can be combined.
fn align_imports(base: &[SpanItem], bsrc: &[u8], side: &mut [SpanItem], ssrc: &[u8]) {
    let src_of = |src: &[u8], it: &SpanItem| import_source(&String::from_utf8_lossy(&src[it.start..it.stop]));
    let present: BTreeSet<Key> = side.iter().map(|i| i.key.clone()).collect();
    for b in base.iter().filter(|b| matches!(b.key, Key::Import(_)) && !present.contains(&b.key)) {
        let Some(bsource) = src_of(bsrc, b) else { continue };
        let base_keys: BTreeSet<&Key> = base.iter().map(|x| &x.key).collect();
        let cands: Vec<usize> = side.iter().enumerate().filter(|(_, i)| matches!(i.key, Key::Import(_)) && !base_keys.contains(&i.key) && src_of(ssrc, i).as_deref() == Some(bsource.as_str())).map(|(n, _)| n).collect();
        if let [one] = cands[..] { side[one].key = b.key.clone(); }
    }
}

/// Two sides added different names to the same import line: keep base names, then head's, then submitted's.
fn import_union(b: &[u8], h: &[u8], s: &[u8]) -> Option<Vec<u8>> {
    let (b, h, s) = (std::str::from_utf8(b).ok()?, std::str::from_utf8(h).ok()?, std::str::from_utf8(s).ok()?);
    // (text before the name list, names, text after)
    let split = |t: &str| -> Option<(String, Vec<String>, String)> {
        if let (Some(o), Some(c)) = (t.find('{'), t.rfind('}')) {
            if o < c { return Some((t[..o + 1].to_string(), t[o + 1..c].split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect(), t[c..].to_string())); }
        }
        let i = t.find(" import ")?;
        if t.contains('(') || !t.trim_start().starts_with("from ") { return None; }
        let end = t.trim_end().len();
        Some((t[..i + 8].to_string(), t[i + 8..end].split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect(), t[end..].to_string()))
    };
    let ((bp, bn, ba), (hp, hn, ha), (sp, sn, sa)) = (split(b)?, split(h)?, split(s)?);
    if bp != hp || bp != sp || ba != ha || ba != sa { return None; }
    // Names only added, never removed or renamed.
    if !bn.iter().all(|x| hn.contains(x) && sn.contains(x)) { return None; }
    let mut out = bn.clone();
    for x in hn.iter().chain(sn.iter()) { if !out.contains(x) { out.push(x.clone()); } }
    let braces = hp.ends_with('{');
    let body = if braces { format!(" {} ", out.join(", ")) } else { out.join(", ") };
    Some(format!("{hp}{body}{ha}").into_bytes())
}

/// Both sides added different keys to one object or dict literal at the same place: keep both, refusing
/// anything that would set one key twice. Both insertions are kept only when each is whole key-value entries:
/// two sides inserting statements, shorthand properties or lines of a string at one place conflict, as in Git.
fn literal_union(lang: Lang, b: &[u8], h: &[u8], s: &[u8]) -> Option<Vec<u8>> {
    if !b.contains(&b'{') { return None; }
    let m = diff3_with(b, h, s, |x, y| whole_entries(lang, x) && whole_entries(lang, y)).ok()?;
    let keys = |x: &[u8]| -> Vec<String> {
        String::from_utf8_lossy(x).lines().filter_map(|l| {
            let l = l.trim();
            let k: String = if l.starts_with(['"', '\'']) { let q = l.chars().next().unwrap(); l[1..].split(q).next().unwrap_or("").to_string() }
                            else { l.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$').collect() };
            (!k.is_empty() && l[l.find(&k[..]).unwrap_or(0) + k.len()..].trim_start_matches(['"', '\'']).trim_start().starts_with(':')).then_some(k)
        }).collect()
    };
    let km = keys(&m);
    let set: BTreeSet<&String> = km.iter().collect();
    if set.len() != km.len() { return None; }
    let want: BTreeSet<String> = keys(b).into_iter().chain(keys(h)).chain(keys(s)).collect();
    (set.into_iter().cloned().collect::<BTreeSet<String>>() == want).then_some(m)
}

/// Lines that are one or more whole key-value entries of an object or dict literal (TypeScript, Python) and
/// otherwise only comments: no statement, shorthand property, spread or part of a longer value.
fn whole_entries(lang: Lang, lines: &[Vec<u8>]) -> bool {
    let (open, close, path): (&[u8], &[u8], &[&str]) = match lang {
        Lang::TypeScript | Lang::Tsx => (b"({\n", b"\n});\n", &["expression_statement", "parenthesized_expression", "object"]),
        Lang::Python => (b"{\n", b"\n}\n", &["expression_statement", "dictionary"]),
        _ => return false,
    };
    let Some(t) = parser(lang).parse([open, &lines.concat(), close].concat(), None) else { return false };
    if t.root_node().has_error() { return false; }
    let mut n = t.root_node();
    for kind in path { match children(n)[..] { [c] if c.kind() == *kind => n = c, _ => return false } }
    let entries: Vec<Node> = children(n).into_iter().filter(|c| !is_trivia(c.kind())).collect();
    !entries.is_empty() && entries.iter().all(|e| e.kind() == "pair")
}

fn is_fn(k: &Key) -> bool { match k { Key::Def(_) => true, Key::Member(_, i) => matches!(**i, Key::Def(_)), _ => false } }

fn unwrap_decl<'t>(n: Node<'t>, lang: Lang) -> Node<'t> {
    let mut x = n;
    if lang == Lang::Python && x.kind() == "decorated_definition" { if let Some(d) = x.child_by_field_name("definition") { x = d; } }
    if lang != Lang::Python && x.kind() == "export_statement" { if let Some(d) = x.child_by_field_name("declaration") { x = d; } }
    x
}

/// Split a class or function item into header, inner container and closing text.
fn block_parts<'t>(lang: Lang, src: &[u8], it: &SpanItem<'t>, field: &str) -> Result<(usize, Container<'t>), MergeError> {
    let decl = unwrap_decl(it.node, lang);
    let mut body = body_node(decl, lang, field).ok_or(Unsupported("NO_BODY".into()))?;
    // Go blocks wrap their statements in a statement_list.
    if let [only] = children(body).into_iter().filter(|c| !is_trivia(c.kind())).collect::<Vec<_>>()[..] { if only.kind() == "statement_list" { body = only; } }
    let first = children(body).into_iter().find(|c| !is_trivia(c.kind())).ok_or(Unsupported("EMPTY_BODY".into()))?;
    let body_start = src[..first.start_byte()].iter().rposition(|&b| b == b'\n').map(|i| i + 1).unwrap_or(0);
    if body_start <= decl.start_byte() { return Err(Unsupported("ONE_LINE_BODY".into())); }
    // Function bodies are statement sequences (names may repeat); class bodies are keyed members.
    let (items, tail_start) = span_items(body, src, lang, body_start, it.stop, !is_fn(&it.key))?;
    Ok((body_start, Container { items, tail_start, end: it.stop }))
}

/// Two new classes of the same name: head's class with the submitted side's extra members appended.
fn class_union(lang: Lang, hs: &[u8], hi: &SpanItem, ss: &[u8], si: &SpanItem) -> Result<Vec<u8>, MergeError> {
    let (hb, hc) = block_parts(lang, hs, hi, "body")?;
    let (sb, sc) = block_parts(lang, ss, si, "body")?;
    let trim = |x: &[u8]| String::from_utf8_lossy(x).trim().to_string();
    if trim(&hs[hi.start..hb]) != trim(&ss[si.start..sb]) { return Err(Conflict("CLASS_HEADER".into())); }
    let mut out = hs[hi.start..hb].to_vec();
    let hkeys: BTreeMap<&Key, &SpanItem> = hc.items.iter().map(|i| (&i.key, i)).collect();
    for i in &hc.items { out.extend_from_slice(&hs[i.start..i.stop]); }
    for i in &sc.items {
        match hkeys.get(&i.key) {
            Some(h) => if trim(&hs[h.start..h.stop]) != trim(&ss[i.start..i.stop]) { return Err(Conflict("CLASS_MEMBER".into())); },
            None => out.extend_from_slice(&ss[i.start..i.stop]),
        }
    }
    out.extend_from_slice(&hs[hc.tail_start..hc.end]);
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn merge_block(lang: Lang, bs: &[u8], b: &SpanItem, hs: &[u8], h: &SpanItem, ss: &[u8], s: &SpanItem, field: &str) -> Result<Vec<u8>, MergeError> {
    let (bb, bc) = block_parts(lang, bs, b, field)?; let (hb, hc) = block_parts(lang, hs, h, field)?; let (sb, sc) = block_parts(lang, ss, s, field)?;
    let header = choose(&Some(bs[b.start..bb].to_vec()), &Some(hs[h.start..hb].to_vec()), &Some(ss[s.start..sb].to_vec()))
        .flatten().ok_or(Conflict("HEADER_CONFLICT".into()))?;
    let is_func = is_fn(&b.key);
    let inner = if is_func { merge_statements(bs, &bc, hs, &hc, ss, &sc)? } else { merge_container(lang, bs, &bc, hs, &hc, ss, &sc, false)? };
    Ok([header, inner].concat())
}

/// diff3 over a function's top-level statements: edits to different statements combine even when adjacent.
fn merge_statements(bs: &[u8], b: &Container, hs: &[u8], h: &Container, ss: &[u8], s: &Container) -> Result<Vec<u8>, MergeError> {
    let seq = |src: &[u8], c: &Container| -> Vec<Vec<u8>> { c.items.iter().map(|i| src[i.start..i.stop].to_vec()).collect() };
    let (bq, hq, sq) = (seq(bs, b), seq(hs, h), seq(ss, s));
    let (hh, sh) = (hunks(&bq, &hq), hunks(&bq, &sq));
    for (a1, a2, ar) in &hh { for (b1, b2, br) in &sh {
        let overlap = (a1 < b2 && b1 < a2) || (a1 == a2 && a1 == b1 && b1 == b2);
        if overlap && !(a1 == b1 && a2 == b2 && ar == br) { return Err(Conflict("SAME_STATEMENT".into())); }
    } }
    let mut all: BTreeMap<(usize, usize), Vec<Vec<u8>>> = BTreeMap::new();
    for (a1, a2, r) in hh.into_iter().chain(sh) { all.insert((a1, a2), r); }
    let mut ordered: Vec<((usize, usize), Vec<Vec<u8>>)> = all.into_iter().collect();
    ordered.sort_by_key(|((a1, a2), _)| (*a1, a1 != a2));
    let (mut out, mut i) = (vec![], 0usize);
    for ((a1, a2), r) in ordered { for x in &bq[i.min(a1)..a1] { out.push(x.clone()); } out.extend(r); i = i.max(a2); }
    out.extend(bq[i..].iter().cloned());
    let tails = choose(&Some(bs[b.tail_start..b.end].to_vec()), &Some(hs[h.tail_start..h.end].to_vec()), &Some(ss[s.tail_start..s.end].to_vec()))
        .flatten().ok_or(Conflict("FUNCTION_TAIL".into()))?;
    Ok([out.concat(), tails].concat())
}

/// Minimal edit hunks (base start, base end, replacement) between two sequences (LCS based).
fn hunks(a: &[Vec<u8>], b: &[Vec<u8>]) -> Vec<(usize, usize, Vec<Vec<u8>>)> {
    let (n, m) = (a.len(), b.len());
    let mut l = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() { for j in (0..m).rev() { l[i][j] = if a[i] == b[j] { l[i + 1][j + 1] + 1 } else { l[i + 1][j].max(l[i][j + 1]) }; } }
    let (mut i, mut j, mut out) = (0, 0, vec![]);
    let (mut hs, mut hb): (Option<usize>, Vec<Vec<u8>>) = (None, vec![]);
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            if let Some(st) = hs.take() { out.push((st, i, std::mem::take(&mut hb))); }
            i += 1; j += 1;
        } else if j < m && (i == n || l[i][j + 1] >= l[i + 1][j]) { hs.get_or_insert(i); hb.push(b[j].clone()); j += 1; }
        else { hs.get_or_insert(i); i += 1; }
    }
    if let Some(st) = hs { out.push((st, n, hb)); }
    out
}

fn module(lang: Lang, src: &[u8]) -> Result<tree_sitter::Tree, MergeError> {
    // C and C++: conditional directives are blanked first (same offsets), so code in #ifdef branches is keyed
    // like any other; the merge still copies the original bytes. A file that does not parse cleanly even then
    // (macros used as statements) is merged by lines only.
    let t = if matches!(lang, Lang::C | Lang::Cpp) { parser(lang).parse(crate::c_cpp::prepare(src), None) } else { parser(lang).parse(src, None) };
    let t = t.ok_or(Unsupported("PARSE_ERROR".into()))?;
    if t.root_node().has_error() { return Err(Unsupported("PARSE_ERROR".into())); }
    Ok(t)
}

fn def_keys(lang: Lang, src: &[u8]) -> Option<Vec<Key>> {
    let t = module(lang, src).ok()?;
    Some(children(t.root_node()).into_iter().filter(|c| !is_trivia(c.kind())).map(|c| key_of(c, src, lang, false))
        .filter(|k| matches!(k, Key::Def(_) | Key::Class(_))).collect())
}

fn has_dupes(v: &[Key]) -> bool { v.iter().collect::<BTreeSet<_>>().len() != v.len() }

/// JSON objects: disjoint keys combine recursively; same key changed differently conflicts.
/// Numbers keep their exact text (arbitrary precision); key order follows base, then head, then submitted.
/// Output is re-serialized with 2-space indentation (documented normalization).
fn merge_json_objects(b: &[u8], h: &[u8], s: &[u8]) -> MergeResult {
    use serde_json::Value;
    let parse = |x: &[u8]| -> Result<Value, MergeError> {
        let v: Value = serde_json::from_slice(x).map_err(|e| Unsupported(format!("JSON_PARSE_ERROR {e}")))?;
        Ok(v)
    };
    let (bv, hv, sv) = (parse(b)?, parse(h)?, parse(s)?);
    fn m(b: Option<&Value>, h: Option<&Value>, s: Option<&Value>) -> Result<Option<Value>, MergeError> {
        if h == s { return Ok(h.cloned()); }
        if h == b { return Ok(s.cloned()); }
        if s == b { return Ok(h.cloned()); }
        match (b, h, s) {
            (Some(Value::Object(bo)), Some(Value::Object(ho)), Some(Value::Object(so))) => {
                let mut keys: Vec<&String> = vec![];
                for k in bo.keys().chain(ho.keys()).chain(so.keys()) { if !keys.contains(&k) { keys.push(k); } }
                let mut out = serde_json::Map::new();
                for k in keys { if let Some(v) = m(bo.get(k), ho.get(k), so.get(k))? { out.insert(k.clone(), v); } }
                Ok(Some(Value::Object(out)))
            }
            // Both sides only appended to the same list: keep base, then head's additions, then submitted's new ones.
            (Some(Value::Array(ba)), Some(Value::Array(ha)), Some(Value::Array(sa))) if ha.starts_with(ba) && sa.starts_with(ba) => {
                // Numbered entries (migrations, steps): both sides claiming the same number is a real clash.
                let num = |v: &Value| v.as_str().map(|t| t.chars().take_while(char::is_ascii_digit).collect::<String>()).filter(|n| !n.is_empty());
                let hn: BTreeSet<String> = ha[ba.len()..].iter().filter_map(num).collect();
                if sa[ba.len()..].iter().filter(|x| !ha[ba.len()..].contains(x)).filter_map(num).any(|n| hn.contains(&n)) { return Err(Conflict("SEQUENCE_NUMBER_CLASH".into())); }
                let mut out = ha.clone();
                for x in &sa[ba.len()..] { if !out[ba.len()..].contains(x) { out.push(x.clone()); } }
                Ok(Some(Value::Array(out)))
            }
            _ => Err(Conflict("JSON_VALUE_COLLISION".into())),
        }
    }
    if !(bv.is_object() && hv.is_object() && sv.is_object()) { return Err(Unsupported("JSON_ROOT_NOT_OBJECT".into())); }
    let v = m(Some(&bv), Some(&hv), Some(&sv))?.unwrap_or(Value::Null);
    let mut out = serde_json::to_string_pretty(&v).map_err(|e| Unsupported(e.to_string()))?.into_bytes();
    out.push(b'\n');
    Ok(Some(out))
}

/// pip requirement files: entries keyed by package name; disjoint changes combine, the same package
/// changed differently conflicts; comments, options and markers are unsupported.
fn merge_requirements(b: &[u8], h: &[u8], s: &[u8]) -> MergeResult {
    let entries = |x: &[u8]| -> Result<Vec<(String, String)>, MergeError> {
        let mut v: Vec<(String, String)> = vec![];
        for line in String::from_utf8_lossy(x).lines() {
            let l = line.trim();
            if l.is_empty() { continue; }
            if l.starts_with('#') || l.starts_with('-') || l.contains(';') { return Err(Unsupported("REQUIREMENTS_SYNTAX".into())); }
            let name: String = l.chars().take_while(|c| c.is_alphanumeric() || "._-[]".contains(*c)).collect();
            if name.is_empty() { return Err(Unsupported("REQUIREMENTS_SYNTAX".into())); }
            let key = name.to_lowercase().replace('_', "-");
            if v.iter().any(|(k, _)| *k == key) { return Err(Unsupported("REQUIREMENTS_DUPLICATE".into())); }
            v.push((key, l.to_string()));
        }
        Ok(v)
    };
    let (be, he, se) = (entries(b)?, entries(h)?, entries(s)?);
    // A dependency removed on one side while the other side adds new ones: the new packages may need it.
    let keys_of = |e: &Vec<(String, String)>| -> BTreeSet<String> { e.iter().map(|(k, _)| k.clone()).collect() };
    let (bk, hk, sk) = (keys_of(&be), keys_of(&he), keys_of(&se));
    for (x, y) in [(&hk, &sk), (&sk, &hk)] {
        let removed = bk.difference(x).next().is_some();
        let added = y.difference(&bk).next().is_some();
        if removed && added { return Err(Conflict("DEPENDENCY_REMOVED_WHILE_ADDED".into())); }
    }
    let get = |e: &Vec<(String, String)>, k: &str| e.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone());
    let mut keys: Vec<String> = vec![];
    for (k, _) in be.iter().chain(he.iter()).chain(se.iter()) { if !keys.contains(k) { keys.push(k.clone()); } }
    let mut out = String::new();
    for k in keys {
        let (bv, hv, sv) = (get(&be, &k), get(&he, &k), get(&se, &k));
        let v = if hv == sv { hv } else if hv == bv { sv } else if sv == bv { hv } else { return Err(Conflict(format!("REQUIREMENT {k}"))) };
        if let Some(v) = v { out.push_str(&v); out.push('\n'); }
    }
    Ok(Some(out.into_bytes()))
}

/// Merge one file. `None` means absent (deleted or not present).
pub fn merge_file(path: &str, base: Option<&[u8]>, head: Option<&[u8]>, sub: Option<&[u8]>) -> MergeResult {
    let (b, h, s) = (base.map(<[u8]>::to_vec), head.map(<[u8]>::to_vec), sub.map(<[u8]>::to_vec));
    if let Some(v) = choose(&b, &h, &s) { return Ok(v); }
    // Both sides created the same file: merge them as two additions to an empty file (code files only).
    if let (None, Some(h), Some(s)) = (&b, &h, &s) {
        // Both created the same changelog/notes/markdown file: lines both have once, the rest of each kept.
        if lang_of(path).is_none() && append_style_text(path) { return union_two(h, s).map(Some); }
    }
    let b = match (b, &h, &s) { (None, Some(_), Some(_)) if lang_of(path).is_some() => Some(vec![]), (b, _, _) => b };
    let (Some(b), Some(h), Some(s)) = (b, h, s) else { return Err(Conflict("ADD_ADD_OR_DELETE_MODIFY".into())) };
    let lang = crate::lang_for(path, &b);
    let file = path.rsplit('/').next().unwrap_or(path);
    if file.starts_with("requirements") && file.ends_with(".txt") {
        match merge_requirements(&b, &h, &s) { Err(Unsupported(_)) => {}, r => return r }
    }
    match diff3(&b, &h, &s, false) {
        Ok(m) => {
            // A clean text merge can still define a name twice (both sides added it in different places).
            if let Some(lang) = lang {
                if let Some(keys) = def_keys(lang, &m) {
                    let input_dupes = [&b, &h, &s].iter().any(|x| def_keys(lang, x).map(|k| has_dupes(&k)).unwrap_or(false));
                    if has_dupes(&keys) && !input_dupes { return Err(Conflict("DUPLICATE_DEFINITION".into())); }
                }
            }
            return Ok(Some(m));
        }
        Err(Conflict(_)) => {}
        Err(e) => return Err(e),
    }
    if path.ends_with(".json") { return merge_json_objects(&b, &h, &s); }
    let Some(lang) = lang else {
        if append_style_text(path) { return diff3(&b, &h, &s, true).map(Some); }
        if ini_style(path) {
            // Both added settings at the same place: keep both, unless that sets one key twice in a section.
            let m = diff3(&b, &h, &s, true)?;
            if ini_duplicate_key(&m) && !ini_duplicate_key(&b) { return Err(Conflict("SETTING_SET_TWICE".into())); }
            return Ok(Some(m));
        }
        return Err(Conflict("TEXT_CONFLICT".into()));
    };
    for v in [&b, &h, &s] { if std::str::from_utf8(v).is_err() { return Err(Unsupported("NON_UTF8".into())); } }
    let (tb, th, ts) = (module(lang, &b)?, module(lang, &h)?, module(lang, &s)?);
    let (bi, bt) = span_items(tb.root_node(), &b, lang, 0, b.len(), true)?;
    let (hi, ht) = span_items(th.root_node(), &h, lang, 0, h.len(), true)?;
    let (mut si, st) = span_items(ts.root_node(), &s, lang, 0, s.len(), true)?;
    let mut hi = hi;
    align_imports(&bi, &b, &mut hi, &h); align_imports(&bi, &b, &mut si, &s);
    let merged = merge_container(lang, &b, &Container { items: bi, tail_start: bt, end: b.len() }, &h, &Container { items: hi, tail_start: ht, end: h.len() },
                                 &s, &Container { items: si, tail_start: st, end: s.len() }, crate::guard::is_test(path))?;
    module(lang, &merged).map_err(|_| Conflict("MERGED_DOES_NOT_PARSE".into()))?;
    Ok(Some(merged))
}

/// One side moved a top-level function or class, unchanged, into another file while the other side edited it in
/// its old place: carry that edit to the new place (and drop it from the old one, where the definition is gone).
/// Only exact moves are recognised, so nothing is guessed.
fn carry_moved_edits(base: &crate::Snapshot, x: &mut crate::Snapshot, y: &mut crate::Snapshot) {
    let items = |path: &str, src: &[u8]| -> Option<Vec<(Key, usize, usize)>> {
        let lang = lang_of(path)?;
        let t = module(lang, src).ok()?;
        let (v, _) = span_items(t.root_node(), src, lang, 0, src.len(), true).ok()?;
        Some(v.into_iter().map(|i| (i.key, i.start, i.stop)).collect())
    };
    let trim = |b: &[u8]| String::from_utf8_lossy(b).trim().to_string();
    let lead = |b: &[u8]| -> Vec<u8> { b.iter().take_while(|c| c.is_ascii_whitespace()).cloned().collect() };
    for (pa, bv) in base {
        let (Some(bi), Some(ya)) = (items(pa, bv), y.get(pa).cloned()) else { continue };
        if x.get(pa) == Some(bv) || y.get(pa) == Some(bv) { continue; }
        let Some(yi) = items(pa, &ya) else { continue };
        let xi = x.get(pa).and_then(|v| items(pa, v)).unwrap_or_default();
        for (k, bs, be) in &bi {
            if !matches!(k, Key::Def(_) | Key::Class(_)) || xi.iter().any(|(xk, _, _)| xk == k) { continue; }
            let Some((_, ys, ye)) = yi.iter().find(|(yk, _, _)| yk == k).cloned() else { continue };
            let base_text = &bv[*bs..*be];
            if trim(&ya[ys..ye]) == trim(base_text) { continue; }   // the other side did not edit it
            // Where did x put it, unchanged?
            let target = x.iter().filter(|(pb, v)| *pb != pa && lang_of(pb) == lang_of(pa) && base.get(*pb) != Some(*v))
                .find_map(|(pb, v)| items(pb, v).and_then(|its| its.into_iter().find(|(xk, s0, e0)| xk == k && trim(&v[*s0..*e0]) == trim(base_text)).map(|(_, s0, e0)| (pb.clone(), s0, e0))));
            let Some((pb, xs, xe)) = target else { continue };
            let xb = x[&pb].clone();
            let moved = [&xb[..xs], &lead(&xb[xs..xe])[..], trim(&ya[ys..ye]).as_bytes(), b"\n", &xb[xe..]].concat();
            let reverted = [&ya[..ys], &lead(&ya[ys..ye])[..], trim(base_text).as_bytes(), b"\n", &ya[ye..]].concat();
            x.insert(pb, moved);
            y.insert(pa.clone(), reverted);
            return carry_moved_edits(base, x, y);   // positions changed: look again from the start
        }
    }
}

/// Merge whole snapshots (absent path = absent or deleted). Files merge one by one; then the combined work is
/// checked for changed meaning: when one side changed what a symbol does (directly or through the helpers
/// it calls) and the other side's new code relies on it, the merge is refused even if the text combined cleanly.
pub fn merge_snapshots(base: &crate::Snapshot, head: &crate::Snapshot, sub: &crate::Snapshot) -> Result<crate::Snapshot, (String, String)> {
    let mut out = crate::Snapshot::new();
    let (mut h2, mut s2) = (head.clone(), sub.clone());
    carry_moved_edits(base, &mut h2, &mut s2);
    carry_moved_edits(base, &mut s2, &mut h2);
    let paths: BTreeSet<&String> = base.keys().chain(h2.keys()).chain(s2.keys()).collect();
    for p in paths {
        let (b, h, s) = (base.get(p).map(Vec::as_slice), h2.get(p).map(Vec::as_slice), s2.get(p).map(Vec::as_slice));
        match merge_file(p, b, h, s) {
            Ok(Some(v)) => { out.insert(p.clone(), v); }
            Ok(None) => {}
            Err(Conflict(c)) => return Err(("MERGE_CONFLICT".into(), format!("{p}: {c}"))),
            Err(Unsupported(c)) => return Err(("UNSUPPORTED_CHANGE".into(), format!("{p}: {c}"))),
        }
    }
    let hz = crate::merge_hazards(base, head, sub);
    if let Some(w) = hz.first() {
        let what = match w.kind.as_str() {
            "UNPARSED" => format!("{}: does not parse", w.path),
            _ => format!("{} changed {} which new code in {} by {} uses", w.changed_by.clone().unwrap_or_default(), w.symbol.clone().unwrap_or_default(), w.path, w.referenced_by.clone().unwrap_or_default()),
        };
        return Err(("SEMANTIC_HAZARD".into(), what));
    }
    Ok(out)
}

/// CLI helper: merge `git merge-file`-style inputs and write JSON.
pub fn merge_json(path: &str, base: Option<&str>, head: Option<&str>, sub: Option<&str>) -> serde_json::Value {
    match merge_file(path, base.map(str::as_bytes), head.map(str::as_bytes), sub.map(str::as_bytes)) {
        Ok(Some(v)) => serde_json::json!({"status": "MERGED", "source": String::from_utf8_lossy(&v)}),
        Ok(None) => serde_json::json!({"status": "DELETED"}),
        Err(Conflict(c)) => serde_json::json!({"status": "MERGE_CONFLICT", "code": c}),
        Err(Unsupported(c)) => serde_json::json!({"status": "UNSUPPORTED_CHANGE", "code": c}),
    }
}

#[allow(dead_code)]
fn _unused(_w: &mut dyn Write) {}
