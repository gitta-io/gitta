//! C and C++: tree-sitter-c and tree-sitter-cpp, read with the preprocessor in mind.
//!
//! Names. Symbols with external linkage share one module, `c` (C has one global name space, and C++ code calls
//! C functions), under their qualified C++ name: `fmt::format_to`, `leveldb::DBImpl::Write`, `sqlite3_open`.
//! A C struct, union or enum tag is `struct S` / `union U` / `enum E` (C keeps tags apart from other names),
//! its fields `S::field`; C enum constants are plain global names. Inline namespaces are left out of names
//! (`fmt::v11::format` is `fmt::format`, as callers write it). What one translation unit keeps to itself is in
//! the file's own module `c:<path>`: `static` functions and variables, anonymous namespaces, and macros defined
//! in a source file (.c, .cc, .cpp ...). Static inline functions and macros in headers belong to every file that
//! includes them, so they are global. Functions are keyed by name plus parameter types (overloads are separate
//! items); a use is matched by name, so a call reaches every overload of that name.
//!
//! Uses. Calls, types, macros (also inside other macros' bodies) and template arguments; a C++ name is looked up
//! in the enclosing namespaces and class, `using namespace`, `using a::b` and namespace aliases. `p->f` and `s.f`
//! name the field of the receiver's declared type: parameters, locals and globals of the file, `this`, and member
//! chains through structs defined in the same file. A receiver declared elsewhere (a global from a header, or a
//! member of a struct defined in a header: `data->set.timeout`) is resolved when the change is judged, against the
//! globals and struct members of the changing side's files.
//!
//! Headers. `#include "x.h"` is looked up next to the including file, then in the include directories (the
//! `.gitta-include` file or the `include` setting), then `include/` and `src/` of the repository; `<x.h>` only in
//! the include directories and `include/`. A header that is not in the repository is external. Since header
//! symbols are global, a change to a declaration, macro or struct field in a header reaches every file whose new
//! code uses it, wherever the header is included.
//!
//! `.h` files. Parsed as C++ when the `.gitta-include` file says so (`.h c++`, optionally for a directory), when a
//! C++ source file of the repository includes them, or when their text uses C++ (classes, namespaces, templates).
//! Otherwise C. When the chosen grammar reads a header badly, the other one is tried and the better read is kept.
//!
//! Preprocessor. Conditional directives (`#if`, `#ifdef`, `#elif`, `#else`, `#endif`) are blanked before parsing,
//! keeping every byte offset, so all branches are read as code: the first branch always, a later branch when it
//! and the first branch both balance their braces and parentheses (two alternative function headers would not
//! parse as one). `#if 0` blocks are skipped. A region that still fails to parse (a macro used as a statement,
//! say) stays a local error: when a top-level error spans more than a few lines, the region is parsed again in
//! top-level chunks. A file is UNPARSED only when errors cover more than half of it.

use crate::{Item, Key, Lang, Refs, FileInfo, Snapshot};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::RwLock;
use tree_sitter::{Node, Tree};

/// Module of every C and C++ symbol with external linkage.
pub const MODULE: &str = "c";
/// Module prefix of a global variable used as a receiver (`server.port`), resolved by `receivers`.
const RECV: &str = "c?";

/// Module of what one translation unit keeps to itself.
pub fn local_module(path: &str) -> String { format!("c:{path}") }

/// How a C or C++ symbol is shown in a warning: `sqlite3_open`, `leveldb::DB::Put`, `server.maxclients`.
pub fn display(module: &str, name: &str) -> Option<String> {
    if module == MODULE || module.starts_with("c:") { return Some(name.to_string()); }
    module.strip_prefix(RECV).map(|v| format!("{v}.{name}"))
}

/// The part of a symbol name that appears in the text of code that uses it: `resize` for `ns::Widget::resize`.
pub fn search_name(n: &str) -> &str {
    let n = n.rsplit("::").next().unwrap_or(n);
    let n = ["struct ", "union ", "enum ", "#define "].iter().find_map(|p| n.strip_prefix(p)).unwrap_or(n);
    n.split('<').next().unwrap_or(n)
}

// ---- settings: include directories and the `.h` language rule

struct Settings { dirs: Vec<String>, h_rules: Vec<(String, Lang)>, ns_macros: Vec<(String, String)>, cpp_headers: BTreeSet<String>, gen: u64 }
static SETTINGS: RwLock<Settings> = RwLock::new(Settings { dirs: Vec::new(), h_rules: Vec::new(), ns_macros: Vec::new(), cpp_headers: BTreeSet::new(), gen: 0 });

/// Changes whenever the settings change: part of the parse cache key of C and C++ files.
pub fn generation() -> u64 { SETTINGS.read().map(|s| s.gen).unwrap_or(0) }

/// Include directories (like `-I`), and `.h` rules: (directory prefix, "" for all headers; language).
pub fn set_config(dirs: Vec<String>, h_rules: Vec<(String, Lang)>) { set_config_all(Config { dirs, h_rules, ns_macros: vec![] }) }

/// What `.gitta-include` sets: include directories, `.h` rules, and macros that open a namespace (macro, name).
#[derive(Default, Debug, PartialEq)]
pub struct Config { pub dirs: Vec<String>, pub h_rules: Vec<(String, Lang)>, pub ns_macros: Vec<(String, String)> }

pub fn set_config_all(c: Config) {
    if let Ok(mut s) = SETTINGS.write() {
        if s.dirs == c.dirs && s.h_rules == c.h_rules && s.ns_macros == c.ns_macros { return; }
        s.dirs = c.dirs; s.h_rules = c.h_rules; s.ns_macros = c.ns_macros; s.gen += 1;
    }
}

/// Headers that C++ source files of the repository include (found by the caller, which sees the whole tree).
pub fn set_cpp_headers(h: BTreeSet<String>) {
    if let Ok(mut s) = SETTINGS.write() { if s.cpp_headers != h { s.cpp_headers = h; s.gen += 1; } }
}

/// Parse the `.gitta-include` text: one include directory per line; `.h c++` or `.h c`, optionally followed by a
/// directory, sets how `.h` files are read; `namespace MACRO name` names the namespace a macro opens (no name: an
/// inline namespace). `#` starts a comment.
pub fn parse_config(text: &str) -> Config {
    let mut c = Config::default();
    for l in text.lines() {
        let l = l.split('#').next().unwrap_or("").trim();
        if l.is_empty() { continue; }
        let w: Vec<&str> = l.split_whitespace().collect();
        if w[0] == ".h" && w.len() >= 2 {
            let lang = match w[1].to_ascii_lowercase().as_str() { "c++" | "cpp" | "cxx" => Lang::Cpp, "c" => Lang::C, _ => continue };
            c.h_rules.push((w.get(2).map(|d| d.trim_matches('/').to_string()).unwrap_or_default(), lang));
        } else if w[0] == "namespace" && w.len() >= 2 {
            c.ns_macros.push((w[1].to_string(), w.get(2).map(|n| n.to_string()).unwrap_or_default()));
        } else { c.dirs.push(l.trim_matches('/').to_string()); }
    }
    c
}

/// Read `.gitta-include` from a repository root, plus include directories from the settings.
pub fn load_config(root: &Path, extra_dirs: &[String]) {
    let t = std::fs::read_to_string(root.join(".gitta-include")).unwrap_or_default();
    let mut c = parse_config(&t);
    for d in extra_dirs { let d = d.trim_matches('/').to_string(); if !c.dirs.contains(&d) { c.dirs.push(d); } }
    set_config_all(c);
}

const C_EXT: &[&str] = &[".c"];
const CPP_EXT: &[&str] = &[".cc", ".cpp", ".cxx", ".c++", ".hpp", ".hh", ".hxx", ".h++", ".ipp", ".inl", ".tpp", ".C"];
const HEADER_EXT: &[&str] = &[".h", ".hpp", ".hh", ".hxx", ".h++", ".ipp", ".inl", ".tpp"];

/// Language by file name (a `.h` file by the settings; its text is looked at in `header_lang`).
pub fn lang_of(path: &str) -> Option<Lang> {
    if path.ends_with(".h") { return Some(header_lang(path, None)); }
    if C_EXT.iter().any(|e| path.ends_with(e)) { return Some(Lang::C); }
    if CPP_EXT.iter().any(|e| path.ends_with(e)) { return Some(Lang::Cpp); }
    None
}

pub fn is_header(path: &str) -> bool { HEADER_EXT.iter().any(|e| path.ends_with(e)) }

/// How a `.h` file is read: the `.gitta-include` rule for its directory, then whether C++ sources include it,
/// then (with its text) whether it uses C++. Otherwise C.
pub fn header_lang(path: &str, src: Option<&[u8]>) -> Lang {
    if let Ok(s) = SETTINGS.read() {
        // The most specific rule wins.
        let rule = s.h_rules.iter().filter(|(d, _)| d.is_empty() || path.starts_with(&format!("{d}/"))).max_by_key(|(d, _)| d.len());
        if let Some((_, l)) = rule { return *l; }
        if s.cpp_headers.contains(path) { return Lang::Cpp; }
    }
    if src.map(looks_cpp).unwrap_or(false) { Lang::Cpp } else { Lang::C }
}

/// C++ only constructs at the start of a line (comments and strings rarely start that way).
fn looks_cpp(src: &[u8]) -> bool {
    let t = String::from_utf8_lossy(&src[..src.len().min(256 * 1024)]);
    t.lines().map(str::trim_start).any(|l| {
        l.starts_with("namespace ") || l.starts_with("template <") || l.starts_with("template<") || l.starts_with("class ") && !l.ends_with(';')
            || l.starts_with("public:") || l.starts_with("private:") || l.starts_with("protected:") || l.starts_with("using namespace ")
            || l.starts_with("extern \"C++\"") || l.starts_with("constexpr ")
    })
}

// ---- preprocessing and parsing

/// Logical lines (start, end without the newline): a line ending in `\` continues on the next one.
fn logical_lines(src: &[u8]) -> Vec<(usize, usize)> {
    let mut lines = vec![];
    let mut i = 0;
    while i < src.len() {
        let s = i;
        loop {
            let e = src[i..].iter().position(|&b| b == b'\n').map(|p| i + p).unwrap_or(src.len());
            let body = src[s..e].strip_suffix(b"\r").unwrap_or(&src[s..e]);
            i = (e + 1).min(src.len());
            if !body.ends_with(b"\\") || e >= src.len() { lines.push((s, e)); break; }
        }
    }
    lines
}

/// Braces and parentheses opened minus closed in `t`, outside comments, strings and character literals.
fn balance(t: &[u8]) -> (i64, i64) {
    let (mut br, mut pa, mut j) = (0i64, 0i64, 0usize);
    while j < t.len() {
        match t[j] {
            b'/' if t.get(j + 1) == Some(&b'/') => { while j < t.len() && t[j] != b'\n' { j += 1; } }
            b'/' if t.get(j + 1) == Some(&b'*') => { j += 2; while j + 1 < t.len() && !(t[j] == b'*' && t[j + 1] == b'/') { j += 1; } j += 1; }
            q @ (b'"' | b'\'') => { j += 1; while j < t.len() && t[j] != q && t[j] != b'\n' { if t[j] == b'\\' { j += 1; } j += 1; } }
            b'{' => br += 1, b'}' => br -= 1, b'(' => pa += 1, b')' => pa -= 1,
            _ => {}
        }
        j += 1;
    }
    (br, pa)
}

/// The source with conditional directives blanked (same length, same line breaks), so every branch reads as code.
pub fn prepare(src: &[u8]) -> Vec<u8> {
    prepare_c(src)
}

fn prepare_c(src: &[u8]) -> Vec<u8> {
    let mut out = src.to_vec();
    let lines = logical_lines(src);
    let directive = |l: &(usize, usize)| -> Option<String> {
        let t = &src[l.0..l.1];
        let k = t.iter().position(|b| !b.is_ascii_whitespace())?;
        (t[k] == b'#').then(|| String::from_utf8_lossy(&t[k + 1..]).trim_start().to_string())
    };
    // Each conditional: the lines of its branch directives, the #endif line, and whether it is `#if 0`.
    let mut stack: Vec<(bool, Vec<usize>)> = vec![];
    let mut conds: Vec<(Vec<usize>, usize, bool)> = vec![];
    for (n, l) in lines.iter().enumerate() {
        let Some(d) = directive(l) else { continue };
        let w: String = d.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
        match w.as_str() {
            "if" | "ifdef" | "ifndef" => {
                let cond = d[w.len()..].split("/*").next().unwrap_or("").split("//").next().unwrap_or("").trim().to_string();
                stack.push((w == "if" && cond == "0", vec![n]));
            }
            "elif" | "else" | "elifdef" | "elifndef" => if let Some(f) = stack.last_mut() { f.1.push(n); },
            "endif" => if let Some((if0, br)) = stack.pop() { conds.push((br, n, if0)); },
            _ => {}
        }
    }
    // An #if never closed: its directive lines are still blanked; the branches stay.
    for (if0, br) in stack { let end = br[br.len() - 1]; let _ = if0; conds.push((br, end, false)); }
    let blank = |out: &mut Vec<u8>, a: usize, b: usize| { for x in &mut out[a..b.max(a)] { if *x != b'\n' && *x != b'\r' { *x = b' '; } } };
    // `class LEVELDB_EXPORT DB {`: an export or visibility macro between the keyword and the name.
    let mut i = 0;
    while i + 6 < src.len() {
        let kw = ["class ", "struct ", "union "].iter().find(|k| src[i..].starts_with(k.as_bytes()) && (i == 0 || !(src[i - 1].is_ascii_alphanumeric() || src[i - 1] == b'_')));
        let Some(kw) = kw else { i += 1; continue };
        let mut j = i + kw.len();
        while j < src.len() && src[j] == b' ' { j += 1; }
        let m0 = j;
        while j < src.len() && (src[j].is_ascii_uppercase() || src[j].is_ascii_digit() || src[j] == b'_') { j += 1; }
        let is_macro = j > m0 + 1 && src[m0].is_ascii_uppercase() && j < src.len() && src[j] == b' ';
        let mut k = j;
        while k < src.len() && src[k] == b' ' { k += 1; }
        let n0 = k;
        while k < src.len() && (src[k].is_ascii_alphanumeric() || src[k] == b'_') { k += 1; }
        while k < src.len() && src[k].is_ascii_whitespace() { k += 1; }
        if is_macro && k > n0 && k < src.len() && (src[k] == b'{' || src[k] == b':' || src[k..].starts_with(b"final")) {
            for x in &mut out[m0..j] { *x = b' '; }
        }
        i = j.max(i + 1);
    }
    namespace_macros(src, &lines, &mut out);
    for (br, end, if0) in &conds {
        let mut bounds = br.clone();
        if bounds.last() != Some(end) { bounds.push(*end); }
        let bal: Vec<(i64, i64)> = bounds.windows(2).map(|w| balance(&src[lines[w[0]].1.min(lines[w[1]].0)..lines[w[1]].0])).collect();
        for (k, w) in bounds.windows(2).enumerate() {
            let drop = if *if0 { k == 0 } else { k > 0 && (bal[k] != (0, 0) || bal[0] != (0, 0)) };
            if drop { blank(&mut out, lines[w[0]].1, lines[w[1]].0); }
        }
        for &d in &bounds { blank(&mut out, lines[d].0, lines[d].1); }
    }
    out
}

/// `FMT_BEGIN_NAMESPACE` / `NLOHMANN_JSON_NAMESPACE_BEGIN` on a line of its own: a macro that opens a namespace,
/// named after its first word (`fmt`, `nlohmann`), or set in `.gitta-include` (`namespace FMT_BEGIN_NAMESPACE fmt`).
/// It is rewritten in place as `namespace fmt {` (the macro is never shorter), its END macro as `}`. Right after an
/// explicit `namespace absl {` the macro opens an inline namespace, which adds nothing to names: it is blanked.
fn namespace_macros(src: &[u8], lines: &[(usize, usize)], out: &mut [u8]) {
    let configured: Vec<(String, String)> = SETTINGS.read().map(|s| s.ns_macros.clone()).unwrap_or_default();
    let role = |w: &str| -> Option<(bool, String)> {
        if let Some((_, n)) = configured.iter().find(|(m, _)| m == w) { return Some((true, n.clone())); }
        if !w.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') || !w.contains("NAMESPACE") { return None; }
        let open = ["_BEGIN_NAMESPACE", "_NAMESPACE_BEGIN", "_NAMESPACE_OPEN", "_OPEN_NAMESPACE"].iter().any(|x| w.ends_with(x)) || w.starts_with("BEGIN_");
        let close = ["_END_NAMESPACE", "_NAMESPACE_END", "_NAMESPACE_CLOSE", "_CLOSE_NAMESPACE"].iter().any(|x| w.ends_with(x)) || w.starts_with("END_");
        if !open && !close { return None; }
        let first = w.trim_start_matches("BEGIN_").trim_start_matches("END_").split('_').next().unwrap_or("").to_ascii_lowercase();
        (!first.is_empty() && first != "namespace").then_some((open, first))
    };
    let mut opened: Vec<bool> = vec![];   // per open macro: rewritten (true) or blanked (false)
    let mut recent: Vec<String> = vec![];
    for &(a, b) in lines {
        let t = String::from_utf8_lossy(&src[a..b]);
        let w = t.trim().trim_end_matches(';');
        let roles = if w.is_empty() || w.contains(' ') || w.starts_with('#') { None } else { role(w) };
        match roles {
            Some((true, ns)) => {
                let start = a + t.find(w).unwrap_or(0);
                let inline = ns.is_empty() || recent.iter().any(|l| l.split_whitespace().collect::<Vec<_>>() == ["namespace", ns.as_str(), "{"]);
                let text = format!("namespace {ns} {{");
                if !inline && text.len() <= w.len() {
                    out[start..start + w.len()].fill(b' ');
                    out[start..start + text.len()].copy_from_slice(text.as_bytes());
                    opened.push(true);
                } else { out[start..start + w.len()].fill(b' '); opened.push(false); }
            }
            Some((false, _)) => {
                let start = a + t.find(w).unwrap_or(0);
                out[start..start + w.len()].fill(b' ');
                if opened.pop().unwrap_or(false) { out[start] = b'}'; }
            }
            None => {}
        }
        if !t.trim().is_empty() { recent.push(t.trim().to_string()); if recent.len() > 3 { recent.remove(0); } }
    }
}

/// Top-level chunks of [s, e): a chunk ends after a line that starts with `}`, or before a line that starts in
/// column 0 after a blank line.
fn chunks(src: &[u8], s: usize, e: usize) -> Vec<(usize, usize)> {
    let (mut out, mut start, mut i, mut prev_blank) = (vec![], s, s, false);
    while i < e {
        let le = src[i..e].iter().position(|&b| b == b'\n').map(|p| i + p + 1).unwrap_or(e);
        let line = &src[i..le];
        let blank = line.iter().all(|b| b.is_ascii_whitespace());
        let col0 = line.first().map(|b| !b.is_ascii_whitespace() && *b != b'{' && *b != b')').unwrap_or(false);
        if col0 && prev_blank && i > start { out.push((start, i)); start = i; }
        if line.first() == Some(&b'}') { out.push((start, le)); start = le; }
        prev_blank = blank;
        i = le;
    }
    if start < e { out.push((start, e)); }
    out.into_iter().filter(|(a, b)| src[*a..*b].iter().any(|c| !c.is_ascii_whitespace())).collect()
}

fn error_bytes(n: Node) -> usize {
    if n.is_error() || n.is_missing() { return n.end_byte() - n.start_byte(); }
    if !n.has_error() { return 0; }
    let mut c = n.walk();
    n.children(&mut c).map(error_bytes).sum()
}

/// Parsed trees of a prepared source: the whole file, plus top-level chunks of any large error region (whose
/// error node is then skipped). Returns the trees and the total bytes still in errors.
pub(crate) struct Parsed { pub trees: Vec<Tree>, pub skip: BTreeSet<(usize, usize)>, pub errors: usize }

pub(crate) fn parse(lang: Lang, src: &[u8]) -> Option<Parsed> {
    let mut p = crate::parser(lang);
    let tree = p.parse(src, None)?;
    let root = tree.root_node();
    let mut errors = error_bytes(root);
    let mut skip = BTreeSet::new();
    let mut trees = vec![];
    if errors > 0 {
        let big: Vec<(usize, usize)> = crate::children_all(root).into_iter().filter(|x| x.is_error() && x.end_byte() - x.start_byte() > 2000).map(|x| (x.start_byte(), x.end_byte())).collect();
        for (s, e) in big {
            errors -= e - s;
            skip.insert((s, e));
            for (a, b) in chunks(src, s, e) {
                let r = tree_sitter::Range { start_byte: a, end_byte: b, start_point: point(src, a), end_point: point(src, b) };
                if p.set_included_ranges(&[r]).is_err() { continue; }
                if let Some(t) = p.parse(src, None) { errors += error_bytes(t.root_node()); trees.push(t); }
            }
            let _ = p.set_included_ranges(&[]);
        }
    }
    trees.insert(0, tree);
    Some(Parsed { trees, skip, errors })
}

fn point(src: &[u8], at: usize) -> tree_sitter::Point {
    let row = src[..at].iter().filter(|&&b| b == b'\n').count();
    let col = at - src[..at].iter().rposition(|&b| b == b'\n').map(|i| i + 1).unwrap_or(0);
    tree_sitter::Point { row, column: col }
}

// ---- reading

fn text<'a>(n: Node, src: &'a [u8]) -> &'a str { n.utf8_text(src).unwrap_or("") }

/// Normalised text: comments dropped, white space only where it separates two words.
fn norm(t: &str) -> String {
    let mut s = String::with_capacity(t.len());
    let b = t.as_bytes();
    let mut i = 0;
    let mut space = false;
    while i < b.len() {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'/') { while i < b.len() && b[i] != b'\n' { i += 1; } space = true; continue; }
        if b[i] == b'/' && b.get(i + 1) == Some(&b'*') { i += 2; while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') { i += 1; } i += 2; space = true; continue; }
        let c = b[i] as char;
        if c.is_whitespace() || c == '\\' && b.get(i + 1).map(|x| *x == b'\n' || *x == b'\r').unwrap_or(false) { space = true; i += 1; continue; }
        if space && !s.is_empty() {
            let prev = s.chars().next_back().unwrap();
            if (prev.is_alphanumeric() || prev == '_') && (c.is_alphanumeric() || c == '_') { s.push(' '); }
        }
        space = false;
        s.push(c);
        i += 1;
    }
    s
}

fn field<'t>(n: Node<'t>, f: &str) -> Option<Node<'t>> { n.child_by_field_name(f) }

/// The name a declarator declares, through pointers, arrays, references, initialisers and parentheses.
fn declared_name<'t>(d: Node<'t>) -> Option<Node<'t>> {
    let mut x = d;
    loop {
        match x.kind() {
            "identifier" | "field_identifier" | "type_identifier" | "qualified_identifier" | "destructor_name" | "operator_name"
            | "template_function" | "operator_cast" | "primitive_type" => return Some(x),
            "parenthesized_declarator" => x = crate::children(x).into_iter().next()?,
            _ => x = field(x, "declarator").or_else(|| crate::children(x).into_iter().find(|c| c.kind().ends_with("declarator") || c.kind() == "identifier"))?,
        }
    }
}

/// The function declarator a declaration declares (`int f(int)`, `char *f(void)`), not a function pointer.
fn function_declarator<'t>(d: Node<'t>) -> Option<Node<'t>> {
    let mut x = d;
    loop {
        match x.kind() {
            "function_declarator" => {
                // `(*cb)(int)`: a pointer to a function, which is a variable.
                let inner = field(x, "declarator")?;
                return if inner.kind() == "parenthesized_declarator" { None } else { Some(x) };
            }
            "pointer_declarator" | "reference_declarator" | "attributed_declarator" => x = field(x, "declarator").or_else(|| crate::children(x).into_iter().rfind(|c| c.kind().ends_with("declarator")))?,
            _ => return None,
        }
    }
}

/// The type a declaration names, as a plain name for member lookup: `struct conn *` -> conn, `std::vector<int>` -> std::vector.
fn base_type(t: Node, src: &[u8]) -> Option<String> {
    match t.kind() {
        "struct_specifier" | "union_specifier" | "class_specifier" | "enum_specifier" => field(t, "name").map(|n| strip_args(&norm(text(n, src)))),
        "type_identifier" | "qualified_identifier" | "template_type" => Some(strip_args(&norm(text(t, src)))).filter(|n| n != "auto"),
        "type_descriptor" => field(t, "type").and_then(|x| base_type(x, src)),
        _ => None,
    }
}

/// `std::vector<int>::iterator` -> `std::vector::iterator`.
fn strip_args(n: &str) -> String {
    let (mut out, mut depth) = (String::new(), 0);
    for c in n.chars() {
        match c { '<' => depth += 1, '>' if depth > 0 => depth -= 1, _ if depth == 0 => out.push(c), _ => {} }
    }
    out
}

/// What a scope sees: enclosing namespaces (inline ones are part of lookup, not of names), the class, usings.
#[derive(Clone, Default)]
struct Scope { ns: Vec<String>, class: Option<String>, local: bool }

impl Scope {
    fn qual(&self, n: &str) -> String { if self.ns.is_empty() { n.to_string() } else { format!("{}::{n}", self.ns.join("::")) } }
}

struct Cx<'a> {
    path: &'a str, src: &'a [u8], orig: &'a [u8], cpp: bool, header: bool,
    usings: BTreeSet<String>, using_decl: BTreeMap<String, String>, ns_alias: BTreeMap<String, String>,
    /// struct or class -> field -> field type (for member chains), and global variable -> type, of this file.
    fields: BTreeMap<String, BTreeMap<String, String>>, globals: BTreeMap<String, String>,
    local: String,
    /// Namespace blocks found in the text: (open brace, close brace, names; None for an anonymous namespace).
    ns_ranges: Vec<(usize, usize, Option<Vec<String>>)>,
}

impl Cx<'_> {
    /// Every symbol a written name may mean from `scope`: (module, name) pairs for `Refs::qual`.
    fn names(&self, raw: &str, scope: &Scope, out: &mut BTreeSet<(String, String)>) {
        let raw = raw.trim();
        if raw.is_empty() { return; }
        let put = |out: &mut BTreeSet<(String, String)>, n: String| { out.insert((MODULE.to_string(), n)); };
        if let Some(abs) = raw.strip_prefix("::") { put(out, abs.to_string()); return; }
        let (first, rest) = raw.split_once("::").map(|(a, b)| (a, Some(b))).unwrap_or((raw, None));
        // A namespace alias or using-declaration replaces the first part.
        let first = self.ns_alias.get(first).or_else(|| self.using_decl.get(first)).cloned().unwrap_or_else(|| first.to_string());
        let n = match rest { Some(r) => format!("{first}::{r}"), None => first };
        put(out, n.clone());
        if rest.is_none() { out.insert((self.local.clone(), n.clone())); }
        for i in 1..=scope.ns.len() { put(out, format!("{}::{n}", scope.ns[..i].join("::"))); }
        if let Some(c) = &scope.class {
            put(out, format!("{c}::{n}"));
            // The class's enclosing classes (`Outer::Inner` sees `Outer`'s members).
            let mut cc = c.as_str();
            while let Some((o, _)) = cc.rsplit_once("::") { put(out, format!("{o}::{n}")); cc = o; }
        }
        for u in &self.usings { put(out, format!("{u}::{n}")); }
        if scope.local { out.insert((self.local.clone(), n)); }
    }
}

/// Types of the parameters and locals declared in `n` (a function): name -> type.
fn local_types(n: Node, src: &[u8]) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    crate::walk(n, |x| {
        if matches!(x.kind(), "parameter_declaration" | "optional_parameter_declaration" | "declaration" | "field_declaration") {
            if let Some(t) = field(x, "type").and_then(|t| base_type(t, src)) {
                let mut c = x.walk();
                for d in x.children_by_field_name("declarator", &mut c) {
                    if let Some(nm) = declared_name(d) { env.insert(norm(text(nm, src)), t.clone()); }
                }
            }
        }
        !matches!(x.kind(), "lambda_expression")
    });
    env
}

/// The references of `n`, with the receivers of `->` and `.` resolved where their type is known.
fn refs_of(n: Node, cx: &Cx, scope: &Scope, env: &BTreeMap<String, String>) -> Refs {
    let src = cx.src;
    let mut r = Refs::default();
    let mut q: BTreeSet<(String, String)> = BTreeSet::new();
    // The declared type of an expression used as a receiver.
    fn recv_type(e: Node, cx: &Cx, scope: &Scope, env: &BTreeMap<String, String>) -> Option<String> {
        let src = cx.src;
        match e.kind() {
            "this" => scope.class.clone(),
            "identifier" => {
                // A local or parameter, a data member of the enclosing class (`db_`), or a global of this file.
                let v = text(e, src);
                let member = || scope.class.as_ref().and_then(|c| cx.fields.get(c).or_else(|| cx.fields.get(search_name(c)))).and_then(|m| m.get(v));
                env.get(v).or_else(member).or_else(|| cx.globals.get(v)).cloned()
            }
            "parenthesized_expression" | "pointer_expression" => crate::children(e).into_iter().last().and_then(|x| recv_type(x, cx, scope, env)),
            "subscript_expression" => field(e, "argument").and_then(|x| recv_type(x, cx, scope, env)),
            "field_expression" => {
                let t = recv_type(field(e, "argument")?, cx, scope, env)?;
                let f = text(field(e, "field")?, src);
                cx.fields.get(&t).or_else(|| cx.fields.get(search_name(&t))).and_then(|m| m.get(f)).cloned()
            }
            _ => None,
        }
    }
    // C names are plain: kept as bare names (`Refs::idents`), which `references` reads in module `c` and, through
    // `FileInfo::opens`, in the file's own module. C++ names are looked up in their scope right away.
    let mut bare: BTreeSet<String> = BTreeSet::new();
    let mut name = |raw: &str, q: &mut BTreeSet<(String, String)>| {
        if cx.cpp { cx.names(raw, scope, q) } else if !bare.contains(raw) { bare.insert(raw.trim().to_string()); }
    };
    // One cursor walk per subtree; a construct read specially queues only the parts still to read.
    let mut stack = vec![n];
    while let Some(top) = stack.pop() {
      crate::walk(top, |x| {
        let k = x.kind();
        match k {
            "comment" | "string_literal" | "char_literal" | "raw_string_literal" | "number_literal" | "system_lib_string" | "concatenated_string" => return false,
            "identifier" | "type_identifier" => { name(text(x, src), &mut q); return false; }
            "qualified_identifier" | "nested_namespace_specifier" => {
                name(&strip_args(&norm(text(x, src))), &mut q);
                // Template arguments inside the name are uses too.
                let mut args = vec![];
                crate::walk(x, |y| { if y.kind() == "template_argument_list" { args.push(y); return false; } true });
                stack.extend(args);
                return false;
            }
            "template_type" | "template_function" | "template_method" => {
                if let Some(nm) = field(x, "name") {
                    let base = strip_args(&norm(text(nm, src)));
                    name(&base, &mut q);
                    name(&norm(text(x, src)), &mut q);
                }
                if let Some(a) = field(x, "arguments") { stack.push(a); }
                return false;
            }
            "struct_specifier" | "union_specifier" | "enum_specifier" | "class_specifier" => {
                if let Some(nm) = field(x, "name") {
                    let tname = strip_args(&norm(text(nm, src)));
                    let tag = k.trim_end_matches("_specifier");
                    if tag != "class" { name(&format!("{tag} {tname}"), &mut q); }
                    name(&tname, &mut q);
                    if nm.kind() == "template_type" { name(&norm(text(nm, src)), &mut q); }
                }
                if let Some(b) = field(x, "body") { stack.push(b); }
                if let Some(b) = crate::children(x).into_iter().find(|c| c.kind() == "base_class_clause") { stack.push(b); }
                return false;
            }
            "field_expression" => {
                if let (Some(a), Some(f)) = (field(x, "argument"), field(x, "field")) {
                    let fname = strip_args(&norm(text(f, src)));
                    match recv_type(a, cx, scope, env) {
                        Some(t) => name(&format!("{t}::{fname}"), &mut q),
                        None if a.kind() == "identifier" && !env.contains_key(text(a, src)) => { q.insert((format!("{RECV}{}", text(a, src)), fname)); }
                        // `data->set.timeout` where `data`'s struct is defined in another file: the receiver is
                        // "field `set` of Curl_easy", resolved like a global when the change is judged.
                        None if a.kind() == "field_expression" => {
                            if let (Some(t), Some(g)) = (field(a, "argument").and_then(|x| recv_type(x, cx, scope, env)), field(a, "field")) {
                                q.insert((format!("{RECV}{t}::{}", text(g, src)), fname));
                            }
                        }
                        None => {}
                    }
                    stack.push(a);
                    if f.kind() != "field_identifier" { stack.push(f); }
                }
                return false;
            }
            "preproc_arg" => {
                // Macro bodies are not parsed: their identifiers are uses (other macros, functions, types).
                for w in idents(text(x, src)) { name(w, &mut q); }
                return false;
            }
            "field_identifier" | "namespace_identifier" | "primitive_type" | "statement_identifier" => return false,
            _ => {}
        }
        true
      });
    }
    r.qual = q;
    r.idents = bare;
    r
}

/// Identifiers in unparsed text (a macro body), outside strings and character literals.
fn idents(t: &str) -> Vec<&str> {
    let b = t.as_bytes();
    let (mut out, mut i) = (vec![], 0usize);
    while i < b.len() {
        let c = b[i];
        if c == b'"' || c == b'\'' { i += 1; while i < b.len() && b[i] != c { if b[i] == b'\\' { i += 1; } i += 1; } i += 1; continue; }
        if c == b'/' && b.get(i + 1) == Some(&b'*') { i += 2; while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') { i += 1; } i += 2; continue; }
        if c == b'/' && b.get(i + 1) == Some(&b'/') { break; }
        if c.is_ascii_alphabetic() || c == b'_' {
            let s = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') { i += 1; }
            out.push(&t[s..i]);
            continue;
        }
        if c.is_ascii_digit() { while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'.') { i += 1; } continue; }
        i += 1;
    }
    out
}

/// Parameters as written for the key (types only) and the signature (types and default values).
fn params(fd: Node, src: &[u8]) -> (String, String) {
    let Some(pl) = field(fd, "parameters") else { return (String::new(), String::new()) };
    let (mut key, mut sig) = (vec![], vec![]);
    for p in crate::children(pl) {
        if p.kind() == "comment" { continue; }
        if p.kind() == "variadic_parameter" || text(p, src).trim() == "..." { key.push("...".to_string()); sig.push("...".to_string()); continue; }
        // The type without the parameter's name (a name is not part of what callers rely on).
        let (s, e) = (p.start_byte(), field(p, "default_value").map(|d| d.start_byte()).unwrap_or(p.end_byte()));
        let raw = &src[s..e];
        // Drop the parameter name in byte space (tree-sitter offsets are byte offsets into src), then convert once.
        // Slicing the string by those offsets would panic once from_utf8_lossy widens an invalid byte to U+FFFD.
        let t = match field(p, "declarator").and_then(declared_name).filter(|n| matches!(n.kind(), "identifier")) {
            Some(nm) if nm.start_byte() >= s && nm.end_byte() <= e => String::from_utf8_lossy(&[&raw[..nm.start_byte() - s], &raw[nm.end_byte() - s..]].concat()).into_owned(),
            _ => String::from_utf8_lossy(raw).into_owned(),
        };
        let t = norm(t.trim().trim_end_matches('=').trim());
        let t = if t == "void" && crate::children(pl).len() == 1 { String::new() } else { t };
        if t.is_empty() { continue; }
        key.push(t.clone());
        sig.push(match field(p, "default_value") { Some(d) => format!("{t} = {}", norm(text(d, src))), None => t });
    }
    (key.join(", "), sig.join(", "))
}

/// What follows the parameters and changes how a function may be called or overridden: const, volatile, &, &&,
/// noexcept, throw(), override, final, = 0, = delete.
fn post_qualifiers(decl: Node, fd: Node, src: &[u8]) -> (String, String) {
    let (mut cvref, mut rest) = (vec![], vec![]);
    for c in crate::children(fd) {
        match c.kind() {
            "type_qualifier" | "ref_qualifier" => cvref.push(norm(text(c, src))),
            "noexcept" | "throw_specifier" | "virtual_specifier" | "trailing_return_type" | "requires_clause" => rest.push(norm(text(c, src))),
            _ => {}
        }
    }
    // `const &` with no named child (older grammars): read the raw tail after the parameters.
    if cvref.is_empty() {
        if let Some(pl) = field(fd, "parameters") {
            let tail = norm(&String::from_utf8_lossy(&src[pl.end_byte()..fd.end_byte()]));
            for w in ["const", "volatile", "&&", "&"] { if tail.split_whitespace().any(|x| x == w) { cvref.push(w.to_string()); } }
        }
    }
    for c in crate::children(decl) {
        if matches!(c.kind(), "pure_virtual_clause" | "delete_method_clause" | "default_method_clause") { rest.push(norm(text(c, src)).trim_end_matches(';').to_string()); }
    }
    // `virtual f() = 0;` in a class body reads as a default value.
    if let Some(v) = field(decl, "default_value") { rest.push(format!("={}", norm(text(v, src)))); }
    (cvref.join(" "), rest.join(" "))
}

/// Specifiers in front of a declaration that change its meaning for callers (`inline` and `extern` do not).
fn pre_specifiers(decl: Node, src: &[u8]) -> Vec<String> {
    crate::children_all(decl).into_iter().filter(|c| matches!(c.kind(), "storage_class_specifier" | "virtual" | "explicit_function_specifier" | "type_qualifier"))
        .map(|c| norm(text(c, src))).filter(|s| !matches!(s.as_str(), "inline" | "extern" | "register" | "auto")).collect()
}

/// The return type: the declaration's type plus the pointers and references of the declarator around the name.
fn return_type(decl: Node, fd: Node, src: &[u8]) -> String {
    let ty = field(decl, "type").map(|t| norm(text(t, src))).unwrap_or_default();
    let mut stars = String::new();
    let mut x = field(decl, "declarator");
    while let Some(d) = x {
        if d.id() == fd.id() { break; }
        match d.kind() { "pointer_declarator" => stars.push('*'), "reference_declarator" => stars.push_str(if text(d, src).trim_start().starts_with("&&") { "&&" } else { "&" }), _ => {} }
        x = field(d, "declarator").or_else(|| crate::children(d).into_iter().rfind(|c| c.kind().ends_with("declarator")));
    }
    format!("{ty}{stars}")
}

/// One keyed item before it is filed: key, symbol (module, name), the item, and the local types for refs.
struct Out { items: BTreeMap<Key, Item>, symbols: BTreeMap<Key, (String, String)>, aliases: BTreeMap<String, (String, Option<String>)> }

impl Out {
    fn put(&mut self, key: Key, sym: (String, String), item: Item) {
        match self.items.get_mut(&key) {
            // The same name again (a prototype and its definition, two #ifdef branches): one item with both texts.
            Some(old) => {
                old.text.push('\n'); old.text.push_str(&item.text);
                old.refs.qual.extend(item.refs.qual); old.refs.props.extend(item.refs.props);
                if old.sig.is_none() { old.sig = item.sig; }
                if item.returns.as_deref().map(|r| !r.is_empty()).unwrap_or(false) { old.returns = item.returns; }
                if let (Some(a), Some(b)) = (&mut old.parts, item.parts) { a.extend(b); }
            }
            None => { self.items.insert(key.clone(), item); }
        }
        self.symbols.insert(key, sym);
    }
}

fn item(key: &Key, text: String, refs: Refs, sig: Option<String>, parts: Option<BTreeMap<String, String>>, returns: Option<String>) -> Item {
    Item { key: key.clone(), text, refs, sig, parts, returns, ext: false }
}

/// The type of a global variable, kept with its item so receivers can be resolved across files.
const TYPE_PART: &str = "\0type";

/// Bytes of a C or C++ file that the reader could not parse (after preprocessing and chunked re-reads).
pub fn error_bytes_of(path: &str, src: &[u8]) -> usize {
    let prep = prepare(src);
    let Some(lang) = (if path.ends_with(".h") { Some(header_lang(path, Some(src))) } else { lang_of(path) }) else { return 0 };
    let e = parse(lang, &prep).map(|p| p.errors).unwrap_or(src.len());
    if path.ends_with(".h") && e * 100 > src.len() {
        let other = if lang == Lang::C { Lang::Cpp } else { Lang::C };
        return e.min(parse(other, &prep).map(|p| p.errors).unwrap_or(e));
    }
    e
}

/// Read a C or C++ file into keyed items. None when errors cover more than half of it.
pub fn analyze(path: &str, src: &[u8], lang: Lang) -> Option<FileInfo> {
    if src.contains(&0) { return None; }
    let prep = prepare_c(src);
    let mut lang = if path.ends_with(".h") { header_lang(path, Some(src)) } else { lang };
    let mut parsed = parse(lang, &prep)?;
    if path.ends_with(".h") && parsed.errors * 100 > src.len() {
        // The other grammar may read this header better (C code using `new` as a name, C++ in a `.h`). A few
        // stray error bytes (a macro the grammar does not know) are not worth a second parse.
        let other = if lang == Lang::C { Lang::Cpp } else { Lang::C };
        if let Some(p2) = parse(other, &prep) { if p2.errors < parsed.errors { parsed = p2; lang = other; } }
    }
    if parsed.errors * 2 > src.len() { return None; }
    let header = is_header(path);
    let mut cx = Cx { path, src: &prep, orig: src, cpp: lang == Lang::Cpp, header, usings: BTreeSet::new(), using_decl: BTreeMap::new(),
                      ns_alias: BTreeMap::new(), fields: BTreeMap::new(), globals: BTreeMap::new(), local: local_module(path),
                      ns_ranges: if lang == Lang::Cpp { namespace_ranges(&prep) } else { vec![] },
};
    let skip = &parsed.skip;
    let tops: Vec<Node> = parsed.trees.iter().enumerate().flat_map(|(i, t)| {
        crate::children_all(t.root_node()).into_iter().filter(move |c| i > 0 || !skip.contains(&(c.start_byte(), c.end_byte())))
    }).filter(|c| c.is_named()).collect();
    // First pass: what the whole file sees (usings, aliases), and the types of its globals and struct fields.
    for &n in &tops { let sc = scope_at(&cx, n.start_byte()); survey(n, &mut cx, &sc); }
    let mut out = Out { items: BTreeMap::new(), symbols: BTreeMap::new(), aliases: BTreeMap::new() };
    for &n in &tops { top(n, &cx, &scope_at(&cx, n.start_byte()), &mut out); }
    let _ = cx.path;
    if cx.cpp { unique_overloads(&mut out); }
    let opens = BTreeSet::from([cx.local.clone()]);
    Some(FileInfo { items: out.items, aliases: out.aliases, lang: Some(lang), module: MODULE.to_string(), symbols: out.symbols, opens, ..FileInfo::default() })
}

/// A function with no other overload in the file is keyed by its name alone, so changing its parameters is a
/// change to that function (not a removal and an addition). Overloads keep their parameter types in the key.
fn unique_overloads(out: &mut Out) {
    let strip = |k: &Key| -> Option<Key> {
        let base = |d: &str| d.find('(').map(|i| d[..i].to_string());
        match k {
            Key::Def(d) if !d.starts_with('#') => base(d).map(Key::Def),
            Key::Member(c, inner) => match &**inner { Key::Def(d) => base(d).map(|b| Key::Member(c.clone(), Box::new(Key::Def(b)))), _ => None },
            _ => None,
        }
    };
    let mut count: BTreeMap<Key, usize> = BTreeMap::new();
    for k in out.items.keys() { if let Some(s) = strip(k) { *count.entry(s).or_default() += 1; } }
    let keys: Vec<Key> = out.items.keys().cloned().collect();
    for k in keys {
        let Some(s) = strip(&k).filter(|s| count.get(s) == Some(&1) && !out.items.contains_key(s)) else { continue };
        let mut it = out.items.remove(&k).unwrap();
        it.key = s.clone();
        out.items.insert(s.clone(), it);
        if let Some(sym) = out.symbols.remove(&k) { out.symbols.insert(s, sym); }
    }
}

/// Names a namespace definition adds: `a::b` -> [a, b]; anonymous -> None; inline namespaces add nothing.
fn ns_names(n: Node, src: &[u8]) -> Option<Vec<String>> {
    let inline = crate::children_all(n).iter().any(|c| c.kind() == "inline");
    let nm = field(n, "name")?;
    if inline { return Some(vec![]); }
    Some(norm(text(nm, src)).split("::").filter(|s| !s.is_empty()).map(String::from).collect())
}

fn inner_scope(n: Node, cx: &Cx, scope: &Scope) -> Scope {
    let mut s = scope.clone();
    match ns_names(n, cx.src) { Some(v) => s.ns.extend(v), None => s.local = true }
    s
}

/// The namespaces around byte `pos`, from the text. A parse error can make tree-sitter close a namespace early
/// (fmt's headers, with macros it does not know); matching braces in the text does not depend on the parse.
fn scope_at(cx: &Cx, pos: usize) -> Scope {
    let mut s = Scope::default();
    for (a, b, names) in &cx.ns_ranges {
        if *a > pos { break; }
        if pos < *b { match names { Some(v) => s.ns.extend(v.iter().cloned()), None => s.local = true } }
    }
    s
}

/// `namespace a::b {`, `inline namespace v1 {`, `namespace {` with their closing braces, outside comments and strings.
fn namespace_ranges(src: &[u8]) -> Vec<(usize, usize, Option<Vec<String>>)> {
    let mut out = vec![];
    let mut open: Vec<(usize, Option<Option<Vec<String>>>)> = vec![];   // per open brace: Some(namespace) or None
    let mut pending: Option<Option<Vec<String>>> = None;
    let (mut i, n) = (0usize, src.len());
    let word = |i: usize| -> &[u8] { let mut j = i; while j < n && (src[j].is_ascii_alphanumeric() || src[j] == b'_') { j += 1; } &src[i..j] };
    while i < n {
        let c = src[i];
        match c {
            b'/' if src.get(i + 1) == Some(&b'/') => { while i < n && src[i] != b'\n' { i += 1; } continue; }
            b'/' if src.get(i + 1) == Some(&b'*') => { i += 2; while i + 1 < n && !(src[i] == b'*' && src[i + 1] == b'/') { i += 1; } i += 2; continue; }
            b'"' | b'\'' => {
                // A raw string R"(...)" may hold anything.
                if c == b'"' && i > 0 && src[i - 1] == b'R' {
                    let d0 = i + 1; let mut d = d0; while d < n && src[d] != b'(' { d += 1; }
                    let close = [&b")"[..], &src[d0..d], &b"\""[..]].concat();
                    i = src[d..].windows(close.len()).position(|w| w == close.as_slice()).map(|p| d + p + close.len()).unwrap_or(n);
                    continue;
                }
                i += 1; while i < n && src[i] != c && src[i] != b'\n' { if src[i] == b'\\' { i += 1; } i += 1; } i += 1; continue;
            }
            b'{' => { open.push((i, pending.take())); i += 1; continue; }
            b'}' => { if let Some((a, Some(names))) = open.pop() { out.push((a, i, names)); } i += 1; continue; }
            b';' => { pending = None; i += 1; continue; }
            _ => {}
        }
        if (c.is_ascii_alphabetic() || c == b'_') && (i == 0 || !(src[i - 1].is_ascii_alphanumeric() || src[i - 1] == b'_')) {
            let w = word(i);
            if w == b"namespace" {
                // The name(s) up to `{`; `namespace x = y;` and `using namespace x;` never reach a brace.
                let mut j = i + w.len();
                let mut names = String::new();
                while j < n && (src[j].is_ascii_whitespace() || src[j].is_ascii_alphanumeric() || src[j] == b'_' || src[j] == b':') { names.push(src[j] as char); j += 1; }
                if j < n && src[j] == b'{' {
                    let before = src[..i].iter().rposition(|b| !b.is_ascii_whitespace()).map(|p| p + 1).unwrap_or(0);
                    let inline = src[..before].ends_with(b"inline");
                    let v: Vec<String> = names.split("::").map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
                    pending = Some(if v.is_empty() { None } else if inline { Some(vec![]) } else { Some(v) });
                }
            }
            i += w.len();
            continue;
        }
        i += 1;
    }
    out.sort();
    out
}

fn survey(n: Node, cx: &mut Cx, scope: &Scope) {
    let src = cx.src;
    match n.kind() {
        "namespace_definition" => {
            if let Some(b) = field(n, "body") { for c in crate::children(b) { let s = if cx.ns_ranges.is_empty() { inner_scope(n, cx, scope) } else { scope_at(cx, c.start_byte()) }; survey(c, cx, &s); } }
        }
        "linkage_specification" | "ERROR" | "declaration_list" => {
            let body = if n.kind() == "linkage_specification" { field(n, "body") } else { Some(n) };
            if let Some(b) = body { if b.kind() == "declaration_list" || b.kind() == "ERROR" { for c in crate::children(b) { survey(c, cx, scope); } } else { survey(b, cx, scope); } }
        }
        "template_declaration" => for c in crate::children(n) { survey(c, cx, scope); },
        "using_declaration" => {
            let t = norm(text(n, src));
            let t = t.trim_end_matches(';').trim();
            if let Some(ns) = t.strip_prefix("using namespace ") { cx.usings.insert(ns.trim_start_matches("::").to_string()); }
            else if let Some(full) = t.strip_prefix("using ") {
                let full = full.trim_start_matches("::").to_string();
                if let Some((_, last)) = full.rsplit_once("::") { cx.using_decl.insert(last.to_string(), full.clone()); }
            }
        }
        "namespace_alias_definition" => {
            if let (Some(nm), Some(t)) = (field(n, "name"), crate::children(n).into_iter().rfind(|c| c.kind() != "namespace_identifier" || c.start_byte() > field(n, "name").map(|x| x.end_byte()).unwrap_or(0))) {
                cx.ns_alias.insert(text(nm, src).to_string(), norm(text(t, src)).trim_start_matches("::").to_string());
            }
        }
        "declaration" => {
            if let Some(t) = field(n, "type") {
                record_struct(t, cx, scope, None);
                if let Some(ty) = base_type(t, src) {
                    let mut c = n.walk();
                    for d in n.children_by_field_name("declarator", &mut c) {
                        if function_declarator(d).is_some() { continue; }
                        if let Some(nm) = declared_name(d) { cx.globals.insert(norm(text(nm, src)), ty.clone()); }
                    }
                }
            }
        }
        "type_definition" => {
            if let Some(t) = field(n, "type") {
                let mut c = n.walk();
                let alias = n.children_by_field_name("declarator", &mut c).find_map(declared_name).map(|x| norm(text(x, src)));
                record_struct(t, cx, scope, alias);
            }
        }
        "struct_specifier" | "union_specifier" | "class_specifier" => record_struct(n, cx, scope, None),
        _ => {}
    }
}

/// Field types of a struct or class defined here, under its name (and a typedef name for it).
fn record_struct(t: Node, cx: &mut Cx, scope: &Scope, alias: Option<String>) {
    if !matches!(t.kind(), "struct_specifier" | "union_specifier" | "class_specifier") { return; }
    let Some(body) = field(t, "body") else { return };
    let src = cx.src;
    let name = field(t, "name").map(|n| strip_args(&norm(text(n, src)))).or(alias.clone());
    let Some(name) = name else { return };
    let mut m = BTreeMap::new();
    for f in crate::children(body).into_iter().filter(|c| c.kind() == "field_declaration") {
        let Some(ty) = field(f, "type").and_then(|t| base_type(t, src)) else { continue };
        let mut c = f.walk();
        for d in f.children_by_field_name("declarator", &mut c) {
            if function_declarator(d).is_some() { continue; }
            if let Some(nm) = declared_name(d) { m.insert(norm(text(nm, src)), ty.clone()); }
        }
    }
    let full = if cx.cpp { scope.qual(&name) } else { name.clone() };
    cx.fields.insert(full.clone(), m.clone());
    if full != name { cx.fields.insert(name, m.clone()); }
    if let Some(a) = alias { cx.fields.insert(a, m); }
}

/// The original text of a node, from the start of its first line (the item owns its indentation).
fn orig_text(n: Node, cx: &Cx) -> String {
    let s = cx.orig[..n.start_byte()].iter().rposition(|&b| b == b'\n').map(|i| i + 1).unwrap_or(0);
    // Blank in the parsed text: indentation, and dialect words the parser does not see.
    let s = if cx.src[s..n.start_byte()].iter().all(|b| b.is_ascii_whitespace()) { s } else { n.start_byte() };
    // Trailing blanks (records padded to column 72 or 80) are not part of the text.
    let t = String::from_utf8_lossy(&cx.orig[s..n.end_byte()]);
    let t = t.trim_end();
    if !t.contains([' ', '\t', '\r']) || !t.split('\n').any(|l| l.ends_with([' ', '\t', '\r'])) { return t.to_string(); }
    t.split('\n').map(str::trim_end).collect::<Vec<_>>().join("\n")
}

/// What the compiler dialect adds to the interface of `name` (nothing in standard C and C++).
fn dialect_sig(name: &str, cx: &Cx) -> String {
    { let _ = (name, cx); String::new() }
}

/// A layout qualifier written before the struct or union keyword at `at` (none in standard C and C++).
fn layout_qualifier(at: usize, cx: &Cx) -> Option<&'static str> {
    { let _ = (at, cx); None }
}

/// The module of a symbol declared at `scope` with `decl`'s storage: a translation unit's own, or global.
fn module_for(decl: Node, cx: &Cx, scope: &Scope) -> String {
    let is_static = crate::children(decl).iter().any(|c| c.kind() == "storage_class_specifier" && text(*c, cx.src).trim() == "static");
    if scope.local || (is_static && !cx.header && scope.class.is_none()) { cx.local.clone() } else { MODULE.to_string() }
}

fn top(n: Node, cx: &Cx, scope: &Scope, out: &mut Out) {
    let src = cx.src;
    match n.kind() {
        "comment" | ";" => {}
        "namespace_definition" => {
            if let Some(b) = field(n, "body") { for c in crate::children(b) { let s = if cx.ns_ranges.is_empty() { inner_scope(n, cx, scope) } else { scope_at(cx, c.start_byte()) }; top(c, cx, &s, out); } }
        }
        "linkage_specification" => match field(n, "body") {
            Some(b) if b.kind() == "declaration_list" => for c in crate::children(b) { top(c, cx, scope, out); },
            Some(b) => top(b, cx, scope, out),
            None => {}
        },
        "ERROR" => {
            // What still parses inside an error region is read as usual; the rest is one opaque item, keyed by
            // its first line so an edit inside it compares against the same region in the other version.
            let kids = crate::children(n);
            let known = |k: &str| matches!(k, "function_definition" | "declaration" | "type_definition" | "preproc_def" | "preproc_function_def"
                | "preproc_include" | "struct_specifier" | "union_specifier" | "enum_specifier" | "class_specifier" | "namespace_definition" | "template_declaration");
            for c in kids.iter().filter(|c| known(c.kind())) { top(*c, cx, scope, out); }
            let first = norm(String::from_utf8_lossy(&cx.orig[n.start_byte()..n.end_byte()]).lines().find(|l| !l.trim().is_empty()).unwrap_or(""));
            let key = Key::Opaque(format!("error region {first}"));
            let refs = refs_of(n, cx, scope, &local_types(n, src));
            out.put(key.clone(), (cx.local.clone(), format!("error region {first}")), item(&key, orig_text(n, cx), refs, None, None, None));
        }
        "template_declaration" => {
            let tp = field(n, "parameters").map(|p| norm(text(p, src))).unwrap_or_default();
            let inner = crate::children(n).into_iter().find(|c| !matches!(c.kind(), "template_parameter_list" | "comment" | "requires_clause"));
            if let Some(i) = inner { decl(i, Some(n), &format!("template{tp} "), cx, scope, out); }
        }
        "preproc_include" => {
            let Some(p) = field(n, "path") else { return };
            let raw = text(p, src).trim();
            let quoted = raw.starts_with('"');
            let spec = raw.trim_matches(|c| c == '"' || c == '<' || c == '>');
            let key = Key::Import(norm(text(n, src)));
            out.put(key.clone(), (cx.local.clone(), format!("#include {spec}")), item(&key, orig_text(n, cx), Refs::default(), None, None, None));
            out.aliases.insert(format!("#include {spec}"), (include_module(cx.path, spec, quoted), None));
        }
        "using_declaration" | "namespace_alias_definition" => {
            let key = Key::Import(norm(text(n, src)));
            out.put(key.clone(), (cx.local.clone(), norm(text(n, src))), item(&key, orig_text(n, cx), Refs::default(), None, None, None));
        }
        "preproc_def" | "preproc_function_def" => {
            let Some(nm) = field(n, "name") else { return };
            let name = text(nm, src).to_string();
            let value = field(n, "value").map(|v| norm(text(v, src))).unwrap_or_default();
            let key = Key::Def(format!("#define {name}"));
            let (sig, ret) = match field(n, "parameters") {
                Some(p) => (format!("#define{} -> macro", norm(text(p, src))), Some(value)),
                None => (format!("#define = {value}"), None),
            };
            // A source file's macros are its own; a header's reach every file that includes it.
            let module = if cx.header { MODULE.to_string() } else { cx.local.clone() };
            let refs = refs_of(n, cx, scope, &BTreeMap::new());
            out.put(key.clone(), (module, name), item(&key, orig_text(n, cx), refs, Some(sig), None, ret));
        }
        "preproc_call" | "preproc_if" | "preproc_ifdef" | "preproc_else" | "preproc_elif" => {
            // `#undef`, `#pragma`, `#error`; conditionals are blanked before parsing, so these are rare leftovers.
            for c in crate::children(n).into_iter().filter(|c| !matches!(c.kind(), "preproc_directive" | "preproc_arg" | "identifier")) { top(c, cx, scope, out); }
        }
        _ => decl(n, None, "", cx, scope, out),
    }
}

/// A declaration, definition or type at file or namespace scope (`tmpl` is its template_declaration).
fn decl(n: Node, tmpl: Option<Node>, tp: &str, cx: &Cx, scope: &Scope, out: &mut Out) {
    let src = cx.src;
    let whole = tmpl.unwrap_or(n);
    match n.kind() {
        "function_definition" | "declaration" | "field_declaration" => {
            let mut c = n.walk();
            let decls: Vec<Node> = n.children_by_field_name("declarator", &mut c).collect();
            if let Some(t) = field(n, "type") { if field(t, "body").is_some() { type_item(t, tmpl, tp, None, cx, scope, out); } }
            if decls.is_empty() { return; }
            for d in decls {
                match function_declarator(d) {
                    Some(fd) => function(n, whole, fd, tp, cx, scope, None, out),
                    None => variable(n, whole, d, cx, scope, None, out),
                }
            }
        }
        "type_definition" => {
            let mut c = n.walk();
            let names: Vec<String> = n.children_by_field_name("declarator", &mut c).filter_map(declared_name).map(|x| norm(text(x, src))).collect();
            let t = field(n, "type");
            if let Some(t) = t.filter(|t| field(*t, "body").is_some()) { type_item(t, tmpl, tp, names.first().cloned(), cx, scope, out); }
            let target = t.map(|t| norm(text(t, src))).unwrap_or_default();
            let refs = refs_of(whole, cx, scope, &BTreeMap::new());
            for name in names {
                let q = if cx.cpp { scope.qual(&name) } else { name.clone() };
                let key = Key::Def(format!("typedef {q}"));
                let refs = refs.clone();
                let module = if scope.local { cx.local.clone() } else { MODULE.to_string() };
                // A typedef of a struct with a body: users of the typedef name depend on the layout, and fields
                // reached through it (`S_t *p; p->x`) are the struct's fields under this name too.
                let layout = t.and_then(|t| field(t, "body").map(|_| layout_of(t, cx)));
                let sig = format!("typedef {} {}", layout.clone().unwrap_or(target.clone()), norm(&declarator_shape(n, src)));
                out.put(key.clone(), (module.clone(), q.clone()), item(&key, orig_text(whole, cx), refs, Some(sig), None, None));
                if let Some(t) = t.filter(|t| field(*t, "body").is_some() && field(*t, "name").map(|nm| norm(text(nm, src)) != name).unwrap_or(false)) {
                    members(t, &q, cx, scope, out);
                }
            }
        }
        "alias_declaration" => {
            let Some(nm) = field(n, "name") else { return };
            let q = class_or_ns(scope, &norm(text(nm, src)));
            let key = Key::Def(format!("using {q}"));
            let sig = format!("{tp}using = {}", field(n, "type").map(|t| norm(text(t, src))).unwrap_or_default());
            let refs = refs_of(whole, cx, scope, &BTreeMap::new());
            out.put(key.clone(), (if scope.local { cx.local.clone() } else { MODULE.to_string() }, q), item(&key, orig_text(whole, cx), refs, Some(sig), None, None));
        }
        "struct_specifier" | "union_specifier" | "class_specifier" | "enum_specifier" => type_item(n, tmpl, tp, None, cx, scope, out),
        "concept_definition" => {
            let Some(nm) = field(n, "name") else { return };
            let q = scope.qual(&norm(text(nm, src)));
            let key = Key::Def(format!("concept {q}"));
            let refs = refs_of(whole, cx, scope, &BTreeMap::new());
            out.put(key.clone(), (MODULE.to_string(), q), item(&key, orig_text(whole, cx), refs, Some(norm(text(whole, src))), None, None));
        }
        "template_declaration" => top(n, cx, scope, out),
        "comment" => {}
        _ => {
            // Anything else at top level (a macro call such as `EXPORT_SYMBOL(x);`, `static_assert`): its uses count.
            let t = norm(text(n, src));
            if t.is_empty() { return; }
            let key = Key::Opaque(t.clone());
            let refs = refs_of(whole, cx, scope, &BTreeMap::new());
            out.put(key.clone(), (cx.local.clone(), t), item(&key, orig_text(whole, cx), refs, None, None, None));
        }
    }
}

/// `int x[4]` -> `[4]`, `char *name` -> `*`: what a declarator adds to the type, without the name.
fn declarator_shape(n: Node, src: &[u8]) -> String {
    let mut c = n.walk();
    let mut v = vec![];
    for d in n.children_by_field_name("declarator", &mut c) {
        let t = text(d, src).to_string();
        let nm = declared_name(d).map(|x| text(x, src).to_string()).unwrap_or_default();
        v.push(if nm.is_empty() { t } else { t.replacen(&nm, "", 1) });
    }
    v.join(", ")
}

fn class_or_ns(scope: &Scope, n: &str) -> String { match &scope.class { Some(c) => format!("{c}::{n}"), None => scope.qual(n) } }

#[allow(clippy::too_many_arguments)]
fn function(n: Node, whole: Node, fd: Node, tp: &str, cx: &Cx, scope: &Scope, class: Option<&str>, out: &mut Out) {
    let src = cx.src;
    let Some(nm) = field(fd, "declarator") else { return };
    let raw = norm(text(nm, src));
    let (kparams, sparams) = params(fd, src);
    let (cvref, rest) = post_qualifiers(n, fd, src);
    let pre = pre_specifiers(n, src);
    // A function defined by a macro (`SYSCALL_DEFINE3(open, ...)`, `TEST(Suite, Name)`): no type, an upper-case
    // name. Its name is the whole declarator; nothing outside the file calls it by that name.
    let macro_made = field(n, "type").is_none() && raw.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') && class.is_none();
    // Out-of-line members (`Widget::resize`) are named as written, under the enclosing namespaces.
    let name = strip_args(&raw);
    let (qname, member_of) = match class {
        Some(c) => (format!("{c}::{name}"), Some(c.to_string())),
        None => {
            let q = scope.qual(raw.trim_start_matches("::"));
            let q = if raw.starts_with("::") { raw.trim_start_matches("::").to_string() } else { q };
            // `Widget::resize` defined outside its class: the body sees the class's names.
            let owner = if raw.contains("::") { strip_args(&q).rsplit_once("::").map(|(o, _)| o.to_string()) } else { None };
            (strip_args(&q), owner)
        }
    };
    let keyname = if macro_made { norm(text(fd, src)) } else if cx.cpp { format!("{qname}({kparams}){}", if cvref.is_empty() { String::new() } else { format!(" {cvref}") }) } else { qname.clone() };
    let key = match class { Some(c) => Key::Member(c.to_string(), Box::new(Key::Def(keyname.clone()))), None => Key::Def(keyname.clone()) };
    let ret = return_type(n, fd, src);
    let pre = if pre.is_empty() { String::new() } else { format!("{} ", pre.join(" ")) };
    let post = [cvref, rest].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" ");
    let sig = format!("{tp}{pre}({sparams}){}{post} -> {ret}{}", if post.is_empty() { "" } else { " " }, dialect_sig(&qname, cx));
    let mut fscope = scope.clone();
    if let Some(c) = &member_of { fscope.class = Some(c.clone()); }
    // A prototype has no body: its parameters are the only locals, and nothing reaches a member through them.
    let env = if field(n, "body").is_some() { local_types(whole, src) } else { BTreeMap::new() };
    let refs = refs_of(whole, cx, &fscope, &env);
    let returns = field(n, "body").map(|_| crate::returns_of(n, src, if cx.cpp { Lang::Cpp } else { Lang::C }));
    let module = if macro_made { cx.local.clone() } else { module_for(n, cx, scope) };
    // What the dialect adds for this name is part of its text: a change to it alone is a change to the function.
    let text = orig_text(whole, cx) + &dialect_sig(&qname, cx);
    out.put(key.clone(), (module, if macro_made { keyname } else { qname }), item(&key, text, refs, Some(sig), None, returns));
}

fn variable(n: Node, whole: Node, d: Node, cx: &Cx, scope: &Scope, class: Option<&str>, out: &mut Out) {
    let src = cx.src;
    let Some(nm) = declared_name(d) else { return };
    let name = norm(text(nm, src));
    if name.is_empty() { return; }
    let ty = field(n, "type").map(|t| format!("{}{}", layout_qualifier(t.start_byte(), cx).map(|q| format!("{q} ")).unwrap_or_default(), norm(text(t, src)))).unwrap_or_default();
    let shape = { let d = if d.kind() == "init_declarator" { field(d, "declarator").unwrap_or(d) } else { d }; text(d, src).replacen(text(nm, src), "", 1) };
    let pre = pre_specifiers(n, src).join(" ");
    let init = field(d, "value").or_else(|| field(n, "default_value")).map(|v| format!(" = {}", norm(text(v, src)))).unwrap_or_default();
    let bits = crate::children(n).into_iter().find(|c| c.kind() == "bitfield_clause").map(|b| format!(" {}", norm(text(b, src)))).unwrap_or_default();
    let sig = format!("{pre} {ty} {}{bits}{init}{}", norm(&shape), dialect_sig(&name, cx)).trim().to_string();
    let (key, sym) = match class {
        Some(c) => (Key::Member(c.to_string(), Box::new(Key::Assign(name.clone()))), format!("{c}::{name}")),
        None => { let q = if cx.cpp { scope.qual(&name) } else { name.clone() }; (Key::Assign(q.clone()), q) }
    };
    // An initialiser rarely reaches a member through a local: no local types to look up.
    let refs = refs_of(whole, cx, scope, &BTreeMap::new());
    let parts = field(n, "type").and_then(|t| base_type(t, src)).map(|t| BTreeMap::from([(TYPE_PART.to_string(), t)]));
    let module = if class.is_some() { MODULE.to_string() } else { module_for(n, cx, scope) };
    out.put(key.clone(), (module, sym), item(&key, orig_text(whole, cx) + &dialect_sig(&name, cx), refs, Some(sig), parts, None));
}

/// The layout callers rely on: base classes, data members in order (type, declarator, bit-field width), the
/// virtual functions (the table of a polymorphic class). Static members and member functions leave it as is.
fn layout_of(t: Node, cx: &Cx) -> String {
    let src = cx.src;
    let mut v = vec![];
    if let Some(q) = layout_qualifier(t.start_byte(), cx) { v.push(q.to_string()); }
    if let Some(b) = crate::children(t).into_iter().find(|c| c.kind() == "base_class_clause") { v.push(norm(text(b, src))); }
    if let Some(body) = field(t, "body") {
        let mut stack: Vec<Node> = crate::children(body).into_iter().rev().collect();
        while let Some(f) = stack.pop() {
            match f.kind() {
                "field_declaration" | "declaration" => {
                    let mut c = f.walk();
                    let ds: Vec<Node> = f.children_by_field_name("declarator", &mut c).collect();
                    let is_fn = ds.iter().any(|d| function_declarator(*d).is_some());
                    let st = crate::children(f).iter().any(|c| c.kind() == "storage_class_specifier" && matches!(text(*c, src).trim(), "static" | "typedef"));
                    if is_fn {
                        if crate::children_all(f).iter().any(|c| c.kind() == "virtual") { v.push(norm(text(f, src)).trim_end_matches(';').to_string()); }
                    } else if !st { v.push(norm(text(f, src)).trim_end_matches(';').to_string()); }
                }
                "function_definition" => if crate::children_all(f).iter().any(|c| c.kind() == "virtual") {
                    let end = field(f, "body").map(|b| b.start_byte()).unwrap_or(f.end_byte());
                    v.push(norm(&String::from_utf8_lossy(&src[f.start_byte()..end])));
                },
                "ERROR" => for c in crate::children(f).into_iter().rev() { stack.push(c); },
                _ => {}
            }
        }
    }
    v.join("; ")
}

/// A struct, union, class or enum with a body, and its members.
fn type_item(t: Node, tmpl: Option<Node>, tp: &str, typedef_name: Option<String>, cx: &Cx, scope: &Scope, out: &mut Out) {
    let src = cx.src;
    let tag = t.kind().trim_end_matches("_specifier");
    let written = field(t, "name").map(|n| norm(text(n, src)));
    let Some(name) = written.clone().or(typedef_name) else {
        // An anonymous struct or enum: its fields or constants are still names (C11 anonymous members, enum constants).
        if tag == "enum" { enumerators(t, None, cx, scope, out); }
        return;
    };
    let whole = tmpl.unwrap_or(t);
    let q = if cx.cpp { class_or_ns(scope, &name) } else { name.clone() };
    // C keeps tags apart from other names (`struct stat` and `stat()`); an anonymous typedef'd struct has only its typedef name.
    let sym = if !cx.cpp && written.is_some() { format!("{tag} {q}") } else { q.clone() };
    let key = Key::Class(sym.clone());
    let refs = refs_of(whole, cx, scope, &BTreeMap::new());
    let module = if scope.local { cx.local.clone() } else { MODULE.to_string() };
    if tag == "enum" {
        let base = field(t, "base").map(|b| format!(" : {}", norm(text(b, src)))).unwrap_or_default();
        let scoped = crate::children_all(t).iter().any(|c| matches!(c.kind(), "class" | "struct"));
        let parts = enum_values(t, src);
        out.put(key.clone(), (module, sym), item(&key, orig_text(whole, cx), refs, Some(format!("enum{}{base}", if scoped { " class" } else { "" })), Some(parts), None));
        enumerators(t, Some(&q), cx, scope, out);
        return;
    }
    out.put(key.clone(), (module, sym), item(&key, orig_text(whole, cx), refs, Some(format!("{tp}{tag} {{{}}}", layout_of(t, cx))), None, None));
    members(t, &q, cx, scope, out);
}

/// Effective value of each enum constant: written, or the previous one plus one.
fn enum_values(t: Node, src: &[u8]) -> BTreeMap<String, String> {
    let mut parts = BTreeMap::new();
    let Some(body) = field(t, "body") else { return parts };
    let (mut base, mut off) = ("0".to_string(), 0usize);
    for e in crate::children(body).into_iter().filter(|c| c.kind() == "enumerator") {
        let Some(nm) = field(e, "name") else { continue };
        if let Some(v) = field(e, "value") { base = norm(text(v, src)); off = 0; }
        parts.insert(text(nm, src).to_string(), if off == 0 { base.clone() } else { format!("{base}+{off}") });
        off += 1;
    }
    parts
}

fn enumerators(t: Node, owner: Option<&str>, cx: &Cx, scope: &Scope, out: &mut Out) {
    let src = cx.src;
    let Some(body) = field(t, "body") else { return };
    let scoped = crate::children_all(t).iter().any(|c| matches!(c.kind(), "class" | "struct"));
    let values = enum_values(t, src);
    let module = if scope.local { cx.local.clone() } else { MODULE.to_string() };
    for e in crate::children(body).into_iter().filter(|c| c.kind() == "enumerator") {
        let Some(nm) = field(e, "name") else { continue };
        let n = text(nm, src).to_string();
        // C and unscoped C++ enum constants are names of the enclosing scope; `enum class` constants are the enum's.
        let sym = match (owner, scoped) { (Some(o), true) => format!("{o}::{n}"), _ => if cx.cpp { class_or_ns(scope, &n) } else { n.clone() } };
        let key = match owner { Some(o) => Key::Member(o.to_string(), Box::new(Key::Assign(n.clone()))), None => Key::Assign(n.clone()) };
        let refs = refs_of(e, cx, scope, &BTreeMap::new());
        // The value goes into the text: a constant inserted above this one changes its value, not its line.
        let v = values.get(&n).cloned().unwrap_or_default();
        out.put(key.clone(), (module.clone(), sym), item(&key, format!("{} /* = {v} */", orig_text(e, cx)), refs, Some(format!("= {v}")), None, None));
    }
}

/// Members of a struct, union or class `q`: data members, member functions, nested types and aliases.
fn members(t: Node, q: &str, cx: &Cx, scope: &Scope, out: &mut Out) {
    let src = cx.src;
    let Some(body) = field(t, "body") else { return };
    let mut cscope = scope.clone();
    cscope.class = Some(q.to_string());
    let mut stack: Vec<(Node, Option<Node>, String)> = crate::children(body).into_iter().rev().map(|c| (c, None, String::new())).collect();
    while let Some((m, tmpl, tp)) = stack.pop() {
        let whole = tmpl.unwrap_or(m);
        match m.kind() {
            "field_declaration" | "declaration" | "function_definition" => {
                let mut c = m.walk();
                let ds: Vec<Node> = m.children_by_field_name("declarator", &mut c).collect();
                if let Some(ty) = field(m, "type").filter(|ty| field(*ty, "body").is_some()) {
                    if field(ty, "name").is_some() || ty.kind() == "enum_specifier" { type_item(ty, None, "", None, cx, &cscope, out); }
                    else if ds.is_empty() { members(ty, q, cx, scope, out); }   // anonymous struct or union member: its fields are the class's
                }
                for d in ds {
                    match function_declarator(d) {
                        Some(fd) => function(m, whole, fd, &tp, cx, &cscope, Some(q), out),
                        None => variable(m, whole, d, cx, &cscope, Some(q), out),
                    }
                }
            }
            "template_declaration" => {
                let tpl = field(m, "parameters").map(|p| format!("template{} ", norm(text(p, src)))).unwrap_or_default();
                if let Some(i) = crate::children(m).into_iter().find(|c| !matches!(c.kind(), "template_parameter_list" | "comment" | "requires_clause")) { stack.push((i, Some(m), tpl)); }
            }
            "alias_declaration" | "type_definition" | "struct_specifier" | "union_specifier" | "class_specifier" | "enum_specifier" => decl(m, tmpl, &tp, cx, &cscope, out),
            "preproc_def" | "preproc_function_def" => top(m, cx, scope, out),
            "ERROR" => for c in crate::children(m).into_iter().rev() { stack.push((c, None, String::new())); },
            _ => {}
        }
    }
}

// ---- includes

/// Where `#include` may find a header, as `imports_of` hands it to the caller: `cinc:` + quote kind + the spec,
/// then candidate paths, tab separated, most likely first.
fn include_module(includer: &str, spec: &str, quoted: bool) -> String {
    let mut c: Vec<String> = vec![];
    let dir = includer.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let join = |base: &str| -> Option<String> {
        let mut parts: Vec<&str> = base.split('/').filter(|s| !s.is_empty()).collect();
        for seg in spec.split('/') { match seg { "." | "" => {}, ".." => { parts.pop()?; }, s => parts.push(s) } }
        Some(parts.join("/"))
    };
    if quoted { if let Some(p) = join(dir) { c.push(p); } }
    if let Ok(s) = SETTINGS.read() { for d in &s.dirs { if let Some(p) = join(d) { c.push(p); } } }
    for d in ["include", "src"] { if quoted || d == "include" { if let Some(p) = join(d) { c.push(p); } } }
    if quoted { if let Some(p) = join("") { c.push(p); } }
    let mut seen = BTreeSet::new();
    c.retain(|x| seen.insert(x.clone()));
    format!("cinc:{}{spec}\t{}", if quoted { '"' } else { '<' }, c.join("\t"))
}

/// The repository files an include names (from `imports_of`): the first candidate that exists; a quoted include
/// with none falls back to files whose path ends in the spec. `None` when `m` is not an include.
pub fn resolve_include<'a>(m: &str, exists: impl Fn(&str) -> bool, all: impl Iterator<Item = &'a str>) -> Option<Vec<String>> {
    let rest = m.strip_prefix("cinc:")?;
    let mut parts = rest.split('\t');
    let head = parts.next().unwrap_or("");
    let (quoted, spec) = (head.starts_with('"'), &head[1.min(head.len())..]);
    if let Some(p) = parts.find(|p| exists(p)) { return Some(vec![p.to_string()]); }
    if !quoted || spec.is_empty() { return Some(vec![]); }
    let suffix = format!("/{spec}");
    Some(all.filter(|p| p.ends_with(&suffix)).map(String::from).collect())
}

/// Headers that `#include "..."` lines of C++ sources name, matched to repository headers.
pub fn cpp_included<'a>(sources: impl Iterator<Item = (&'a str, &'a [u8])>, headers: &BTreeSet<String>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (p, src) in sources {
        for line in String::from_utf8_lossy(src).lines() {
            let l = line.trim_start();
            let Some(r) = l.strip_prefix('#') else { continue };
            let Some(r) = r.trim_start().strip_prefix("include") else { continue };
            let r = r.trim();
            let (Some(s), true) = (r.strip_prefix('"').and_then(|x| x.split('"').next()), r.starts_with('"')) else { continue };
            if !s.ends_with(".h") { continue; }
            let m = include_module(p, s, true);
            if let Some(v) = resolve_include(&m, |c| headers.contains(c), headers.iter().map(String::as_str)) { out.extend(v); }
        }
    }
    out
}

// ---- severity and receivers

/// How severe a change to a C or C++ item is for code that uses it.
pub fn severity(old: &Item, new: Option<&Item>) -> &'static str {
    let Some(new) = new else { return "high" };
    if old.sig != new.sig {
        if let (Some(a), Some(b)) = (&old.sig, &new.sig) { if appended_defaults(a, b) { return "low"; } }
        return "high";
    }
    if old.returns != new.returns { return "high"; }
    if let (Some(a), Some(b)) = (&old.parts, &new.parts) { if a.iter().any(|(k, v)| b.get(k) != Some(v)) { return "high"; } }
    "low"
}

/// The parameter list of a signature `pre(params) post -> ret`, and the rest around it.
fn split_sig(s: &str) -> Option<(&str, &str, &str)> {
    let open = s.find('(')?;
    let mut depth = 0;
    for (i, c) in s[open..].char_indices() {
        match c { '(' => depth += 1, ')' => { depth -= 1; if depth == 0 { return Some((&s[..open], &s[open + 1..open + i], &s[open + i + 1..])); } } _ => {} }
    }
    None
}

/// C++ parameters appended with default values: every existing call compiles and means the same.
fn appended_defaults(a: &str, b: &str) -> bool {
    let (Some((pa, xa, ra)), Some((pb, xb, rb))) = (split_sig(a), split_sig(b)) else { return false };
    if pa != pb || ra != rb || !xb.starts_with(xa) { return false; }
    let added = &xb[xa.len()..];
    let added = if xa.is_empty() { added } else { match added.strip_prefix(", ") { Some(x) => x, None => return false } };
    !added.is_empty() && added.split(", ").all(|p| p.contains(" = "))
}

/// A struct field changed (`T::f`): code that reaches it through a global of type T declared in another file
/// (`server.port` with `extern struct redisServer server;` in a header) names the receiver `c?server`. Find the
/// globals of each changed struct among the side's files and add those receiver symbols with the same severity.
pub fn receivers(x: &Snapshot, changed: &mut BTreeMap<(String, String), &'static str>) {
    let fields: Vec<(String, String, &'static str)> = changed.iter().filter(|((m, n), _)| m == MODULE && n.contains("::"))
        .filter_map(|((_, n), s)| n.rsplit_once("::").map(|(t, f)| (t.to_string(), f.to_string(), *s))).collect();
    if fields.is_empty() { return; }
    let types: BTreeSet<&str> = fields.iter().map(|(t, _, _)| search_name(t)).collect();
    for (p, v) in x {
        if !matches!(crate::lang_of(p), Some(Lang::C | Lang::Cpp)) { continue; }
        if !types.iter().any(|t| !t.is_empty() && v.windows(t.len()).any(|w| w == t.as_bytes())) { continue; }
        let Some(info) = crate::analyze_file(p, v) else { continue };
        for (k, it) in &info.items {
            let Some(ty) = it.parts.as_ref().and_then(|m| m.get(TYPE_PART)) else { continue };
            // A global variable (`server`), or a member whose type is the struct (`Curl_easy::set`).
            let holders: Vec<String> = match k {
                Key::Assign(var) => vec![var.clone()],
                Key::Member(c, inner) => match &**inner { Key::Assign(f) => { let mut v = vec![format!("{c}::{f}")]; if search_name(c) != c { v.push(format!("{}::{f}", search_name(c))); } v } _ => continue },
                _ => continue,
            };
            for (t, f, s) in &fields {
                if t != ty && search_name(t) != ty { continue; }
                for h in &holders {
                    let e = changed.entry((format!("{RECV}{h}"), f.clone())).or_insert(s);
                    if *s == "high" { *e = "high"; }
                }
            }
        }
    }
}

/// Key of a top-level node for the structural merge (the reader above keys more finely).
pub(crate) fn merge_key(n: Node, src: &[u8], lang: Lang) -> Key {
    let normt = || norm(text(n, src));
    match n.kind() {
        "preproc_include" | "using_declaration" | "namespace_alias_definition" => Key::Import(normt()),
        "preproc_def" | "preproc_function_def" => Key::Def(format!("#define {}", field(n, "name").map(|x| text(x, src)).unwrap_or(""))),
        "namespace_definition" => Key::Class(format!("namespace {}", field(n, "name").map(|x| norm(text(x, src))).unwrap_or_default())),
        "linkage_specification" => Key::Class(format!("extern {}", field(n, "value").map(|x| text(x, src)).unwrap_or(""))),
        "struct_specifier" | "union_specifier" | "class_specifier" | "enum_specifier" => match field(n, "name") {
            Some(nm) => Key::Class(format!("{} {}", n.kind(), norm(text(nm, src)))), None => Key::Opaque(normt()) },
        "template_declaration" => match crate::children(n).into_iter().find(|c| c.kind() != "template_parameter_list") {
            Some(i) => match merge_key(i, src, lang) { Key::Def(d) => Key::Def(format!("template {d}")), Key::Class(c) => Key::Class(format!("template {c}")), k => k },
            None => Key::Opaque(normt()),
        },
        "function_definition" | "declaration" | "field_declaration" | "type_definition" | "alias_declaration" => {
            let mut c = n.walk();
            let d: Vec<Node> = n.children_by_field_name("declarator", &mut c).collect();
            if n.kind() == "alias_declaration" { return Key::Def(format!("using {}", field(n, "name").map(|x| text(x, src)).unwrap_or(""))); }
            if d.len() != 1 {
                return match field(n, "type").filter(|t| field(*t, "body").is_some()).and_then(|t| field(t, "name")) {
                    Some(nm) => Key::Class(format!("{} {}", n.kind(), norm(text(nm, src)))), None => Key::Opaque(normt()) };
            }
            match function_declarator(d[0]) {
                Some(fd) => {
                    let nm = field(fd, "declarator").map(|x| norm(text(x, src))).unwrap_or_default();
                    let ps = if lang == Lang::Cpp { format!("({})", params(fd, src).0) } else { String::new() };
                    // A prototype is its own key: a file may declare a function before defining it.
                    if n.kind() == "function_definition" { Key::Def(format!("{nm}{ps}")) } else { Key::Def(format!("{nm}{ps};")) }
                }
                None => match declared_name(d[0]) {
                    Some(nm) if n.kind() == "type_definition" => Key::Def(format!("typedef {}", norm(text(nm, src)))),
                    Some(nm) => Key::Assign(norm(text(nm, src))),
                    None => Key::Opaque(normt()),
                },
            }
        }
        _ => Key::Opaque(text(n, src).to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preprocessing_keeps_offsets() {
        let s = b"#ifdef A\nint f(int a) {\n#else\nint f(long a) {\n#endif\n  return 0;\n}\n#if 0\nbroken (\n#endif\n";
        let p = prepare(s);
        assert_eq!(p.len(), s.len());
        let t = String::from_utf8(p).unwrap();
        assert!(t.contains("int f(int a) {") && !t.contains("long") && !t.contains("broken") && !t.contains('#'));
        assert_eq!(norm("unsigned  int /* x */ *  p"), "unsigned int*p");
        assert_eq!(strip_args("std::vector<std::pair<int,int>>::iterator"), "std::vector::iterator");
        assert!(appended_defaults("(int) -> void", "(int, bool = false) -> void") && !appended_defaults("(int) -> void", "(int, bool) -> void"));
        assert_eq!(search_name("ns::Widget::resize"), "resize");
        assert_eq!(search_name("struct conn"), "conn");
    }
}
