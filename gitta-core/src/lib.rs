//! Gitta core: symbol extraction and cross-agent hazard analysis (Python, TypeScript, TSX, Go, Rust, Java, C#, Kotlin, C, C++, COBOL, JCL).
//!
//! Given a base snapshot and the current states of two agents, report
//!   OVERLAP  both agents changed the same symbol (or the same non-code file)
//!   HAZARD   one agent changed/removed a symbol that the other agent's NEW code references
//!   UNPARSED a changed source file does not parse (not analyzed, never treated as clean)
//!   SECURITY one agent weakened a security control (see security.rs) that the other agent's NEW code uses
//! Semantics match the Python prototype (merge_ts.hazards / watch.analyze) and add TypeScript.

use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use tree_sitter::{Node, Parser, Tree};

pub mod merge;
pub mod guard;
pub mod security;
pub mod code_security;
pub mod c_cpp;

pub type Snapshot = BTreeMap<String, Vec<u8>>;
pub const ATTR: &str = "<attribute>";

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, serde::Deserialize)]
pub enum Lang {
    Python, TypeScript, Tsx, Go, Rust, Java, CSharp, Kotlin, C, Cpp,
}

pub fn lang_of(path: &str) -> Option<Lang> {
    if path.ends_with(".py") { Some(Lang::Python) }
    else if path.ends_with(".go") { Some(Lang::Go) }
    else if path.ends_with(".rs") { Some(Lang::Rust) }
    else if path.ends_with(".java") { Some(Lang::Java) }
    else if path.ends_with(".cs") { Some(Lang::CSharp) }
    else if path.ends_with(".kt") || path.ends_with(".kts") { Some(Lang::Kotlin) }
    else if path.ends_with(".tsx") || path.ends_with(".jsx") { Some(Lang::Tsx) }
    else if [".ts", ".mts", ".cts", ".js", ".mjs", ".cjs"].iter().any(|e| path.ends_with(e)) { Some(Lang::TypeScript) }
    else { abap_lang(path).or_else(|| other_lang_of(path)) }
}

fn abap_lang(_path: &str) -> Option<Lang> { None }

fn other_lang_of(path: &str) -> Option<Lang> { c_cpp::lang_of(path) }

/// The language a file is read in, with its text: a `.h` file may be C or C++ (see c_cpp.rs).
pub fn lang_for(path: &str, src: &[u8]) -> Option<Lang> {
    if path.ends_with(".h") { return Some(c_cpp::header_lang(path, Some(src))); }
    lang_of(path)
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, serde::Deserialize)]
pub enum Key {
    Def(String),
    Class(String),
    Assign(String),
    Import(String),
    Docstring,
    Opaque(String),
    Member(String, Box<Key>),
}

impl Key {
    fn bound(&self) -> Option<&str> {
        match self { Key::Def(n) | Key::Class(n) | Key::Assign(n) => Some(n), _ => None }
    }
    /// The same kind of key with another name (for a key whose symbol is listed in `FileInfo::symbols`).
    fn renamed(&self, n: &str) -> Key {
        match self { Key::Def(_) => Key::Def(n.into()), Key::Class(_) => Key::Class(n.into()), Key::Assign(_) => Key::Assign(n.into()), k => k.clone() }
    }
    fn label(&self) -> String {
        match self {
            Key::Def(n) => format!("def {n}"), Key::Class(n) => format!("class {n}"), Key::Assign(n) => format!("assign {n}"),
            Key::Import(t) => format!("import {t}"), Key::Docstring => "docstring".into(), Key::Opaque(_) => "statement".into(),
            Key::Member(c, k) => format!("{c}.{}", k.label()),
        }
    }
}

/// One keyed item with the references found inside it.
#[derive(Default, Clone, Debug, Serialize, serde::Deserialize)]
pub struct Refs {
    pub idents: BTreeSet<String>,
    pub members: BTreeSet<(String, String)>, // (object identifier, property)
    pub props: BTreeSet<String>,             // every `.prop` used, any object
    pub paths: BTreeSet<(String, String)>,   // Rust `a::b::name` paths: (raw path, name)
    /// References the parser already resolved to (module, name): COBOL names of a nested program's own scope,
    /// GLOBAL items of an enclosing program, EXTERNAL data, INVOKE on a known class.
    pub qual: BTreeSet<(String, String)>,
}

#[derive(Clone, Debug, Serialize, serde::Deserialize)]
pub struct Item {
    pub key: Key, pub text: String, pub refs: Refs,
    /// Interface of the item: parameters + return type for functions/methods.
    pub sig: Option<String>,
    /// Named parts for object literals, interfaces/type literals, enums: name -> text (optional fields end with '?').
    pub parts: Option<BTreeMap<String, String>>,
    /// What a function hands back: normalized text of its return statements.
    pub returns: Option<String>,
    /// C# `this` parameter / Kotlin receiver type: callers write `value.name(...)` with no class in sight.
    pub ext: bool,
}

#[derive(Default, Debug, Clone)]
pub struct FileInfo {
    pub items: BTreeMap<Key, Item>,
    pub aliases: BTreeMap<String, (String, Option<String>)>, // local name -> (module, name or None for a namespace)
    pub instance_attrs: BTreeSet<String>,                     // self.X = / this.X = / class fields
    pub attr_init: BTreeMap<String, String>,                  // attribute -> how the constructor initialises it (its shape)
    pub lang: Option<Lang>,
    pub module: String,
    /// Modules whose names are all in scope: C# `using N;` and enclosing namespaces, Kotlin `import a.b.*`.
    pub opens: BTreeSet<String>,
    /// The symbol (module, name) of a key whose symbol is not (`module`, its name): the items of the second and
    /// later programs of a COBOL source file, and EXTERNAL data, which every program that declares it shares.
    /// The bare names such a key uses are not in `module`'s scope (they are resolved in `Refs::qual`).
    pub symbols: BTreeMap<Key, (String, String)>,
}

/// Go, Rust, Java, C# and Kotlin: languages added after Python/TypeScript (module-scoped member resolution).
fn is_new_lang(l: Lang) -> bool { matches!(l, Lang::Go | Lang::Rust | Lang::Java | Lang::CSharp | Lang::Kotlin) }

pub(crate) fn is_trivia(kind: &str) -> bool {
    matches!(kind, "comment" | "line_comment" | "block_comment" | "attribute_item" | "inner_attribute_item"
        | "preproc_region" | "preproc_endregion" | "preproc_pragma" | "preproc_nullable")
}

/// The body of a class or function. Kotlin's grammar has no `body` field: the class body, or the block
/// inside a function body, is a plain child.
pub(crate) fn body_node<'t>(n: Node<'t>, lang: Lang, field: &str) -> Option<Node<'t>> {
    if lang != Lang::Kotlin { return n.child_by_field_name(field); }
    let b = children(n).into_iter().find(|c| matches!(c.kind(), "class_body" | "enum_class_body" | "function_body" | "block"))?;
    if b.kind() == "function_body" { return children(b).into_iter().find(|c| c.kind() == "block"); }
    Some(b)
}

/// The namespace (C#) or package (Kotlin) a file declares; the global namespace / default package is "".
/// A C# file with several namespaces is named after the first; the others are prefixed onto their type names.
fn declared_module(root: Node, src: &[u8], lang: Lang) -> String {
    let norm = |n: Node| text(n, src).chars().filter(|c| !c.is_whitespace()).collect::<String>();
    for c in children(root) {
        match (lang, c.kind()) {
            (Lang::Kotlin, "package_header") => {
                return children(c).into_iter().find(|x| x.kind() == "qualified_identifier" || x.kind() == "identifier").map(norm).unwrap_or_default();
            }
            (Lang::CSharp, "namespace_declaration" | "file_scoped_namespace_declaration") => {
                return c.child_by_field_name("name").map(norm).unwrap_or_default();
            }
            _ => {}
        }
    }
    String::new()
}

/// Bare type name of a receiver / impl target: `*T`, `&T`, `T<U>`, `a::T` -> T.
fn base_type_name(n: Node, src: &[u8]) -> String {
    let mut x = n;
    loop {
        let next = match x.kind() {
            "type_identifier" | "identifier" => return text(x, src).to_string(),
            "parameter_list" => children(x).into_iter().find(|c| c.kind() == "parameter_declaration"),
            "parameter_declaration" => x.child_by_field_name("type"),
            "pointer_type" => children(x).into_iter().next(),
            "reference_type" => x.child_by_field_name("type"),
            "generic_type" => x.child_by_field_name("type"),
            "scoped_type_identifier" => x.child_by_field_name("name"),
            _ => None,
        };
        match next { Some(nx) => x = nx, None => return text(x, src).to_string() }
    }
}

/// Module that a Rust path resolves to (crate::, self::, super::), joined with "::".
fn rust_resolve(module: &str, segs: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut it = segs.iter().peekable();
    match it.peek().copied().copied() {
        Some("crate") => { it.next(); }
        Some("self") => { it.next(); out = module.split("::").filter(|x| !x.is_empty()).map(String::from).collect(); }
        Some("super") => {
            out = module.split("::").filter(|x| !x.is_empty()).map(String::from).collect();
            while it.peek().copied().copied() == Some("super") { it.next(); out.pop(); }
        }
        _ => {}
    }
    out.extend(it.map(|s| s.to_string()));
    out
}

fn rust_use(n: Node, src: &[u8], prefix: &[String], module: &str, out: &mut BTreeMap<String, (String, Option<String>)>) {
    let segs = |t: &str| -> Vec<String> { t.split("::").map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect() };
    let full = |extra: Vec<String>| -> Vec<String> {
        let mut v = prefix.to_vec(); v.extend(extra);
        let r: Vec<&str> = v.iter().map(String::as_str).collect();
        rust_resolve(module, &r)
    };
    fn put(out: &mut BTreeMap<String, (String, Option<String>)>, path: Vec<String>, local: Option<String>) {
        let Some((last, m)) = path.split_last() else { return };
        let local = local.unwrap_or(last.clone());
        if local == "self" { return; }
        out.insert(local, (m.join("::"), Some(last.clone())));
    }
    match n.kind() {
        "identifier" | "scoped_identifier" | "crate" | "self" | "super" => {
            let raw = segs(text(n, src));
            if raw.last().map(|s| s == "self").unwrap_or(false) {
                // `use a::b::{self}` -> b
                let mut p = full(raw); p.pop(); put(out, p, None);
            } else { put(out, full(raw), None); }
        }
        "use_as_clause" => {
            let p = n.child_by_field_name("path").map(|x| segs(text(x, src))).unwrap_or_default();
            let a = n.child_by_field_name("alias").map(|x| text(x, src).to_string());
            put(out, full(p), a);
        }
        "scoped_use_list" => {
            let mut p = prefix.to_vec();
            if let Some(x) = n.child_by_field_name("path") { p.extend(segs(text(x, src))); }
            if let Some(l) = n.child_by_field_name("list") { for c in children(l) { rust_use(c, src, &p, module, out); } }
        }
        "use_list" => for c in children(n) { rust_use(c, src, prefix, module, out); },
        _ => {}
    }
}

pub(crate) fn parser(lang: Lang) -> Parser {
    let mut p = Parser::new();
    let l: tree_sitter::Language = match lang {
        Lang::Python => tree_sitter_python::LANGUAGE.into(),
        Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        Lang::Go => tree_sitter_go::LANGUAGE.into(),
        Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
        Lang::Java => tree_sitter_java::LANGUAGE.into(),
        Lang::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
        Lang::Kotlin => tree_sitter_kotlin_ng::LANGUAGE.into(),
        Lang::C => tree_sitter_c::LANGUAGE.into(),
        Lang::Cpp => tree_sitter_cpp::LANGUAGE.into(),
    };
    p.set_language(&l).expect("grammar");
    p
}

fn text<'a>(n: Node, src: &'a [u8]) -> &'a str { n.utf8_text(src).unwrap_or("") }

/// Pre-order walk over `n` and its named descendants with one cursor (no allocation per node).
/// `f` returns false to skip a node's descendants.
pub(crate) fn walk<'t>(n: Node<'t>, mut f: impl FnMut(Node<'t>) -> bool) {
    let mut c = n.walk();
    let mut down = f(n);
    loop {
        if !(down && c.goto_first_child()) {
            loop {
                if c.node() == n { return; }
                if c.goto_next_sibling() { break; }
                if !c.goto_parent() { return; }
            }
        }
        let x = c.node();
        down = if x.is_named() { f(x) } else { x.child_count() > 0 };
    }
}

pub(crate) fn children(n: Node) -> Vec<Node> {
    let mut c = n.walk();
    n.named_children(&mut c).collect()
}

/// All children, keywords and punctuation included.
pub(crate) fn children_all(n: Node) -> Vec<Node> {
    let mut c = n.walk();
    n.children(&mut c).collect()
}

pub(crate) fn line_end(src: &[u8], pos: usize) -> usize {
    src[pos..].iter().position(|&b| b == b'\n').map(|i| pos + i + 1).unwrap_or(src.len())
}

/// Module identity of a file: dotted for Python, path without extension for TS/JS.
/// C# and Kotlin files are analysed under the namespace/package they declare; this path-based name is the
/// conventional guess (Kotlin `src/main/kotlin/a/b`, C# `src/Shop.Pricing/Models`) used to find files by import.
pub fn module_name(path: &str) -> String {
    // C and C++ symbols are global (module `c`); a file is found by its path, which is what `#include` names.
    if c_cpp::lang_of(path).is_some() { return path.to_string(); }
    let dir = || path.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
    if path.ends_with(".go") { return dir(); }
    if path.ends_with(".java") || path.ends_with(".kt") || path.ends_with(".kts") {
        let d = format!("/{}/", dir());
        let d = match ["/kotlin/", "/java/"].iter().filter_map(|r| d.rfind(r).map(|i| i + r.len())).max() { Some(i) => &d[i..], None => &d[..] };
        return d.trim_matches('/').replace('/', ".");
    }
    if path.ends_with(".cs") {
        let d = dir();
        let d = match d.rfind("/src/") { Some(i) => &d[i + 5..], None => d.strip_prefix("src/").unwrap_or(if d == "src" { "" } else { &d }) };
        return d.replace('/', ".");
    }
    if let Some(p) = path.strip_suffix(".rs") {
        let p = match p.rfind("/src/") { Some(i) => &p[i + 5..], None => p.strip_prefix("src/").unwrap_or(p) };
        let p = p.strip_suffix("/mod").unwrap_or(p);
        if p == "lib" || p == "main" || p == "mod" { return String::new(); }
        return p.replace('/', "::");
    }
    if let Some(p) = path.strip_suffix(".py") {
        // The "src layout" (src/package/module.py) keeps packages under src/; it is not part of the import name.
        let p = match p.rfind("/src/") { Some(i) => &p[i + 5..], None => p.strip_prefix("src/").unwrap_or(p) };
        let m = p.replace('/', ".");
        return m.strip_suffix(".__init__").map(str::to_string).unwrap_or(m);
    }
    let mut p = path.to_string();
    for ext in [".tsx", ".ts", ".mts", ".cts", ".jsx", ".js", ".mjs", ".cjs"] {
        if let Some(s) = p.strip_suffix(ext) { p = s.to_string(); break; }
    }
    p.strip_suffix("/index").map(str::to_string).unwrap_or(p)
}

/// Resolve a Python relative import (`.utils`, `..models.user`) against the importing file's package.
fn resolve_py(importer: &str, spec: &str) -> String {
    let dots = spec.chars().take_while(|c| *c == '.').count();
    if dots == 0 { return spec.to_string(); }
    let me = module_name(importer);
    let mut parts: Vec<&str> = if me.is_empty() { vec![] } else { me.split('.').collect() };
    // A module's package is its parent; a package's __init__ is the package itself.
    if !importer.ends_with("__init__.py") { parts.pop(); }
    for _ in 1..dots { parts.pop(); }
    let rest = &spec[dots..];
    if !rest.is_empty() { parts.push(rest); }
    parts.join(".")
}

/// Resolve a TS import specifier relative to the importing file. `@/x` is the Next.js root alias.
fn resolve_ts(importer: &str, spec: &str) -> String {
    let spec = spec.trim_matches(|c| c == '"' || c == '\'' || c == '`');
    let raw = if let Some(rest) = spec.strip_prefix("@/") { rest.to_string() }
    else if spec.starts_with('.') {
        let dir = importer.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        let mut parts: Vec<&str> = if dir.is_empty() { vec![] } else { dir.split('/').collect() };
        for seg in spec.split('/') {
            match seg { "." | "" => {}, ".." => { parts.pop(); }, s => parts.push(s) }
        }
        parts.join("/")
    } else { return spec.to_string() };
    module_name(&raw)   // './pricing.js' and './pricing' both name src/pricing.ts
}

fn collect_refs(n: Node, src: &[u8], lang: Lang, refs: &mut Refs) {
    walk(n, |x| {
        match x.kind() {
            "identifier" | "type_identifier" | "shorthand_property_identifier" => { refs.idents.insert(text(x, src).to_string()); }
            "field_identifier" | "package_identifier" if is_new_lang(lang) => { refs.idents.insert(text(x, src).to_string()); }
            "selector_expression" | "field_expression" | "method_invocation" | "field_access" if is_new_lang(lang) => {
                let (of, pf) = match x.kind() { "selector_expression" => ("operand", "field"), "field_expression" => ("value", "field"),
                                                "method_invocation" => ("object", "name"), _ => ("object", "field") };
                if let (Some(o), Some(p)) = (x.child_by_field_name(of), x.child_by_field_name(pf)) {
                    if matches!(o.kind(), "identifier" | "package_identifier") { refs.members.insert((text(o, src).into(), text(p, src).into())); }
                    refs.props.insert(text(p, src).into());
                }
            }
            "member_access_expression" if lang == Lang::CSharp => {
                if let (Some(o), Some(p)) = (x.child_by_field_name("expression"), x.child_by_field_name("name")) {
                    if o.kind() == "identifier" { refs.members.insert((text(o, src).into(), text(p, src).into())); }
                    refs.props.insert(text(p, src).into());
                }
            }
            "navigation_expression" if lang == Lang::Kotlin => {
                // `obj.name` / `obj?.name`: the receiver is the first child, the member the last identifier.
                let c = children(x);
                if let (Some(o), Some(p)) = (c.first(), c.last().filter(|p| p.kind() == "identifier" && c.len() > 1)) {
                    if o.kind() == "identifier" { refs.members.insert((text(*o, src).into(), text(*p, src).into())); }
                    refs.props.insert(text(*p, src).into());
                }
            }
            "scoped_identifier" | "scoped_type_identifier" if lang == Lang::Rust => {
                if let (Some(p), Some(nm)) = (x.child_by_field_name("path"), x.child_by_field_name("name")) {
                    refs.paths.insert((text(p, src).split_whitespace().collect(), text(nm, src).into()));
                }
            }
            "attribute" if lang == Lang::Python => {
                if let (Some(o), Some(a)) = (x.child_by_field_name("object"), x.child_by_field_name("attribute")) {
                    if o.kind() == "identifier" { refs.members.insert((text(o, src).into(), text(a, src).into())); }
                    refs.props.insert(text(a, src).into());
                }
            }
            "member_expression" => {
                if let (Some(o), Some(p)) = (x.child_by_field_name("object"), x.child_by_field_name("property")) {
                    if o.kind() == "identifier" { refs.members.insert((text(o, src).into(), text(p, src).into())); }
                    refs.props.insert(text(p, src).into());
                }
            }
            _ => {}
        }
        true
    });
}

/// How each attribute is first set up: `self.X = <value>` inside __init__/constructor, or a class field's
/// initialiser. A different initial value means a different shape (list -> dict, None -> 0 ...).
fn attr_inits(n: Node, src: &[u8], lang: Lang, out: &mut BTreeMap<String, String>) {
    let (assign_kind, member_kind, field, me) = match lang {
        Lang::Python => ("assignment", "attribute", "attribute", "self"),
        _ => ("assignment_expression", "member_expression", "property", "this"),
    };
    walk(n, |x| {
        if lang != Lang::Python && x.kind() == "public_field_definition" {
            if let (Some(nm), Some(v)) = (x.child_by_field_name("name"), x.child_by_field_name("value")) { out.insert(text(nm, src).into(), text(v, src).split_whitespace().collect::<Vec<_>>().join(" ")); }
        }
        let ctor = matches!(x.kind(), "function_definition" | "method_definition" | "constructor_declaration")
            && x.child_by_field_name("name").map(|nm| matches!(text(nm, src), "__init__" | "constructor")).unwrap_or(x.kind() == "constructor_declaration");
        if !ctor { return true; }
        walk(x, |y| {
            if y.kind() == assign_kind {
                if let (Some(l), Some(r)) = (y.child_by_field_name("left"), y.child_by_field_name("right")) {
                    if l.kind() == member_kind && l.child_by_field_name("object").map(|o| text(o, src) == me).unwrap_or(false) {
                        if let Some(a) = l.child_by_field_name(field) { out.entry(text(a, src).into()).or_insert_with(|| text(r, src).split_whitespace().collect::<Vec<_>>().join(" ")); }
                    }
                }
            }
            true
        });
        false
    });
}

fn instance_attrs(n: Node, src: &[u8], lang: Lang, out: &mut BTreeSet<String>) {
    let (assign_kind, member_kind, field, me) = match lang {
        Lang::Python => ("assignment", "attribute", "attribute", "self"),
        _ => ("assignment_expression", "member_expression", "property", "this"),
    };
    walk(n, |x| {
        if x.kind() == assign_kind {
            if let Some(l) = x.child_by_field_name("left") {
                if l.kind() == member_kind && l.child_by_field_name("object").map(|o| text(o, src) == me).unwrap_or(false) {
                    if let Some(a) = l.child_by_field_name(field) { out.insert(text(a, src).into()); }
                }
            }
        }
        if lang != Lang::Python && x.kind() == "public_field_definition" {
            if let Some(nm) = x.child_by_field_name("name") { out.insert(text(nm, src).into()); }
        }
        true
    });
}

pub(crate) fn key_of(n: Node, src: &[u8], lang: Lang, first: bool) -> Key {
    let name = |m: Node| m.child_by_field_name("name").map(|x| text(x, src).to_string()).unwrap_or_default();
    let norm = |m: Node| text(m, src).split_whitespace().collect::<Vec<_>>().join(" ");
    match lang {
        Lang::C | Lang::Cpp => c_cpp::merge_key(n, src, lang),
        Lang::Go => match n.kind() {
            "function_declaration" => Key::Def(name(n)),
            "method_declaration" => {
                let recv = n.child_by_field_name("receiver").map(|r| base_type_name(r, src)).unwrap_or_default();
                Key::Member(recv, Box::new(Key::Def(name(n))))
            }
            "type_declaration" => match children(n).into_iter().find(|c| matches!(c.kind(), "type_spec" | "type_alias")) {
                Some(ts) => {
                    let ty = ts.child_by_field_name("type").map(|t| t.kind()).unwrap_or("");
                    if matches!(ty, "struct_type" | "interface_type") { Key::Class(name(ts)) } else { Key::Def(name(ts)) }
                }
                None => Key::Opaque(norm(n)),
            },
            "const_declaration" | "var_declaration" => {
                let spec = find_first(n, &["const_spec", "var_spec"]);
                match spec.and_then(|s| s.child_by_field_name("name")) { Some(nm) => Key::Assign(text(nm, src).into()), None => Key::Opaque(norm(n)) }
            }
            "import_declaration" => Key::Import(norm(n)),
            _ => Key::Opaque(text(n, src).into()),
        },
        Lang::Rust => match n.kind() {
            "function_item" | "function_signature_item" | "type_item" | "mod_item" | "macro_definition" | "associated_type" | "enum_variant" => Key::Def(name(n)),
            "struct_item" | "enum_item" | "trait_item" | "union_item" => Key::Class(name(n)),
            "impl_item" => {
                let end = n.child_by_field_name("body").map(|b| b.start_byte()).unwrap_or(n.end_byte());
                let head = String::from_utf8_lossy(&src[n.start_byte()..end]).split_whitespace().collect::<Vec<_>>().join(" ");
                Key::Class(head)
            }
            "const_item" | "static_item" | "field_declaration" => Key::Assign(name(n)),
            "use_declaration" | "extern_crate_declaration" => Key::Import(norm(n)),
            _ => Key::Opaque(text(n, src).into()),
        },
        Lang::Java => match n.kind() {
            "class_declaration" | "interface_declaration" | "enum_declaration" | "record_declaration" | "annotation_type_declaration" => Key::Class(name(n)),
            "method_declaration" | "constructor_declaration" | "compact_constructor_declaration" | "enum_constant" => Key::Def(name(n)),
            "field_declaration" | "constant_declaration" => {
                match n.child_by_field_name("declarator").and_then(|d| d.child_by_field_name("name")) {
                    Some(nm) => Key::Assign(text(nm, src).into()), None => Key::Opaque(norm(n)),
                }
            }
            "import_declaration" => Key::Import(norm(n)),
            _ => Key::Opaque(text(n, src).into()),
        },
        Lang::CSharp => match n.kind() {
            "class_declaration" | "struct_declaration" | "record_declaration" | "record_struct_declaration" | "interface_declaration" | "enum_declaration" => Key::Class(name(n)),
            // A block namespace is a container: the merge recurses into it like a class body.
            "namespace_declaration" => Key::Class(format!("namespace {}", n.child_by_field_name("name").map(norm).unwrap_or_default())),
            "method_declaration" | "constructor_declaration" | "destructor_declaration" | "local_function_statement" | "delegate_declaration"
            | "enum_member_declaration" => Key::Def(name(n)),
            "operator_declaration" | "conversion_operator_declaration" | "indexer_declaration" => {
                let end = n.child_by_field_name("body").map(|b| b.start_byte()).unwrap_or(n.end_byte());
                Key::Def(String::from_utf8_lossy(&src[n.start_byte()..end]).split_whitespace().collect::<Vec<_>>().join(" "))
            }
            "property_declaration" | "event_declaration" => Key::Assign(name(n)),
            "field_declaration" | "event_field_declaration" => {
                let v = children(n).into_iter().find(|c| c.kind() == "variable_declaration");
                let d = v.and_then(|v| children(v).into_iter().find(|c| c.kind() == "variable_declarator"));
                match d.and_then(|d| d.child_by_field_name("name")) { Some(nm) => Key::Assign(text(nm, src).into()), None => Key::Opaque(norm(n)) }
            }
            "using_directive" | "extern_alias_directive" | "file_scoped_namespace_declaration" => Key::Import(norm(n)),
            _ => Key::Opaque(text(n, src).into()),
        },
        Lang::Kotlin => match n.kind() {
            "class_declaration" | "object_declaration" => Key::Class(name(n)),
            "companion_object" => Key::Class(n.child_by_field_name("name").map(|x| text(x, src).to_string()).unwrap_or("Companion".into())),
            "function_declaration" => Key::Def(name(n)),
            "secondary_constructor" => Key::Def("constructor".into()),
            "enum_entry" => match children(n).into_iter().find(|c| c.kind() == "identifier") { Some(i) => Key::Def(text(i, src).into()), None => Key::Opaque(norm(n)) },
            "type_alias" => Key::Def(n.child_by_field_name("type").map(|t| text(t, src).to_string()).unwrap_or_default()),
            "property_declaration" => match kotlin_property_name(n, src) { Some(nm) => Key::Assign(nm), None => Key::Opaque(norm(n)) },
            "import" | "package_header" => Key::Import(norm(n)),
            _ => Key::Opaque(text(n, src).into()),
        },
        Lang::Python => {
            let inner = if n.kind() == "decorated_definition" { n.child_by_field_name("definition").unwrap_or(n) } else { n };
            let name = |m: Node| m.child_by_field_name("name").map(|x| text(x, src).to_string()).unwrap_or_default();
            match inner.kind() {
                "function_definition" => Key::Def(name(inner)),
                "class_definition" => Key::Class(name(inner)),
                "import_statement" | "import_from_statement" | "future_import_statement" => Key::Import(text(n, src).split_whitespace().collect::<Vec<_>>().join(" ")),
                "expression_statement" => {
                    let c = children(n);
                    if c.len() == 1 && c[0].kind() == "string" && first { return Key::Docstring; }
                    if c.len() == 1 && c[0].kind() == "assignment" {
                        if let Some(l) = c[0].child_by_field_name("left") { if l.kind() == "identifier" { return Key::Assign(text(l, src).into()); } }
                    }
                    Key::Opaque(text(n, src).into())
                }
                _ => Key::Opaque(text(n, src).into()),
            }
        }
        _ => {
            let n = if n.kind() == "export_statement" {
                match n.child_by_field_name("declaration") { Some(d) => d, None => {
                    if let Some(v) = n.child_by_field_name("value") { let _ = v; return Key::Assign("default".into()); }
                    return Key::Opaque(text(n, src).into());
                } }
            } else { n };
            let name = |m: Node| m.child_by_field_name("name").map(|x| text(x, src).to_string()).unwrap_or_default();
            match n.kind() {
                "function_declaration" | "generator_function_declaration" | "function_signature" | "interface_declaration"
                | "type_alias_declaration" | "enum_declaration" | "method_definition" | "method_signature" | "abstract_method_signature" => Key::Def(name(n)),
                "class_declaration" | "abstract_class_declaration" => Key::Class(name(n)),
                "public_field_definition" => Key::Assign(name(n)),
                "lexical_declaration" | "variable_declaration" => {
                    let d: Vec<Node> = children(n).into_iter().filter(|c| c.kind() == "variable_declarator").collect();
                    match d.first().and_then(|d| d.child_by_field_name("name")) {
                        Some(nm) if nm.kind() == "identifier" => Key::Assign(text(nm, src).into()),
                        _ => Key::Opaque(text(n, src).into()),
                    }
                }
                "import_statement" => Key::Import(text(n, src).split_whitespace().collect::<Vec<_>>().join(" ")),
                _ => Key::Opaque(text(n, src).into()),
            }
        }
    }
}

/// `val total: Int` -> total. Destructuring declarations (`val (a, b) = ...`) have no single name.
fn kotlin_property_name(n: Node, src: &[u8]) -> Option<String> {
    let v = children(n).into_iter().find(|c| c.kind() == "variable_declaration")?;
    children(v).into_iter().find(|c| c.kind() == "identifier").map(|i| text(i, src).to_string())
}

/// C# extension method (`this` on the first parameter) or Kotlin extension function/property (`fun Int.twice()`).
fn is_extension(n: Node, src: &[u8], lang: Lang) -> bool {
    match (lang, n.kind()) {
        (Lang::CSharp, "method_declaration") => n.child_by_field_name("parameters")
            .and_then(|p| children(p).into_iter().find(|c| c.kind() == "parameter"))
            .map(|p| children(p).into_iter().any(|m| m.kind() == "modifier" && text(m, src) == "this")).unwrap_or(false),
        (Lang::Kotlin, "function_declaration" | "property_declaration") => {
            // A receiver is a type written before the name (functions) or before the variable (properties).
            let stop = n.child_by_field_name("name").or_else(|| children(n).into_iter().find(|c| c.kind() == "variable_declaration"));
            let Some(stop) = stop else { return false };
            children(n).into_iter().take_while(|c| c.start_byte() < stop.start_byte())
                .any(|c| matches!(c.kind(), "user_type" | "nullable_type" | "parenthesized_type" | "function_type"))
        }
        _ => false,
    }
}

/// Kotlin functions: `<T> Receiver.(params) -> return type`, in the shape `severity` compares.
fn kotlin_sig(n: Node, src: &[u8]) -> String {
    let norm = |t: &str| t.split_whitespace().collect::<Vec<_>>().join(" ");
    let c = children(n);
    let Some(pi) = c.iter().position(|x| x.kind() == "function_value_parameters") else { return String::new() };
    let head: String = c[..pi].iter().filter(|x| matches!(x.kind(), "type_parameters" | "user_type" | "nullable_type" | "parenthesized_type" | "function_type"))
        .map(|x| format!("{}.", norm(text(*x, src)))).collect();
    let ret = c[pi + 1..].iter().find(|x| !matches!(x.kind(), "function_body" | "type_constraints") && !is_trivia(x.kind())).map(|r| norm(text(*r, src))).unwrap_or_default();
    format!("{head}{} -> {ret}", norm(text(c[pi], src)))
}

fn unwrap_decl<'t>(n: Node<'t>, lang: Lang) -> Node<'t> {
    let mut x = n;
    if lang == Lang::Python && x.kind() == "decorated_definition" { if let Some(d) = x.child_by_field_name("definition") { x = d; } }
    if lang != Lang::Python && x.kind() == "export_statement" { if let Some(d) = x.child_by_field_name("declaration") { x = d; } }
    x
}

fn find_first<'t>(n: Node<'t>, kinds: &[&str]) -> Option<Node<'t>> {
    let mut stack = vec![n];
    while let Some(x) = stack.pop() { if kinds.contains(&x.kind()) { return Some(x); } let mut c = children(x); c.reverse(); stack.extend(c); }
    None
}

fn find_kind<'t>(n: Node<'t>, kinds: &[&str]) -> Option<Node<'t>> {
    let mut stack = vec![n];
    while let Some(x) = stack.pop() { if kinds.contains(&x.kind()) { return Some(x); } stack.extend(children(x)); }
    None
}

/// Signature and named parts used to judge how severe a change is.
fn interface_of(n: Node, src: &[u8], lang: Lang) -> (Option<String>, Option<BTreeMap<String, String>>) {
    let n = unwrap_decl(n, lang);
    let norm = |t: &str| t.split_whitespace().collect::<Vec<_>>().join(" ");
    match n.kind() {
        "function_declaration" | "secondary_constructor" if lang == Lang::Kotlin => (Some(kotlin_sig(n, src)), None),
        "function_definition" | "function_declaration" | "generator_function_declaration" | "method_definition" | "function_signature" | "method_signature" => {
            let params = n.child_by_field_name("parameters").map(|p| norm(text(p, src))).unwrap_or_default();
            let ret = n.child_by_field_name("return_type").or_else(|| n.child_by_field_name("result")).map(|r| norm(text(r, src))).unwrap_or_default();
            (Some(format!("{params} -> {ret}")), None)
        }
        "method_declaration" | "function_item" | "function_signature_item" | "constructor_declaration" | "local_function_statement" | "operator_declaration" if is_new_lang(lang) => {
            let params = n.child_by_field_name("parameters").map(|p| norm(text(p, src))).unwrap_or_default();
            let tp = n.child_by_field_name("type_parameters").map(|p| norm(text(p, src))).unwrap_or_default();
            let ret = ["return_type", "result", "type", "returns"].iter().find_map(|f| n.child_by_field_name(f)).map(|r| norm(text(r, src))).unwrap_or_default();
            (Some(format!("{tp}{params} -> {ret}")), None)
        }
        "interface_declaration" | "type_alias_declaration" | "enum_declaration" | "lexical_declaration" | "variable_declaration" | "expression_statement" => {
            let body = find_kind(n, &["object_type", "interface_body", "enum_body", "object", "dictionary", "enum_member_declaration_list"]);
            let Some(body) = body else { return (None, None) };
            let mut parts = BTreeMap::new();
            for c in children(body) {
                let name = c.child_by_field_name("name").or_else(|| c.child_by_field_name("key")).or_else(|| children(c).into_iter().next());
                let Some(name) = name else { continue };
                let mut nm = text(name, src).trim_matches(|ch| ch == '"' || ch == '\'').to_string();
                if c.kind() == "property_signature" && text(c, src).contains("?:") { nm.push('?'); }
                parts.insert(nm, norm(text(c, src)));
            }
            (None, Some(parts))
        }
        _ => (None, None),
    }
}

pub(crate) fn returns_of(n: Node, src: &[u8], lang: Lang) -> String {
    let mut out = vec![];
    if lang == Lang::Rust {
        // Implicit return: the tail expression of the function body.
        if let Some(body) = n.child_by_field_name("body") {
            if let Some(last) = children(body).into_iter().filter(|c| !is_trivia(c.kind())).last() {
                let k = last.kind();
                if !(k.ends_with("_statement") || k.ends_with("_item") || k == "let_declaration" || k == "empty_statement" || k.ends_with("_declaration")) {
                    out.push(format!("tail {}", text(last, src).split_whitespace().collect::<Vec<_>>().join(" ")));
                }
            }
        }
    }
    // Expression bodies (C# `=> x * 2`, Kotlin `= x * 2`) hand back their expression.
    let expr_body = match lang {
        Lang::CSharp => n.child_by_field_name("body").filter(|b| b.kind() == "arrow_expression_clause"),
        Lang::Kotlin => children(n).into_iter().find(|c| c.kind() == "function_body").filter(|b| !children(*b).iter().any(|x| x.kind() == "block")),
        _ => None,
    };
    if let Some(b) = expr_body { out.push(format!("expr {}", text(b, src).trim_start_matches(['=', '>', ' ']).split_whitespace().collect::<Vec<_>>().join(" "))); }
    // One walk: return statements, the names they hand back, and local definitions (checked afterwards).
    let mut names: BTreeSet<String> = BTreeSet::new();
    let mut defs: Vec<(Node, Node)> = vec![];
    walk(n, |x| {
        let k = x.kind();
        if k == "return_statement" || k == "yield" || k == "return_expression" {
            let t = text(x, src).split_whitespace().collect::<Vec<_>>().join(" ");
            // A bare `return` hands back nothing, exactly like falling off the end: not a change in what callers get.
            if !matches!(t.as_str(), "return" | "return;") { out.push(t); }
            if k != "yield" {
                walk(x, |y| { if matches!(y.kind(), "identifier" | "shorthand_property_identifier") { names.insert(text(y, src).to_string()); } true });
            }
            return false;
        }
        let nm = match k {
            "variable_declarator" | "let_declaration" | "short_var_declaration" => x.child_by_field_name("name").or_else(|| x.child_by_field_name("pattern")).or_else(|| x.child_by_field_name("left")),
            "assignment" => x.child_by_field_name("left"),
            "property_declaration" if lang == Lang::Kotlin => children(x).into_iter().find(|c| c.kind() == "variable_declaration")
                .and_then(|v| children(v).into_iter().find(|c| c.kind() == "identifier")),
            _ => None,
        };
        if let Some(nm) = nm { defs.push((nm, x)); }
        true
    });
    // What a function hands back also depends on the local values it returns: `return { total }` is a
    // different result when `const total = ...` changed. Include those definitions (one level).
    for (nm, whole) in defs { if names.contains(text(nm, src)) { out.push(format!("def {}", text(whole, src).split_whitespace().collect::<Vec<_>>().join(" "))); } }
    out.sort(); out.join("\n")
}

/// How severe a change from `old` to `new` (None = removed) is for code that uses the symbol.
fn severity(old: &Item, new: Option<&Item>, lang: Option<Lang>) -> &'static str {
    if matches!(lang, Some(Lang::C | Lang::Cpp)) { return c_cpp::severity(old, new); }
    let Some(new) = new else { return "high" };
    if let (Some(a), Some(b)) = (&old.sig, &new.sig) {
        if a == b {
            // Same signature: still high when what the function returns changed (units, shape, semantics).
            return if old.returns != new.returns { "high" } else { "low" };
        }
        let (pa, pb) = (a.split(" -> ").next().unwrap_or(""), b.split(" -> ").next().unwrap_or(""));
        let (ra, rb) = (a.split(" -> ").nth(1).unwrap_or(""), b.split(" -> ").nth(1).unwrap_or(""));
        let core = |p: &str| p.trim_start_matches('(').trim_end_matches(')').to_string();
        let (ca, cb) = (core(pa), core(pb));
        if ra == rb && cb.starts_with(ca.trim_end_matches(',')) {
            let added = cb[ca.trim_end_matches(',').len()..].to_string();
            if added.split(',').filter(|x| !x.trim().is_empty()).all(|x| x.contains('?') || x.contains('=')) { return "low"; }  // optional params appended
        }
        return "high";
    }
    if let (Some(a), Some(b)) = (&old.parts, &new.parts) {
        for (k, v) in a { if b.get(k) != Some(v) { return "high"; } }  // existing part removed or changed
        for k in b.keys() { if !a.contains_key(k) && !k.ends_with('?') && old.key_is_type() { return "high"; } }  // new required field in a type
        return "low";                                                  // only additions
    }
    if matches!(old.key, Key::Assign(_)) { return "high"; }            // constant / value changed
    // A class constant, field or property of the module-scoped languages too (C# keeps every constant in a class).
    if lang.map(is_new_lang).unwrap_or(false) && matches!(&old.key, Key::Member(_, k) if matches!(**k, Key::Assign(_))) { return "high"; }
    "low"
}

impl Item {
    fn key_is_type(&self) -> bool { self.text.contains("interface ") || self.text.contains("type ") }
}

fn items_of<'t>(container: Node<'t>, src: &[u8], lang: Lang, start: usize, end: usize) -> Vec<(Key, Item, Node<'t>)> {
    let mut out = vec![];
    let (mut cursor, mut covered, mut first) = (start, start, true);
    for child in children(container) {
        if child.start_byte() < covered || is_trivia(child.kind()) { continue; }
        let key = key_of(child, src, lang, first); first = false;
        let stop = line_end(src, child.end_byte()).min(end);
        let mut refs = Refs::default();
        collect_refs(child, src, lang, &mut refs);
        let (sig, parts) = interface_of(child, src, lang);
        let returns = sig.as_ref().map(|_| returns_of(child, src, lang));
        let ext = is_extension(child, src, lang);
        let item = Item { key: key.clone(), text: String::from_utf8_lossy(&src[cursor..stop]).into_owned(), refs, sig, parts, returns, ext };
        out.push((key, item, child));
        cursor = stop; covered = stop;
    }
    out
}

fn class_body<'t>(n: Node<'t>, lang: Lang) -> Option<Node<'t>> {
    let mut x = n;
    if lang == Lang::Python && x.kind() == "decorated_definition" { x = x.child_by_field_name("definition")?; }
    if lang != Lang::Python && x.kind() == "export_statement" { x = x.child_by_field_name("declaration")?; }
    body_node(x, lang, "body")
}

/// Parse a file into keyed items (top level + class members), import aliases and instance attributes.
/// Returns None when the file does not parse.
pub fn analyze_file(path: &str, src: &[u8]) -> Option<FileInfo> {
    // The same file content is analysed for several agent pairs and merge checks in one run: parse it once.
    use std::collections::HashMap;
    use std::hash::{Hash, Hasher};
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<(String, u64, usize), Option<FileInfo>>>> = OnceLock::new();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut h);
    // How a C or C++ header is read depends on the include settings.
    if c_cpp::lang_of(path).is_some() { c_cpp::generation().hash(&mut h); }
    let key = (path.to_string(), h.finish(), src.len());
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if std::env::var_os("GITTA_NO_PARSE_CACHE").is_some() { return analyze_file_uncached(path, src); }
    if let Some(v) = cache.lock().unwrap().get(&key) { return v.clone(); }
    let v = match disk_cache_file(&key) {
        // A large file is read once per content, not once per process: hooks run as separate processes.
        Some(f) => match std::fs::read(&f).ok().and_then(|b| serde_json::from_slice::<Option<Stored>>(&b).ok()) {
            Some(st) => st.map(Stored::into_info),
            None => {
                let v = analyze_file_uncached(path, src);
                disk_cache_write(&f, &v.as_ref().map(Stored::from_info));
                v
            }
        },
        None => analyze_file_uncached(path, src),
    };
    let mut c = cache.lock().unwrap();
    if c.len() > 32768 { c.clear(); }
    c.insert(key, v.clone());
    v
}

/// Where large files' analyses are kept between processes (set by the CLI: `.gitta/cache/parse`).
static PARSE_DIR: std::sync::RwLock<Option<std::path::PathBuf>> = std::sync::RwLock::new(None);
/// Files smaller than this parse faster than their cached analysis loads.
const DISK_CACHE_MIN: usize = 32 * 1024;

pub fn set_parse_cache_dir(dir: Option<std::path::PathBuf>) { if let Ok(mut d) = PARSE_DIR.write() { *d = dir; } }

fn disk_cache_file(key: &(String, u64, usize)) -> Option<std::path::PathBuf> {
    use std::hash::{Hash, Hasher};
    if key.2 < DISK_CACHE_MIN { return None; }
    let dir = PARSE_DIR.read().ok()?.clone()?;
    // The binary's own build is part of the key: a rebuilt reader never trusts an older reader's results.
    static BUILD: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let build = *BUILD.get_or_init(|| {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        env!("CARGO_PKG_VERSION").hash(&mut h);
        if let Ok(m) = std::env::current_exe().and_then(std::fs::metadata) { (m.len(), m.modified().ok()).hash(&mut h); }
        h.finish()
    });
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (key, build).hash(&mut h);
    Some(dir.join(format!("{:016x}.json", h.finish())))
}

fn disk_cache_write(f: &std::path::Path, v: &Option<Stored>) {
    let Some(dir) = f.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() { return; }
    // Bounded: a full cache starts over (old contents are never needed again once their files changed).
    if std::fs::read_dir(dir).map(|d| d.count() > 2000).unwrap_or(false) { let _ = std::fs::remove_dir_all(dir); let _ = std::fs::create_dir_all(dir); }
    let tmp = f.with_extension(format!("tmp{}", std::process::id()));
    if serde_json::to_vec(v).ok().map(|b| std::fs::write(&tmp, b).is_ok()).unwrap_or(false) { let _ = std::fs::rename(&tmp, f); }
}

/// `FileInfo` as stored on disk (JSON maps need text keys, so keyed maps are lists).
#[derive(Serialize, serde::Deserialize)]
struct Stored {
    items: Vec<(Key, Item)>, aliases: BTreeMap<String, (String, Option<String>)>, instance_attrs: BTreeSet<String>, attr_init: BTreeMap<String, String>,
    lang: Option<Lang>, module: String, opens: BTreeSet<String>, symbols: Vec<(Key, (String, String))>,
}

impl Stored {
    fn from_info(i: &FileInfo) -> Stored {
        Stored { items: i.items.iter().map(|(k, v)| (k.clone(), v.clone())).collect(), aliases: i.aliases.clone(), instance_attrs: i.instance_attrs.clone(), attr_init: i.attr_init.clone(),
                 lang: i.lang, module: i.module.clone(), opens: i.opens.clone(), symbols: i.symbols.iter().map(|(k, v)| (k.clone(), v.clone())).collect() }
    }
    fn into_info(self) -> FileInfo {
        FileInfo { items: self.items.into_iter().collect(), aliases: self.aliases, instance_attrs: self.instance_attrs,
                   attr_init: self.attr_init, lang: self.lang, module: self.module, opens: self.opens, symbols: self.symbols.into_iter().collect() }
    }
}

fn analyze_file_uncached(path: &str, src: &[u8]) -> Option<FileInfo> {
    let lang = lang_of(path)?;
    if matches!(lang, Lang::C | Lang::Cpp) { return c_cpp::analyze(path, src, lang); }
    let tree: Tree = parser(lang).parse(src, None)?;
    let root = tree.root_node();
    if root.has_error() { return None; }
    let mut info = FileInfo { lang: Some(lang), module: module_name(path), ..FileInfo::default() };
    if matches!(lang, Lang::CSharp | Lang::Kotlin) {
        info.module = declared_module(root, src, lang);
        // C# code sees the names of every enclosing namespace without a using directive.
        let parts: Vec<&str> = info.module.split('.').collect();
        for i in 1..parts.len() { info.opens.insert(parts[..i].join(".")); }
    }
    let items = items_of(root, src, lang, 0, src.len());
    add_items(&mut info, path, src, lang, items, "", "");
    instance_attrs(root, src, lang, &mut info.instance_attrs);
    attr_inits(root, src, lang, &mut info.attr_init);
    Some(info)
}

/// Properties declared in a primary constructor: Kotlin `class Line(val qty: Int)`, C# `record R(int A)`.
fn primary_properties(n: Node, src: &[u8], lang: Lang, cname: &str) -> Vec<(Key, Item)> {
    let params: Vec<Node> = match (lang, n.kind()) {
        (Lang::Kotlin, "class_declaration") => children(n).into_iter().filter(|c| c.kind() == "primary_constructor")
            .flat_map(children).filter(|c| c.kind() == "class_parameters").flat_map(children)
            .filter(|p| p.kind() == "class_parameter" && { let t = text(*p, src); t.contains("val ") || t.contains("var ") }).collect(),
        (Lang::CSharp, "record_declaration" | "record_struct_declaration") => children(n).into_iter().filter(|c| c.kind() == "parameter_list")
            .flat_map(children).filter(|p| p.kind() == "parameter").collect(),
        _ => vec![],
    };
    params.into_iter().filter_map(|p| {
        let nm = p.child_by_field_name("name").or_else(|| children(p).into_iter().find(|c| c.kind() == "identifier"))?;
        let k = Key::Member(cname.to_string(), Box::new(Key::Assign(text(nm, src).into())));
        let mut refs = Refs::default();
        collect_refs(p, src, lang, &mut refs);
        Some((k.clone(), Item { key: k, text: text(p, src).into(), refs, sig: None, parts: None, returns: None, ext: false }))
    }).collect()
}

/// Enum entries written on one line (`RED, GREEN`): items own whole lines, so all but the first need their own key.
fn same_line_entries(body: Node, src: &[u8], lang: Lang, cname: &str) -> Vec<(Key, Item)> {
    children(body).into_iter().filter(|c| matches!(c.kind(), "enum_entry" | "enum_member_declaration" | "enum_constant")).map(|c| {
        let k = Key::Member(cname.to_string(), Box::new(key_of(c, src, lang, false)));
        let mut refs = Refs::default();
        collect_refs(c, src, lang, &mut refs);
        (k.clone(), Item { key: k, text: text(c, src).into(), refs, sig: None, parts: None, returns: None, ext: false })
    }).collect()
}

/// Add keyed items of one container (the file, or a C# namespace block) with class members and imports.
/// `ns` is the container's full namespace, `prefix` that namespace relative to the file's own.
fn add_items<'t>(info: &mut FileInfo, path: &str, src: &[u8], lang: Lang, items: Vec<(Key, Item, Node<'t>)>, ns: &str, prefix: &str) {
    for (key, item, node) in items {
        if lang == Lang::CSharp && node.kind() == "namespace_declaration" {
            // Block namespace: its declarations are the file's items. Nested or further namespaces prefix their
            // type names relative to the file's namespace, so `Shop.Pricing.Inner.Q` stays `Shop.Pricing.Inner.Q`.
            let nm: String = node.child_by_field_name("name").map(|x| text(x, src).chars().filter(|c| !c.is_whitespace()).collect()).unwrap_or_default();
            let full = if ns.is_empty() { nm } else { format!("{ns}.{nm}") };
            let rel = if full == info.module { String::new() } else { full.strip_prefix(&format!("{}.", info.module)).unwrap_or(&full).to_string() };
            if let Some(body) = node.child_by_field_name("body") {
                let bstart = src[..body.start_byte()].iter().rposition(|&b| b == b'\n').map(|i| i + 1).unwrap_or(0);
                let inner = items_of(body, src, lang, bstart, line_end(src, body.end_byte()));
                add_items(info, path, src, lang, inner, &full, &rel);
            }
            continue;
        }
        let (key, item) = match &key {
            Key::Class(c) if !prefix.is_empty() => { let k = Key::Class(format!("{prefix}.{c}")); (k.clone(), Item { key: k, ..item }) }
            _ => (key, item),
        };
        if let Key::Class(cname) = &key {
            let cname = if node.kind() == "impl_item" {
                node.child_by_field_name("type").map(|t| base_type_name(t, src)).unwrap_or_else(|| cname.clone())
            } else { cname.clone() };
            let cname = &cname;
            for (k, it) in primary_properties(node, src, lang, cname) { info.items.insert(k, it); }
            if let Some(body) = class_body(node, lang) {
                let bstart = src[..body.start_byte()].iter().rposition(|&b| b == b'\n').map(|i| i + 1).unwrap_or(0);
                for (mk, mi, mn) in items_of(body, src, lang, bstart.max(body.start_byte().min(bstart)), line_end(src, body.end_byte())) {
                    // Kotlin companion object members are called on the class: `Cart.make()`.
                    if lang == Lang::Kotlin && mn.kind() == "companion_object" {
                        if let Some(cb) = body_node(mn, lang, "body") {
                            let cstart = src[..cb.start_byte()].iter().rposition(|&b| b == b'\n').map(|i| i + 1).unwrap_or(0);
                            for (ck, ci, _) in items_of(cb, src, lang, cstart.max(cb.start_byte().min(cstart)), line_end(src, cb.end_byte())) {
                                let k = Key::Member(cname.clone(), Box::new(ck));
                                info.items.insert(k.clone(), Item { key: k, ..ci });
                            }
                        }
                        continue;
                    }
                    let k = Key::Member(cname.clone(), Box::new(mk));
                    info.items.insert(k.clone(), Item { key: k, ..mi });
                }
                for (k, it) in same_line_entries(body, src, lang, cname) { info.items.entry(k).or_insert(it); }
            }
        }
        // imports -> aliases
        match (lang, node.kind()) {
            (Lang::Python, "import_from_statement") => {
                let module = node.child_by_field_name("module_name").map(|m| text(m, src).to_string()).unwrap_or_default();
                let module = resolve_py(path, &module);
                let mut c = node.walk();
                for n in node.children_by_field_name("name", &mut c) {
                    let nm = n.child_by_field_name("name").unwrap_or(n);
                    let local = n.child_by_field_name("alias").unwrap_or(nm);
                    let name = text(nm, src).rsplit('.').next().unwrap_or("").to_string();
                    info.aliases.insert(text(local, src).rsplit('.').next().unwrap_or("").into(), (module.clone(), Some(name)));
                }
            }
            (Lang::Python, "import_statement") => {
                let mut c = node.walk();
                for n in node.children_by_field_name("name", &mut c) {
                    let full = text(n.child_by_field_name("name").unwrap_or(n), src).to_string();
                    match n.child_by_field_name("alias") {
                        Some(a) => { info.aliases.insert(text(a, src).into(), (full, None)); }
                        None => { let head = full.split('.').next().unwrap_or("").to_string(); info.aliases.insert(head.clone(), (head, None)); }
                    }
                }
            }
            (Lang::TypeScript | Lang::Tsx, "import_statement") => {
                let Some(source) = node.child_by_field_name("source") else { continue_imports(info, &key, item); continue; };
                let module = resolve_ts(path, text(source, src));
                for clause in children(node).into_iter().filter(|c| c.kind() == "import_clause") {
                    for part in children(clause) {
                        match part.kind() {
                            "identifier" => { info.aliases.insert(text(part, src).into(), (module.clone(), Some("default".into()))); }
                            "namespace_import" => {
                                if let Some(id) = children(part).into_iter().find(|c| c.kind() == "identifier") { info.aliases.insert(text(id, src).into(), (module.clone(), None)); }
                            }
                            "named_imports" => {
                                for spec in children(part).into_iter().filter(|c| c.kind() == "import_specifier") {
                                    let nm = spec.child_by_field_name("name").map(|x| text(x, src).to_string()).unwrap_or_default();
                                    let local = spec.child_by_field_name("alias").map(|x| text(x, src).to_string()).unwrap_or(nm.clone());
                                    info.aliases.insert(local, (module.clone(), Some(nm)));
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            (Lang::Go, "import_declaration") => {
                let mut stack = vec![node];
                while let Some(x) = stack.pop() {
                    if x.kind() == "import_spec" {
                        let Some(p) = x.child_by_field_name("path") else { continue };
                        let ipath = text(p, src).trim_matches(|c| c == '"' || c == '`').to_string();
                        let local = match x.child_by_field_name("name") { Some(nm) => text(nm, src).to_string(),
                            None => ipath.rsplit('/').next().unwrap_or("").to_string() };
                        if local != "_" && local != "." { info.aliases.insert(local, (format!("go:{ipath}"), None)); }
                        continue;
                    }
                    stack.extend(children(x));
                }
            }
            (Lang::Rust, "use_declaration") => {
                if let Some(arg) = node.child_by_field_name("argument") { rust_use(arg, src, &[], &info.module.clone(), &mut info.aliases); }
            }
            (Lang::Java, "import_declaration") => {
                let t = text(node, src).trim().trim_start_matches("import").trim().trim_end_matches(';').trim().to_string();
                let t = t.strip_prefix("static ").map(str::trim).unwrap_or(&t).to_string();
                if !t.ends_with('*') {
                    if let Some((m, n)) = t.rsplit_once('.') { info.aliases.insert(n.to_string(), (m.to_string(), Some(n.to_string()))); }
                }
            }
            (Lang::CSharp, "using_directive") => {
                // `using N;` opens a namespace, `using static N.T;` a type's members, `using A = N.T;` names a type.
                let target = children(node).into_iter().rfind(|c| !is_trivia(c.kind()));
                let target: String = target.map(|t| text(t, src).chars().filter(|c| !c.is_whitespace()).collect()).unwrap_or_default();
                match node.child_by_field_name("name") {
                    Some(a) => if let Some((m, n)) = target.rsplit_once('.') {
                        info.aliases.insert(text(a, src).into(), (m.to_string(), Some(n.split('<').next().unwrap_or(n).to_string())));
                    } else { info.aliases.insert(text(a, src).into(), (String::new(), Some(target))); },
                    None => { info.opens.insert(target); }
                }
            }
            (Lang::Kotlin, "import") => {
                let Some(q) = children(node).into_iter().find(|c| c.kind() == "qualified_identifier" || c.kind() == "identifier") else { continue };
                let full: String = text(q, src).chars().filter(|c| !c.is_whitespace()).collect();
                if text(node, src).trim_end().ends_with('*') { info.opens.insert(full); }
                else {
                    let alias = children(node).into_iter().rfind(|c| c.kind() == "identifier" && c.start_byte() > q.end_byte());
                    let (m, n) = full.rsplit_once('.').map(|(m, n)| (m.to_string(), n.to_string())).unwrap_or((String::new(), full.clone()));
                    let local = alias.map(|a| text(a, src).to_string()).unwrap_or(n.clone());
                    info.aliases.insert(local, (m, Some(n)));
                }
            }
            _ => {}
        }
        info.items.insert(key, item);
    }
}

fn continue_imports(_info: &mut FileInfo, _k: &Key, _i: Item) {}

#[derive(Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Warning {
    pub kind: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")] pub symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub changed_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub referenced_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub severity: Option<String>,
    pub agents: Vec<String>,
    /// SECURITY: how the control was weakened. HAZARD: set when the changed symbol is a security check.
    #[serde(skip_serializing_if = "Option::is_none")] pub security: Option<security::Note>,
}

struct Side {
    changed: BTreeMap<(String, String), &'static str>, // (module or module.Class or ATTR, name) -> severity
    touched: BTreeMap<String, BTreeSet<Key>>, // path -> added/modified keys
    files: BTreeSet<String>,                  // changed non-code files
    unparsed: BTreeSet<String>,
    removed: BTreeMap<String, BTreeSet<Key>>, // path -> keys deleted on this side
    classes: BTreeSet<String>,                // `module.Class` keys in `changed` (member changes)
    weak: BTreeMap<(String, String), security::Note>, // keys of `changed` whose security control was weakened
    secure: BTreeSet<(String, String)>,       // keys of `changed` that are security checks (changed otherwise)
}

/// Both versions of each changed code file, parsed in order of `paths`. Large changes (a branch far behind the
/// other) parse thousands of files, so they are parsed on a few threads.
fn parse_changed(base: &Snapshot, x: &Snapshot, paths: &BTreeSet<&String>) -> Vec<(Option<FileInfo>, Option<FileInfo>)> {
    let todo: Vec<(&String, Option<&Vec<u8>>, Option<&Vec<u8>>)> = paths.iter().map(|p| (*p, base.get(*p), x.get(*p)))
        .filter(|(p, b, v)| b != v && lang_of(p).is_some()).collect();
    let one = |(p, b, v): &(&String, Option<&Vec<u8>>, Option<&Vec<u8>>)| {
        let f = |src: Option<&Vec<u8>>| match src { Some(s) => analyze_file(p, s), None => Some(FileInfo::default()) };
        (f(*b), f(*v))
    };
    let width = std::thread::available_parallelism().map(|n| n.get() / 2).unwrap_or(2).clamp(1, 4);
    if todo.len() < 64 || width < 2 { return todo.iter().map(one).collect(); }
    let size = todo.len().div_ceil(width);
    std::thread::scope(|sc| {
        let hs: Vec<_> = todo.chunks(size).map(|c| sc.spawn(move || c.iter().map(one).collect::<Vec<_>>())).collect();
        hs.into_iter().flat_map(|h| h.join().unwrap()).collect()
    })
}

fn side_changes(base: &Snapshot, x: &Snapshot) -> Side {
    let mut s = Side { changed: BTreeMap::new(), touched: BTreeMap::new(), files: BTreeSet::new(), unparsed: BTreeSet::new(), removed: BTreeMap::new(), classes: BTreeSet::new(), weak: BTreeMap::new(), secure: BTreeSet::new() };
    let paths: BTreeSet<&String> = base.keys().chain(x.keys()).collect();
    let mut parsed = parse_changed(base, x, &paths).into_iter();
    for path in paths {
        let (bv, xv) = (base.get(path), x.get(path));
        if bv == xv { continue; }
        if lang_of(path).is_none() { s.files.insert(path.clone()); continue; }
        let (bi, xi) = parsed.next().expect("one parse per changed code file");
        let (Some(bi), Some(xi)) = (bi, xi) else { s.unparsed.insert(path.clone()); continue; };
        let module = if bv.is_some() { &bi } else { &xi }.module.clone();
        let member_changed: BTreeSet<String> = bi.items.iter().chain(xi.items.iter())
            .filter(|(k, _)| matches!(k, Key::Member(..)) && bi.items.get(*k).map(|i| &i.text) != xi.items.get(*k).map(|i| &i.text))
            .filter_map(|(k, _)| if let Key::Member(c, _) = k { Some(c.clone()) } else { None }).collect();
        let mut put = |key: (String, String), sev: &'static str| {
            let e = s.changed.entry(key).or_insert(sev);
            if sev == "high" { *e = "high"; }
        };
        for (k, item) in &bi.items {
            let new = xi.items.get(k);
            if new.map(|i| &i.text) == Some(&item.text) { continue; }
            let sev = severity(item, new, bi.lang.or(xi.lang));
            if let Some(lang) = bi.lang {
                // Security controls: the same keys as `changed`, so a hit on the symbol finds the note.
                let skey = match k { _ if bi.symbols.contains_key(k) => bi.symbols.get(k).cloned(), Key::Member(c, inner) => inner.bound().map(|n| (format!("{module}.{c}"), n.to_string())), _ => k.bound().map(|n| (module.clone(), n.to_string())) };
                // A key with its own symbol (a nested COBOL program's paragraph) is judged under that symbol.
                let (smod, sk) = match bi.symbols.get(k) { Some((m, n)) => (m.clone(), k.renamed(n)), None => (module.clone(), k.clone()) };
                if let Some(skey) = skey {
                    match security::weakening(path, &smod, &sk, item, new, lang) {
                        Some(note) => {
                            // A weakened member weakens its class for code that uses the class (a view, a controller).
                            if let Key::Member(c, _) = k { s.weak.entry((module.clone(), c.clone())).or_insert_with(|| note.clone()); }
                            s.weak.insert(skey, note);
                        }
                        None => if sev == "high" && security::relevant(path, &smod, &sk, item, lang) { s.secure.insert(skey); },
                    }
                }
            }
            // Extension methods are called as `value.name(...)`: any use of `.name` may reach them.
            if item.ext { if let Some(n) = k.bound().or(if let Key::Member(_, i) = k { i.bound() } else { None }) { put((ATTR.into(), n.to_string()), sev); } }
            if let Some(sym) = bi.symbols.get(k).or_else(|| xi.symbols.get(k)) { put(sym.clone(), sev); continue; }
            match k {
                Key::Member(c, inner) => if let Some(n) = inner.bound() {
                    s.classes.insert(format!("{module}.{c}"));
                    put((format!("{module}.{c}"), n.to_string()), sev);
                    if matches!(**inner, Key::Def(_)) && new.is_none() { put((ATTR.into(), n.to_string()), "high"); }
                },
                Key::Class(c) if member_changed.contains(c) && new.is_some() => {
                    // Users of the class (subclasses, constructors) inherit the worst member change.
                    let worst = bi.items.iter().filter(|(mk, _)| matches!(mk, Key::Member(cc, _) if cc == c))
                        .map(|(mk, mi)| if xi.items.get(mk).map(|i| &i.text) == Some(&mi.text) { "low" } else { severity(mi, xi.items.get(mk), bi.lang) })
                        .fold("low", |acc, x| if x == "high" { "high" } else { acc });
                    put((module.clone(), c.clone()), worst);
                }
                _ => if let Some(n) = k.bound() { put((module.clone(), n.to_string()), sev); },
            }
        }
        for a in bi.instance_attrs.difference(&xi.instance_attrs) { s.changed.insert((ATTR.into(), a.clone()), "high"); }
        for (a, v) in &bi.attr_init { if xi.attr_init.get(a).map(|w| w != v).unwrap_or(false) { s.changed.insert((ATTR.into(), a.clone()), "high"); } }
        s.removed.insert(path.clone(), bi.items.keys().filter(|k| !xi.items.contains_key(*k)).cloned().collect());
        let diff: BTreeSet<Key> = xi.items.iter().filter(|(k, i)| bi.items.get(*k).map(|b| &b.text) != Some(&i.text)).map(|(k, _)| k.clone()).collect();
        let member_classes: BTreeSet<String> = diff.iter().filter_map(|k| if let Key::Member(c, _) = k { Some(c.clone()) } else { None }).collect();
        s.touched.insert(path.clone(), diff.into_iter().filter(|k| !matches!(k, Key::Class(c) if member_classes.contains(c))).collect());
    }
    c_cpp::receivers(x, &mut s.changed);
    s
}

/// Bare identifiers and qualified (module, name) references made by the given keys of a file.
fn references(info: &FileInfo, keys: &BTreeSet<Key>) -> (BTreeSet<String>, BTreeSet<(String, String)>) {
    let (mut bare, mut qual) = (BTreeSet::new(), BTreeSet::new());
    let mut scoped: BTreeSet<String> = BTreeSet::new();   // bare names of keys with their own symbol scope
    for k in keys {
        let Some(item) = info.items.get(k) else { continue };
        // C and C++ keys all have their own symbol, but their bare names are names of module `c` (see c_cpp.rs).
        let c = matches!(info.lang, Some(Lang::C | Lang::Cpp));
        if info.symbols.contains_key(k) && !c { scoped.extend(item.refs.idents.iter().cloned()); } else { bare.extend(item.refs.idents.iter().cloned()); }
        qual.extend(item.refs.qual.iter().cloned());
        for p in &item.refs.props { qual.insert((ATTR.to_string(), p.clone())); }
        for (obj, prop) in &item.refs.members {
            if let Some((m, None)) = info.aliases.get(obj) { qual.insert((m.clone(), prop.clone())); }
            if let Some((m, Some(n))) = info.aliases.get(obj) { qual.insert((format!("{m}.{n}"), prop.clone())); }
            if info.lang.map(is_new_lang).unwrap_or(false) && !info.aliases.contains_key(obj) {
                qual.insert((format!("{}.{obj}", info.module), prop.clone()));  // same package/module type
                for m in &info.opens { qual.insert((format!("{m}.{obj}"), prop.clone())); }   // type from an opened namespace
            }
        }
        for (raw, name) in &item.refs.paths {
            let segs: Vec<&str> = raw.split("::").filter(|s| !s.is_empty()).collect();
            let mut full: Vec<String> = match segs.first().and_then(|f| info.aliases.get(*f)) {
                Some((m, Some(n))) => { let mut v: Vec<String> = m.split("::").filter(|s| !s.is_empty()).map(String::from).collect(); v.push(n.clone()); v.extend(segs[1..].iter().map(|s| s.to_string())); v }
                _ => rust_resolve(&info.module, &segs),
            };
            if full.is_empty() { full = vec![]; }
            qual.insert((full.join("::"), name.clone()));
            if let Some((last, m)) = full.split_last() { qual.insert((format!("{}.{last}", m.join("::")), name.clone())); }
        }
    }
    for (local, (m, n)) in &info.aliases {
        if let Some(n) = n { if bare.contains(local) || scoped.contains(local) { qual.insert((m.clone(), n.clone())); } }
    }
    // A bare name may come from any opened namespace (`using Shop.Pricing;`, `import shop.pricing.*`).
    for m in &info.opens { for b in bare.iter().chain(&scoped) { qual.insert((m.clone(), b.clone())); } }
    (bare, qual)
}

/// The plain type name a signature returns (`-> : Order`, `-> Order`), if it is a named type.
fn returned_type(sig: &str) -> Option<String> {
    let r = sig.rsplit(" -> ").next()?.trim().trim_start_matches(':').trim().trim_start_matches("->").trim();
    let ok = !r.is_empty() && r.chars().next().map(|c| c.is_alphabetic() || c == '_').unwrap_or(false) && r.chars().all(|c| c.is_alphanumeric() || c == '_');
    let builtin = ["number", "string", "boolean", "void", "any", "unknown", "never", "None", "int", "str", "float", "bool", "list", "dict", "bytes", "object", "undefined", "null"];
    (ok && !builtin.contains(&r)).then(|| r.to_string())
}

/// Transitive effect: an unchanged function that uses a symbol whose meaning changed (high) now behaves
/// differently too, so it counts as changed (high) for code that calls it. Follows calls up to three hops
/// within the side's own snapshot.
fn propagate(x: &Snapshot, side: &mut Side) {
    // Parse lazily: a file can only use a changed symbol if its text contains the symbol's name, so most
    // files of a large repository are never parsed.
    let mut parsed: BTreeMap<String, Option<FileInfo>> = BTreeMap::new();
    for _ in 0..3 {
        let high: Vec<(String, String)> = side.changed.iter().filter(|(_, s)| **s == "high").map(|(k, _)| k.clone()).collect();
        // The changed names, plus the class for class members (a module's own name would match far too much).
        let names: BTreeSet<&str> = high.iter().flat_map(|(m, n)| [Some(text_name(m, n)), side.classes.contains(m).then(|| m.rsplit('.').next().unwrap_or(m))]).flatten().collect();
        // One prepared substring search per name: the text of every file is searched for every name.
        let finders: Vec<(&str, memchr::memmem::Finder)> = names.iter().filter(|n| !n.is_empty()).map(|n| (*n, memchr::memmem::Finder::new(n.as_bytes()))).collect();
        let todo: Vec<(&String, &Vec<u8>)> = x.iter().filter(|(p, _)| lang_of(p).is_some() && !parsed.contains_key(*p))
            .filter(|(p, v)| { let ci = caseless(p); finders.iter().any(|(n, f)| if ci { v.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n.as_bytes())) } else { f.find(v).is_some() }) }).collect();
        for (p, v) in todo { parsed.insert(p.clone(), analyze_file(p, v)); }
        let infos: Vec<(&String, &FileInfo)> = parsed.iter().filter_map(|(p, i)| i.as_ref().map(|i| (p, i))).collect();
        let mut added = false;
        // A function whose result changed changes what its declared return type holds (units, shape, meaning).
        for (_, info) in infos.iter().copied() {
            let module = info.module.clone();
            for (k, item) in &info.items {
                let Key::Def(n) = k else { continue };
                if info.symbols.contains_key(k) || !high.contains(&(module.clone(), n.clone())) { continue; }
                let Some(t) = item.sig.as_deref().and_then(returned_type) else { continue };
                let target = match info.aliases.get(&t) { Some((m, Some(nm))) => (m.clone(), nm.clone()), _ => (module.clone(), t.clone()) };
                if side.changed.get(&target) != Some(&"high") { side.changed.insert(target, "high"); added = true; }
            }
        }
        for (_, info) in infos.iter().copied() {
            let module = info.module.clone();
            for (k, item) in &info.items {
                if item.sig.is_none() { continue; }   // functions and methods only
                // C and C++ declarations carry a signature too: only definitions (and macros) have a body to follow.
                if matches!(info.lang, Some(Lang::C | Lang::Cpp)) && item.returns.is_none() { continue; }
                let target = match k {
                    _ if info.symbols.contains_key(k) => info.symbols[k].clone(),
                    Key::Def(n) => (module.clone(), n.clone()),
                    Key::Member(c, inner) => match inner.bound() { Some(n) => (format!("{module}.{c}"), n.to_string()), None => continue },
                    _ => continue,
                };
                if side.changed.get(&target) == Some(&"high") { continue; }
                let (bare, qual) = references(info, &BTreeSet::from([k.clone()]));
                let uses = high.iter().any(|(cm, name)| *name != target.1 && ((cm == &module && bare.contains(name)) || qual.contains(&(cm.clone(), name.clone()))
                    || (cm == ATTR && matches!(k, Key::Member(..)) && item.refs.props.contains(name))));
                if uses { side.changed.insert(target, "high"); added = true; }
            }
        }
        if !added { break; }
    }
}

fn caseless(_: &str) -> bool { false }

/// The part of a changed symbol's name that the text of code using it contains (C++ `ns::W::f` is called as `f`).
fn text_name<'a>(m: &str, n: &'a str) -> &'a str { if c_cpp::display(m, n).is_some() { c_cpp::search_name(n) } else { n } }

/// Modules a file imports (as `module_name` spells them; Go packages as `go:<import path>`; C and C++ includes as
/// `cinc:` candidates, see `c_cpp::resolve_include`).
pub fn imports_of(path: &str, src: &[u8]) -> BTreeSet<String> {
    // A C or C++ file's own module (`c:<path>`) is in its `opens` but is not something it imports.
    analyze_file(path, src).map(|i| i.aliases.into_values().map(|(m, _)| m).chain(i.opens.into_iter().filter(|m| !m.starts_with("c:"))).collect()).unwrap_or_default()
}

/// Names whose meaning one side changed (directly or through helpers in `x`). A file that mentions none of
/// them cannot be affected, so callers can load only the files that do (for example with `git grep`).
pub fn high_names(base: &Snapshot, x: &Snapshot) -> BTreeSet<String> {
    let mut side = side_changes(base, x);
    propagate(x, &mut side);
    side.changed.iter().filter(|(_, s)| **s == "high")
        .flat_map(|((m, n), _)| [Some(text_name(m, n).to_string()), side.classes.contains(m).then(|| m.rsplit('.').next().unwrap_or(m).to_string())]).flatten()
        .filter(|n| n != ATTR).collect()
}

/// An OVERLAP that only informs once the file merges cleanly (`merged` is the merged file): the merge holds both
/// edits and neither can change what other code means. That is a non-code file (docs, changelogs, manifests, CI
/// files), a symbol in a test file (a clash there fails that test, and no other code calls it), or an import line
/// the merged file holds once (the same line added in two places declares the name twice, so that stays a warning).
/// Symbols in other code stay warnings: two clean edits of one function or object can still clash, for example
/// as a duplicate object key.
pub fn quiet_overlap(w: &Warning, merged: &[u8]) -> bool {
    if w.kind != "OVERLAP" { return false; }
    let Some(sym) = w.symbol.as_deref().filter(|s| !s.is_empty()) else { return lang_of(&w.path).is_none() };
    if guard::is_test(&w.path) { return true; }
    match sym.strip_prefix("import ").map(str::trim) {
        Some(line) if !line.is_empty() => String::from_utf8_lossy(merged).matches(line).count() <= 1,
        _ => false,
    }
}

/// Warnings for the current states of two agents relative to base.
pub fn analyze(base: &Snapshot, a: (&str, &Snapshot), b: (&str, &Snapshot)) -> Vec<Warning> { analyze_with(base, a, b, false) }

/// High-severity hazards between two sides, following changed meaning through helper functions.
/// A merge that has any of these is semantically unsafe even when the text combines cleanly.
pub fn merge_hazards(base: &Snapshot, head: &Snapshot, sub: &Snapshot) -> Vec<Warning> {
    // A file both sides hold with the same text (the submitter pulled in the other's work with `sync --from`, or
    // both made the same edit) changed nothing under either side: each side's checks already ran against it.
    // Counting it would report a side's own copy of the change as a hazard against itself.
    let (mut head, mut sub) = (head.clone(), sub.clone());
    let same: Vec<String> = head.iter().filter(|(p, v)| sub.get(*p) == Some(*v) && base.get(*p) != Some(*v)).map(|(p, _)| p.clone()).collect();
    for p in same {
        match base.get(&p) {
            Some(b) => { head.insert(p.clone(), b.clone()); sub.insert(p, b.clone()); }
            None => { head.remove(&p); sub.remove(&p); }
        }
    }
    analyze_with(base, ("head", &head), ("submitted", &sub), true).into_iter()
        .filter(|w| w.kind == "UNPARSED" || w.kind == "SECURITY" || (w.kind == "HAZARD" && w.severity.as_deref() == Some("high"))).collect()
}

fn analyze_with(base: &Snapshot, a: (&str, &Snapshot), b: (&str, &Snapshot), transitive: bool) -> Vec<Warning> {
    let (an, asnap) = a; let (bn, bsnap) = b;
    let mut sa = side_changes(base, asnap); let mut sb = side_changes(base, bsnap);
    if transitive { propagate(asnap, &mut sa); propagate(bsnap, &mut sb); }
    let mut out = vec![];
    for (agent, side) in [(an, &sa), (bn, &sb)] {
        for p in &side.unparsed { out.push(Warning { kind: "UNPARSED".into(), path: p.clone(), symbol: None, changed_by: None, referenced_by: None, severity: None, agents: vec![agent.into()], security: None }); }
    }
    let both = vec![an.to_string(), bn.to_string()];
    let edited = |side: &Side, p: &str| -> BTreeSet<Key> {
        side.touched.get(p).into_iter().chain(side.removed.get(p)).flatten().cloned().collect()
    };
    for path in sa.touched.keys() {
        if sb.touched.contains_key(path) {
            let (ka, kb) = (edited(&sa, path), edited(&sb, path));
            for k in ka.intersection(&kb) {
                if matches!(k, Key::Opaque(_)) { continue; }
                out.push(Warning { kind: "OVERLAP".into(), path: path.clone(), symbol: Some(k.label()), changed_by: None, referenced_by: None, severity: None, agents: both.clone(), security: None });
            }
        }
    }
    for f in sa.files.intersection(&sb.files) {
        out.push(Warning { kind: "OVERLAP".into(), path: f.clone(), symbol: None, changed_by: None, referenced_by: None, severity: None, agents: both.clone(), security: None });
    }
    if !sa.unparsed.is_empty() || !sb.unparsed.is_empty() { return out; }
    for (ch, chname, other, oname, osnap) in [(&sa, an, &sb, bn, bsnap), (&sb, bn, &sa, an, asnap)] {
        for (path, keys) in &other.touched {
            // An import or re-export line names a symbol but does not depend on what it does; the code that calls
            // it does, and that code is its own key.
            let keys: &BTreeSet<Key> = &keys.iter().filter(|k| !matches!(k, Key::Import(_))).cloned().collect();
            if keys.is_empty() { continue; }
            let Some(info) = osnap.get(path).and_then(|s| analyze_file(path, s)) else { continue };
            let (mut bare, mut qual) = references(&info, keys);
            if let Some(binfo) = base.get(path).and_then(|s| analyze_file(path, s)) {
                // Only NEW references are hazards: base callers were exercised by the changing side's own checks.
                let (ob, oq) = references(&binfo, keys);
                let grown: BTreeSet<String> = BTreeSet::new();
                bare.retain(|n| !ob.contains(n) || grown.contains(n));
                qual.retain(|q| !oq.contains(q) || grown.contains(&q.1));
            }
            let module = info.module.clone();
            let own: BTreeSet<&str> = keys.iter().filter_map(|k| k.bound()).collect();
            for ((cm, name), sev) in &ch.changed {
                // A changed method or property used as `this.name` / `x.name` by code that works with that class.
                let class_member = ch.classes.contains(cm) && cm.rsplit_once('.').map(|(_, c)| (keys.iter().any(|k| matches!(k, Key::Member(kc, _) if kc == c)) || bare.contains(c))
                    && qual.contains(&(ATTR.to_string(), name.clone()))).unwrap_or(false);
                // Java, C# and Kotlin call members of their own class without `this.`: a new member of the class
                // (another part of a C# partial class, say) that names a changed member uses it.
                let own_class = info.lang.map(|l| matches!(l, Lang::Java | Lang::CSharp | Lang::Kotlin)).unwrap_or(false) && bare.contains(name)
                    && keys.iter().any(|k| matches!(k, Key::Member(kc, _) if cm == &format!("{module}.{kc}")));
                let mf = false;
                let hit = class_member || own_class || mf || (cm == &module && bare.contains(name) && !own.contains(name.as_str())) || qual.contains(&(cm.clone(), name.clone()))
                    || (!cm.is_empty() && qual.iter().any(|(m, n)| n == name && m.strip_prefix("go:").map(|r| r == cm || r.ends_with(&format!("/{cm}"))).unwrap_or(false)));
                if hit {
                    let key = (cm.clone(), name.clone());
                    let (kind, sev, note) = match ch.weak.get(&key) {
                        Some(n) => ("SECURITY", "high", Some(n.clone())),
                        None if ch.secure.contains(&key) => ("HAZARD", *sev, Some(security::Note { what: "changed the interface of".into(), risk: "It is a security check: make sure your code still gets the protection it expects.".into() })),
                        None => ("HAZARD", *sev, None),
                    };
                    let symbol = c_cpp::display(cm, name).unwrap_or_else(|| format!("{cm}.{name}"));
                    out.push(Warning { kind: kind.into(), path: path.clone(), symbol: Some(symbol),
                        changed_by: Some(chname.into()), referenced_by: Some(oname.into()), severity: Some(sev.to_string()), agents: both.clone(), security: note });
                }
            }
        }
    }
    // C and C++: a class whose member changed is reported through that member (`leveldb::DB::GetProperty`, not
    // also `leveldb::DB`) when both reach the same new code.
    let members: BTreeSet<(String, String, String)> = out.iter().filter(|w| w.kind != "OVERLAP")
        .filter_map(|w| Some((w.path.clone(), w.changed_by.clone()?, w.symbol.as_ref()?.rsplit_once("::")?.0.to_string()))).collect();
    out.retain(|w| !(w.kind == "HAZARD" && matches!(lang_of(&w.path), Some(Lang::C | Lang::Cpp))
        && members.contains(&(w.path.clone(), w.changed_by.clone().unwrap_or_default(), w.symbol.clone().unwrap_or_default()))));
    out
}

/// Symbols (`module.name`) that an agent's NEW or changed code references, relative to base.
/// Used for claims: warn as soon as an agent starts building on a symbol another agent claimed.
pub fn new_references(base: &Snapshot, x: &Snapshot) -> BTreeSet<String> {
    let side = side_changes(base, x);
    let mut out = BTreeSet::new();
    for (path, keys) in &side.touched {
        if keys.is_empty() { continue; }
        let Some(info) = x.get(path).and_then(|s| analyze_file(path, s)) else { continue };
        let (mut bare, mut qual) = references(&info, keys);
        if let Some(binfo) = base.get(path).and_then(|s| analyze_file(path, s)) {
            let (ob, oq) = references(&binfo, keys);
            bare = bare.difference(&ob).cloned().collect();
            qual = qual.difference(&oq).cloned().collect();
        }
        let module = info.module.clone();
        for b in bare { out.insert(format!("{module}.{b}")); }
        for (m, n) in qual { if m != ATTR { out.insert(format!("{m}.{n}")); } }
    }
    out
}

/// Symbols (`module.name`) an agent added or changed, relative to base (top level and class members).
pub fn provided_symbols(base: &Snapshot, x: &Snapshot) -> BTreeSet<String> {
    let side = side_changes(base, x);
    let mut out: BTreeSet<String> = side.changed.keys().filter(|(m, _)| m != ATTR).map(|(m, n)| format!("{m}.{n}")).collect();
    for (path, keys) in &side.touched {
        let info = x.get(path).or(base.get(path)).and_then(|v| analyze_file(path, v));
        let syms = info.as_ref().map(|i| i.symbols.clone()).unwrap_or_default();
        let module = info.map(|i| i.module).unwrap_or_else(|| module_name(path));
        for k in keys {
            if let Some((m, n)) = syms.get(k) { out.insert(format!("{m}.{n}")); continue; }
            match k { Key::Member(c, inner) => if let Some(n) = inner.bound() { out.insert(format!("{module}.{c}.{n}")); out.insert(format!("{module}.{c}")); },
                      _ => if let Some(n) = k.bound() { out.insert(format!("{module}.{n}")); } }
        }
    }
    out
}

#[cfg(test)]
mod python_layout_tests {
    use super::*;
    #[test]
    fn src_layout_and_relative_imports() {
        assert_eq!(module_name("src/requests/utils.py"), "requests.utils");
        assert_eq!(module_name("libs/core/src/acme/api.py"), "acme.api");
        assert_eq!(module_name("shop/pricing.py"), "shop.pricing");
        assert_eq!(resolve_py("src/requests/diagnostics.py", ".utils"), "requests.utils");
        assert_eq!(resolve_py("src/requests/adapters/http.py", "..utils"), "requests.utils");
        assert_eq!(resolve_py("src/requests/__init__.py", ".api"), "requests.api");
        assert_eq!(resolve_py("src/requests/x.py", "."), "requests");
        assert_eq!(resolve_py("a/b.py", "os.path"), "os.path");
    }
}

#[cfg(test)]
mod csharp_kotlin_layout_tests {
    use super::*;
    #[test]
    fn path_modules_and_declared_namespaces() {
        assert_eq!(module_name("src/main/java/com/acme/util/Util.java"), "com.acme.util");
        assert_eq!(module_name("java/Util.java"), "");
        assert_eq!(module_name("app/src/main/kotlin/shop/pricing/Vat.kt"), "shop.pricing");
        assert_eq!(module_name("src/test/kotlin/shop/VatTest.kt"), "shop");
        assert_eq!(module_name("src/Shop.Pricing/Vat.cs"), "Shop.Pricing");
        assert_eq!(module_name("Shop/Pricing/Vat.cs"), "Shop.Pricing");
        // The declared namespace wins over the path; no namespace is the global namespace.
        assert_eq!(analyze_file("x/y/Vat.cs", b"namespace Shop.Pricing;\nclass Vat {}\n").unwrap().module, "Shop.Pricing");
        assert_eq!(analyze_file("x/y/Vat.cs", b"class Vat {}\n").unwrap().module, "");
        assert_eq!(analyze_file("x/y/vat.kt", b"package shop.pricing\n\nclass Vat\n").unwrap().module, "shop.pricing");
        // Enclosing namespaces are in scope in C#.
        assert!(analyze_file("a.cs", b"namespace Shop.Web.Pages;\nclass A {}\n").unwrap().opens.contains("Shop.Web"));
    }
}
