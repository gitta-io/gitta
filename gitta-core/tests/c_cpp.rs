//! C and C++: symbols, uses, header fan-out, include resolution, preprocessor robustness, severity, merge,
//! security rules and guard rules. Code shapes follow real C and C++ projects (Redis, curl, LevelDB, fmt).
use gitta_core::c_cpp;
use gitta_core::merge::{line_merge, merge_file, MergeError};
use gitta_core::{analyze, analyze_file, merge_hazards, Key, Snapshot, Warning};

fn snap(files: &[(&str, &str)]) -> Snapshot { files.iter().map(|(p, t)| (p.to_string(), t.as_bytes().to_vec())).collect() }

fn high(w: &[Warning]) -> Vec<&Warning> { w.iter().filter(|x| x.kind == "HAZARD" && x.severity.as_deref() == Some("high")).collect() }

/// Agent a changes `path` to `changed`; agent b writes `files`. High hazards that reach b's files.
fn hazards(base: &[(&str, &str)], path: &str, changed: &str, files: &[(&str, &str)]) -> Vec<Warning> {
    let base = snap(base);
    let mut a = base.clone(); a.insert(path.into(), changed.as_bytes().to_vec());
    let mut b = base.clone();
    for (p, t) in files { b.insert(p.to_string(), t.as_bytes().to_vec()); }
    merge_hazards(&base, &a, &b).into_iter().filter(|w| w.kind == "HAZARD" && w.changed_by.as_deref() == Some("head")).collect()
}

fn symbols_of(path: &str, src: &str) -> Vec<String> {
    let i = analyze_file(path, src.as_bytes()).expect("parses");
    i.items.keys().filter_map(|k| i.symbols.get(k)).map(|(m, n)| format!("{m}|{n}")).collect()
}

// ---------------- symbols ----------------

const SERVER_H: &str = r#"#ifndef __REDIS_H
#define __REDIS_H

#include <stdio.h>
#include "sds.h"

#define PROTO_IOBUF_LEN (1024*16)
#define serverAssert(_e) ((_e)?(void)0 : (_serverAssert(#_e,__FILE__,__LINE__),__builtin_unreachable()))

typedef struct client {
    uint64_t id;
    int fd;
    sds querybuf;
    size_t qb_pos;
    int flags;
} client;

struct redisServer {
    int port;
    int maxclients;
    char *bindaddr[16];
};

typedef enum {
    CLIENT_TYPE_NORMAL = 0,
    CLIENT_TYPE_SLAVE,
    CLIENT_TYPE_PUBSUB,
} clientType;

extern struct redisServer server;

void addReplyBulk(client *c, robj *obj);
void addReplyError(client *c, const char *err);
static inline int clientHasPendingReplies(client *c) { return c->bufpos != 0; }

#endif
"#;

#[test]
fn c_symbols_with_their_scope() {
    let s = symbols_of("src/server.h", SERVER_H);
    for want in ["c|PROTO_IOBUF_LEN", "c|serverAssert", "c|struct client", "c|client", "c|client::querybuf", "c|struct redisServer",
                 "c|redisServer::maxclients", "c|clientType", "c|CLIENT_TYPE_SLAVE", "c|server", "c|addReplyBulk", "c|clientHasPendingReplies"] {
        assert!(s.iter().any(|x| x == want), "missing {want} in {s:?}");
    }
    // A source file keeps its static functions, statics and macros to itself.
    let src = "#include \"server.h\"\n#define MAX_KEYS 64\nstatic int keysCount;\nstatic void freeClientArgv(client *c) {\n    c->argc = 0;\n}\nvoid freeClient(client *c) {\n    freeClientArgv(c);\n}\n";
    let s = symbols_of("src/networking.c", src);
    assert!(s.contains(&"c:src/networking.c|MAX_KEYS".to_string()) && s.contains(&"c:src/networking.c|freeClientArgv".to_string()) && s.contains(&"c:src/networking.c|keysCount".to_string()), "{s:?}");
    assert!(s.contains(&"c|freeClient".to_string()), "{s:?}");
    // A prototype and its definition in one file are one item.
    let one = analyze_file("src/x.c", b"static int f(int a);\nint g(void) { return f(1); }\nstatic int f(int a) { return a + 1; }\n").unwrap();
    assert_eq!(one.items.keys().filter(|k| matches!(k, Key::Def(n) if n == "f")).count(), 1);
    assert!(one.items[&Key::Def("f".into())].returns.as_deref().unwrap_or("").contains("a + 1"));
}

const DB_H: &str = r#"#ifndef STORAGE_LEVELDB_INCLUDE_DB_H_
#define STORAGE_LEVELDB_INCLUDE_DB_H_

#include <cstdint>
#include <string>

#include "leveldb/export.h"
#include "leveldb/options.h"

namespace leveldb {

class Snapshot;

class LEVELDB_EXPORT DB {
 public:
  static Status Open(const Options& options, const std::string& name, DB** dbptr);

  DB() = default;
  DB(const DB&) = delete;
  DB& operator=(const DB&) = delete;
  virtual ~DB();

  virtual Status Put(const WriteOptions& options, const Slice& key, const Slice& value) = 0;
  virtual Status Get(const ReadOptions& options, const Slice& key, std::string* value) = 0;
  virtual Status Get(const ReadOptions& options, const Slice& key, PinnableSlice* value) = 0;
  virtual const Snapshot* GetSnapshot() = 0;
  virtual bool GetProperty(const Slice& property, std::string* value) = 0;

 private:
  int refs_;
};

namespace {
int CompactionCount() { return 0; }
}

inline namespace v1 {
Status DestroyDB(const std::string& name, const Options& options);
}

template <typename T>
class Iterator {
 public:
  T Value() const;
  using value_type = T;
};

template <>
class Iterator<Slice> {
 public:
  Slice Value() const;
};

enum class CompressionType : unsigned char { kNoCompression = 0x0, kSnappyCompression = 0x1 };

}  // namespace leveldb

#endif
"#;

#[test]
fn cpp_symbols_namespaces_classes_overloads_templates() {
    let s = symbols_of("include/leveldb/db.h", DB_H);
    for want in ["c|leveldb::DB", "c|leveldb::DB::Open", "c|leveldb::DB::Put", "c|leveldb::DB::~DB", "c|leveldb::DB::operator=", "c|leveldb::DB::refs_",
                 "c|leveldb::DestroyDB", "c|leveldb::Iterator", "c|leveldb::Iterator<Slice>", "c|leveldb::Iterator::value_type",
                 "c|leveldb::CompressionType", "c|leveldb::CompressionType::kSnappyCompression", "c:include/leveldb/db.h|leveldb::CompactionCount"] {
        assert!(s.iter().any(|x| x == want), "missing {want} in {s:?}");
    }
    let i = analyze_file("include/leveldb/db.h", DB_H.as_bytes()).unwrap();
    // Overloads are separate items, keyed by parameter types.
    let gets: Vec<&Key> = i.items.keys().filter(|k| matches!(k, Key::Member(c, d) if c == "leveldb::DB" && matches!(&**d, Key::Def(n) if n.starts_with("leveldb::DB::Get(")))).collect();
    assert_eq!(gets.len(), 2, "{gets:?}");
    // A function with no overload is keyed by its name; its signature carries what callers and overriders rely on.
    let put = &i.items[&Key::Member("leveldb::DB".into(), Box::new(Key::Def("leveldb::DB::Put".into())))];
    let sig = put.sig.clone().unwrap();
    assert!(sig.contains("virtual") && sig.contains("=0") && sig.contains("const WriteOptions&") && sig.ends_with("-> Status"), "{sig}");
    // Out-of-line definitions in a namespace are named like the declaration.
    let s = symbols_of("db/db_impl.cc", "namespace leveldb {\n\nStatus DBImpl::Put(const WriteOptions& o, const Slice& key, const Slice& val) {\n  return DB::Put(o, key, val);\n}\n\n}  // namespace leveldb\n");
    assert!(s.contains(&"c|leveldb::DBImpl::Put".to_string()), "{s:?}");
}

// ---------------- hazards across files ----------------

const NETWORKING_C: &str = "#include \"server.h\"\n\nvoid addReplyBulk(client *c, robj *obj) {\n    addReplyBulkLen(c, obj);\n    addReply(c, obj);\n}\n";

#[test]
fn header_prototype_change_reaches_every_new_caller() {
    let base = [("src/server.h", SERVER_H), ("src/networking.c", NETWORKING_C), ("src/t_string.c", "#include \"server.h\"\n\nvoid getCommand(client *c) {\n    getGenericCommand(c);\n}\n"),
                ("src/t_hash.c", "#include \"server.h\"\n\nvoid hgetCommand(client *c) {\n    hashTypeGet(c);\n}\n")];
    let changed = SERVER_H.replace("void addReplyBulk(client *c, robj *obj);", "void addReplyBulk(client *c, robj *obj, int flags);");
    let w = hazards(&base, "src/server.h", &changed, &[
        ("src/t_string.c", "#include \"server.h\"\n\nvoid getCommand(client *c) {\n    getGenericCommand(c);\n}\n\nvoid getdelCommand(client *c) {\n    addReplyBulk(c, c->argv[1]);\n}\n"),
        ("src/t_hash.c", "#include \"server.h\"\n\nvoid hgetCommand(client *c) {\n    hashTypeGet(c);\n}\n\nvoid hstrlenCommand(client *c) {\n    addReplyBulk(c, c->argv[2]);\n}\n"),
    ]);
    let paths: Vec<&str> = w.iter().map(|x| x.path.as_str()).collect();
    assert!(paths.contains(&"src/t_string.c") && paths.contains(&"src/t_hash.c"), "{w:?}");
    assert!(w.iter().all(|x| x.symbol.as_deref() == Some("addReplyBulk")), "{w:?}");
    // An unrelated new function in the same files: nothing.
    let w = hazards(&base, "src/server.h", &changed, &[("src/t_hash.c", "#include \"server.h\"\n\nvoid hgetCommand(client *c) {\n    hashTypeGet(c);\n}\n\nvoid hlenCommand(client *c) {\n    hashTypeLength(c);\n}\n")]);
    assert!(w.is_empty(), "{w:?}");
    // A change in the definition that does not change what it hands back is not a hazard.
    let body = NETWORKING_C.replace("    addReply(c, obj);\n", "    serverLog(LL_DEBUG, \"bulk\");\n    addReply(c, obj);\n");
    let w = hazards(&base, "src/networking.c", &body, &[("src/t_string.c", "#include \"server.h\"\n\nvoid getCommand(client *c) {\n    getGenericCommand(c);\n}\n\nvoid getdelCommand(client *c) {\n    addReplyBulk(c, c->argv[1]);\n}\n")]);
    assert!(w.is_empty(), "{w:?}");
}

#[test]
fn struct_field_changes_reach_field_uses() {
    let base = [("src/server.h", SERVER_H), ("src/networking.c", NETWORKING_C)];
    let typed = SERVER_H.replace("    size_t qb_pos;\n", "    int qb_pos;\n");
    // Through a typed parameter, a typedef name and `->`.
    let w = hazards(&base, "src/server.h", &typed, &[("src/networking.c", &format!("{NETWORKING_C}\nsize_t clientQueryPos(client *c) {{\n    return c->qb_pos;\n}}\n"))]);
    assert!(w.iter().any(|x| x.symbol.as_deref() == Some("client::qb_pos")), "{w:?}");
    // Through a global declared in the header (`extern struct redisServer server;`), from another file.
    let ported = SERVER_H.replace("    int maxclients;\n", "    unsigned int maxclients;\n");
    let w = hazards(&base, "src/server.h", &ported, &[("src/config.c", "#include \"server.h\"\n\nint maxClientsReached(int n) {\n    return n >= server.maxclients;\n}\n")]);
    assert!(w.iter().any(|x| x.symbol.as_deref() == Some("server.maxclients")), "{w:?}");
    // Through a member chain whose outer struct is defined in the header (`data->set.timeout` in curl's style).
    let urldata = "struct UserDefined {\n  long timeout;\n  long connecttimeout;\n};\n\nstruct Curl_easy {\n  struct UserDefined set;\n  int magic;\n};\n";
    let base2 = [("lib/urldata.h", urldata), ("lib/transfer.c", "#include \"urldata.h\"\n")];
    let w = hazards(&base2, "lib/urldata.h", &urldata.replace("  long timeout;", "  timediff_t timeout;"),
        &[("lib/transfer.c", "#include \"urldata.h\"\n\nlong Curl_timeleft(struct Curl_easy *data) {\n  return data->set.timeout;\n}\n")]);
    assert!(w.iter().any(|x| x.symbol.as_deref() == Some("Curl_easy::set.timeout")), "{w:?}");
    // A new field changes the layout: new code that works with the struct depends on it.
    let grown = SERVER_H.replace("    int flags;\n} client;", "    int flags;\n    int resp;\n} client;");
    let w = hazards(&base, "src/server.h", &grown, &[("src/networking.c", &format!("{NETWORKING_C}\nclient *createClient(void) {{\n    client *c = zmalloc(sizeof(client));\n    return c;\n}}\n"))]);
    assert!(w.iter().any(|x| x.symbol.as_deref() == Some("client")), "{w:?}");
    // A field of another struct with the same name changed: no hazard for a different struct.
    let other = SERVER_H.replace("    int port;\n", "    long port;\n");
    let w = hazards(&base, "src/server.h", &other, &[("src/networking.c", &format!("{NETWORKING_C}\nint clientFd(client *c) {{\n    return c->fd;\n}}\n"))]);
    assert!(w.is_empty(), "{w:?}");
}

#[test]
fn macros_enums_and_file_local_names() {
    let base = [("src/server.h", SERVER_H), ("src/networking.c", NETWORKING_C)];
    // An object-like macro's value.
    let w = hazards(&base, "src/server.h", &SERVER_H.replace("(1024*16)", "(1024*32)"),
        &[("src/networking.c", &format!("{NETWORKING_C}\nvoid readQuery(client *c) {{\n    char buf[PROTO_IOBUF_LEN];\n    readInto(c, buf);\n}}\n"))]);
    assert!(w.iter().any(|x| x.symbol.as_deref() == Some("PROTO_IOBUF_LEN")), "{w:?}");
    // A function-like macro's parameters.
    let w = hazards(&base, "src/server.h", &SERVER_H.replace("#define serverAssert(_e) ((_e)?", "#define serverAssert(_e, _msg) ((_e)?"),
        &[("src/networking.c", &format!("{NETWORKING_C}\nvoid checkClient(client *c) {{\n    serverAssert(c->fd >= 0);\n}}\n"))]);
    assert!(w.iter().any(|x| x.symbol.as_deref() == Some("serverAssert")), "{w:?}");
    // An enum constant inserted in the middle moves the later values; one appended at the end does not.
    let mid = SERVER_H.replace("    CLIENT_TYPE_SLAVE,\n", "    CLIENT_TYPE_REPLICA_ONLY,\n    CLIENT_TYPE_SLAVE,\n");
    let user = format!("{NETWORKING_C}\nint isPubsub(client *c) {{\n    return getClientType(c) == CLIENT_TYPE_PUBSUB;\n}}\n");
    let w = hazards(&base, "src/server.h", &mid, &[("src/networking.c", &user)]);
    assert!(w.iter().any(|x| x.symbol.as_deref() == Some("CLIENT_TYPE_PUBSUB")), "{w:?}");
    let end = SERVER_H.replace("    CLIENT_TYPE_PUBSUB,\n", "    CLIENT_TYPE_PUBSUB,\n    CLIENT_TYPE_MASTER,\n");
    assert!(hazards(&base, "src/server.h", &end, &[("src/networking.c", &user)]).is_empty());
    // A static function of one file changed; another file has its own static of that name.
    let a_c = "static int dictExpandIfNeeded(dict *d) {\n    return d->size;\n}\n";
    let b_c = "static int dictExpandIfNeeded(dict *d) {\n    return 0;\n}\n";
    let base = [("src/dict.c", a_c), ("src/zipmap.c", b_c)];
    let w = hazards(&base, "src/dict.c", &a_c.replace("dict *d)", "dict *d, int force)"),
        &[("src/zipmap.c", &format!("{b_c}\nint grow(dict *d) {{\n    return dictExpandIfNeeded(d);\n}}\n"))]);
    assert!(w.is_empty(), "{w:?}");
    let w = hazards(&base, "src/dict.c", &a_c.replace("dict *d)", "dict *d, int force)"),
        &[("src/dict.c", &format!("{a_c}\nint grow(dict *d) {{\n    return dictExpandIfNeeded(d);\n}}\n"))]);
    assert!(w.iter().any(|x| x.symbol.as_deref() == Some("dictExpandIfNeeded")), "{w:?}");
}

const DB_IMPL_CC: &str = "#include \"db/db_impl.h\"\n\nnamespace leveldb {\n\nStatus DBImpl::Write(const WriteOptions& options, WriteBatch* updates) {\n  return WriteInternal(options, updates);\n}\n\n}  // namespace leveldb\n";

#[test]
fn cpp_methods_qualifiers_and_lookup() {
    let base = [("include/leveldb/db.h", DB_H), ("db/db_impl.cc", DB_IMPL_CC)];
    // A method's const qualifier, through a typed local in another namespace with a qualified type.
    let h = DB_H.replace("virtual bool GetProperty(const Slice& property, std::string* value) = 0;", "virtual bool GetProperty(const Slice& property, std::string* value) const = 0;");
    let user = "#include \"leveldb/db.h\"\n\nnamespace tools {\n\nvoid Dump(leveldb::DB* db) {\n  std::string stats;\n  db->GetProperty(\"leveldb.stats\", &stats);\n}\n\n}  // namespace tools\n";
    let w = hazards(&base, "include/leveldb/db.h", &h, &[("tools/dump.cc", user)]);
    assert!(w.iter().any(|x| x.symbol.as_deref() == Some("leveldb::DB::GetProperty")), "{w:?}");
    // Inside the namespace, the unqualified class name and `using namespace` find it too.
    let inside = "#include \"leveldb/db.h\"\n\nnamespace leveldb {\n\nvoid Dump(DB* db) {\n  std::string stats;\n  db->GetProperty(\"leveldb.stats\", &stats);\n}\n\n}  // namespace leveldb\n";
    assert!(!hazards(&base, "include/leveldb/db.h", &h, &[("db/dump.cc", inside)]).is_empty());
    let using = "#include \"leveldb/db.h\"\n\nusing namespace leveldb;\n\nvoid Dump(DB* db) {\n  std::string stats;\n  db->GetProperty(\"leveldb.stats\", &stats);\n}\n";
    assert!(!hazards(&base, "include/leveldb/db.h", &h, &[("tools/dump2.cc", using)]).is_empty());
    // A static member function called by its qualified name; noexcept and override count.
    let open = DB_H.replace("static Status Open(const Options& options, const std::string& name, DB** dbptr);", "static Status Open(const Options& options, const std::string& name, DB** dbptr) noexcept;");
    let caller = "#include \"leveldb/db.h\"\n\nint main() {\n  leveldb::DB* db;\n  leveldb::Options options;\n  leveldb::Status s = leveldb::DB::Open(options, \"/tmp/testdb\", &db);\n  return s.ok() ? 0 : 1;\n}\n";
    assert!(high(&analyze(&snap(&base), ("a", &snap(&[("include/leveldb/db.h", &open), ("db/db_impl.cc", DB_IMPL_CC)])), ("b", &snap(&[("include/leveldb/db.h", DB_H), ("db/db_impl.cc", DB_IMPL_CC), ("db/main.cc", caller)]))))
        .iter().any(|x| x.symbol.as_deref() == Some("leveldb::DB::Open")));
    // An inline namespace is not part of the name callers write.
    let d = DB_H.replace("Status DestroyDB(const std::string& name, const Options& options);", "Status DestroyDB(const std::string& name, const Options& options, bool force);");
    let caller = "#include \"leveldb/db.h\"\n\nvoid Wipe(const leveldb::Options& o) {\n  leveldb::DestroyDB(\"/tmp/db\", o);\n}\n";
    assert!(!hazards(&base, "include/leveldb/db.h", &d, &[("tools/wipe.cc", caller)]).is_empty());
    // A parameter appended with a default value keeps every call the same.
    let dflt = DB_H.replace("Status DestroyDB(const std::string& name, const Options& options);", "Status DestroyDB(const std::string& name, const Options& options, bool force = false);");
    assert!(hazards(&base, "include/leveldb/db.h", &dflt, &[("tools/wipe.cc", caller)]).is_empty());
    // A using-alias's target type.
    let alias = DB_H.replace("using value_type = T;", "using value_type = const T&;");
    let user = "#include \"leveldb/db.h\"\n\nnamespace leveldb {\n\ntemplate <typename T>\ntypename Iterator<T>::value_type First(Iterator<T>& it) {\n  return it.Value();\n}\n\n}\n";
    assert!(!hazards(&base, "include/leveldb/db.h", &alias, &[("db/first.h", user)]).is_empty());
}

#[test]
fn transitive_change_through_a_helper() {
    // The header is unchanged; the helper's result changed, and a function that returns it changed meaning too.
    let util = "#include \"util.h\"\n\nsize_t curlx_sotouz(curl_off_t v) {\n  return (size_t)v;\n}\n\nsize_t Curl_bufsize(curl_off_t v) {\n  return curlx_sotouz(v);\n}\n";
    let base = [("lib/util.c", util), ("lib/util.h", "size_t curlx_sotouz(curl_off_t v);\nsize_t Curl_bufsize(curl_off_t v);\n")];
    let changed = util.replace("  return (size_t)v;", "  return v < 0 ? 0 : (size_t)v;");
    let w = hazards(&base, "lib/util.c", &changed, &[("lib/transfer.c", "#include \"util.h\"\n\nvoid Curl_xfer_setup(struct Curl_easy *data, curl_off_t size) {\n  data->req.size = Curl_bufsize(size);\n}\n")]);
    assert!(w.iter().any(|x| x.symbol.as_deref() == Some("Curl_bufsize")), "{w:?}");
}

// ---------------- includes and settings ----------------

#[test]
fn include_resolution_and_header_language() {
    let files = ["lib/vtls/openssl.c", "lib/curl_setup.h", "include/curl/curl.h", "lib/vtls/vtls.h", "src/tool_cfgable.h", "deps/hiredis/sds.h"];
    let exists = |p: &str| files.contains(&p);
    let mods = gitta_core::imports_of("lib/vtls/openssl.c", b"#include \"curl_setup.h\"\n#include \"vtls.h\"\n#include <curl/curl.h>\n#include <openssl/ssl.h>\n");
    let resolved: Vec<Vec<String>> = mods.iter().map(|m| c_cpp::resolve_include(m, exists, files.iter().copied()).expect("an include")).collect();
    let all: Vec<&String> = resolved.iter().flatten().collect();
    // Next to the file first; then by its path from the repository (common layouts, include/), <...> only in include dirs.
    assert!(all.contains(&&"lib/vtls/vtls.h".to_string()) && all.contains(&&"include/curl/curl.h".to_string()) && all.contains(&&"lib/curl_setup.h".to_string()), "{resolved:?}");
    assert!(resolved.iter().any(|v| v.is_empty()), "<openssl/ssl.h> is a system header: {resolved:?}");
    // Configured include directories come before the fallbacks.
    let cfg = c_cpp::parse_config("# include paths\ndeps/hiredis\n.h c++ src/\nnamespace PROTOBUF_NAMESPACE_OPEN google\n");
    assert_eq!(cfg.dirs, vec!["deps/hiredis".to_string()]);
    assert_eq!(cfg.h_rules.len(), 1);
    assert_eq!(cfg.ns_macros, vec![("PROTOBUF_NAMESPACE_OPEN".to_string(), "google".to_string())]);
    c_cpp::set_config(cfg.dirs, cfg.h_rules);
    let m = gitta_core::imports_of("src/server.c", b"#include <sds.h>\n");
    let r = c_cpp::resolve_include(m.iter().next().unwrap(), exists, files.iter().copied()).unwrap();
    assert_eq!(r, vec!["deps/hiredis/sds.h".to_string()]);
    assert_eq!(c_cpp::header_lang("src/tool_cfgable.h", None), gitta_core::Lang::Cpp);
    assert_eq!(c_cpp::header_lang("lib/curl_setup.h", Some(b"#define CURL_SETUP 1\nint x;\n")), gitta_core::Lang::C);
    c_cpp::set_config(vec![], vec![]);
    // Without a rule: C++ by the header's text, or because C++ sources include it.
    assert_eq!(c_cpp::header_lang("include/fmt/core.h", Some(b"namespace fmt {\ntemplate <typename T> class basic_string_view;\n}\n")), gitta_core::Lang::Cpp);
    let headers = ["src/util.h".to_string()].into_iter().collect();
    let srcs = [("src/main.cpp", &b"#include \"util.h\"\n"[..])];
    let inc = c_cpp::cpp_included(srcs.iter().map(|(p, s)| (*p, *s)), &headers);
    assert!(inc.contains("src/util.h"));
    c_cpp::set_cpp_headers(inc);
    assert_eq!(c_cpp::header_lang("src/util.h", None), gitta_core::Lang::Cpp);
    c_cpp::set_cpp_headers(Default::default());
    // A C++ header named .h reads as C++ (fmt keeps its headers in .h files).
    let i = analyze_file("include/fmt/base.h", b"namespace fmt {\ninline namespace v11 {\ntemplate <typename T> struct formatter {};\nauto vformat(string_view fmt, format_args args) -> std::string;\n}\n}\n").unwrap();
    assert_eq!(i.lang, Some(gitta_core::Lang::Cpp));
    assert!(i.symbols.values().any(|(_, n)| n == "fmt::vformat"), "{:?}", i.symbols);
    // A namespace opened by a macro (fmt's FMT_BEGIN_NAMESPACE); right after `namespace absl {` it is inline.
    let i = analyze_file("include/fmt/format.h", b"#include \"base.h\"\n\nFMT_BEGIN_NAMESPACE\nFMT_API auto vformat(string_view fmt, format_args args) -> std::string;\nFMT_END_NAMESPACE\n").unwrap();
    assert!(i.symbols.values().any(|(_, n)| n == "fmt::vformat"), "{:?}", i.symbols);
    let i = analyze_file("absl/strings/str_cat.h", b"namespace absl {\nABSL_NAMESPACE_BEGIN\n\nstd::string StrCat(const AlphaNum& a);\n\nABSL_NAMESPACE_END\n}  // namespace absl\n").unwrap();
    assert!(i.symbols.values().any(|(_, n)| n == "absl::StrCat"), "{:?}", i.symbols);
}

// ---------------- preprocessor ----------------

#[test]
fn preprocessor_branches_x_macros_and_continuations() {
    let src = r#"#include "curl_setup.h"

#ifdef USE_OPENSSL
static int ossl_init(void)
#else
static int ossl_init(int flags)
#endif
{
  return 1;
}

#if defined(_WIN32) && !defined(__MINGW32__)
#define CURL_FORMAT_CURL_OFF_T "I64d"
#elif defined(__APPLE__)
#define CURL_FORMAT_CURL_OFF_T "lld"
#else
#define CURL_FORMAT_CURL_OFF_T "ld"
#endif

#define CURL_ERRORS \
  X(CURLE_OK, "No error") \
  X(CURLE_UNSUPPORTED_PROTOCOL, "Unsupported protocol")

#define X(code, msg) case code: return msg;
const char *curl_easy_strerror(CURLcode error)
{
  switch(error) {
  CURL_ERRORS
  default: break;
  }
  return "Unknown error";
}
#undef X

#if 0
this is not C {{{ at all
#endif

int Curl_ssl_init(void)
{
#ifdef USE_SSL
  if(!init_ssl) {
#endif
    init_ssl = TRUE;
#ifdef USE_SSL
  }
#endif
  return ossl_init();
}
"#;
    let i = analyze_file("lib/vtls/vtls.c", src.as_bytes()).expect("a file with directives parses");
    let names: Vec<&String> = i.symbols.values().map(|(_, n)| n).collect();
    for want in ["ossl_init", "CURL_FORMAT_CURL_OFF_T", "CURL_ERRORS", "X", "curl_easy_strerror", "Curl_ssl_init"] {
        assert!(names.iter().any(|n| *n == want), "missing {want} in {names:?}");
    }
    // The X-macro expansion site uses the list macro.
    let f = i.items.iter().find(|(k, _)| matches!(k, Key::Def(n) if n == "curl_easy_strerror")).unwrap().1;
    assert!(f.refs.idents.contains("CURL_ERRORS") || f.refs.qual.iter().any(|(_, n)| n == "CURL_ERRORS"));
    // A macro value change in one branch is a change to the macro.
    let changed = src.replace("\"lld\"", "\"qd\"");
    let a = analyze_file("lib/vtls/vtls.c", changed.as_bytes()).unwrap();
    let k = Key::Def("#define CURL_FORMAT_CURL_OFF_T".into());
    assert_ne!(a.items[&k].text, i.items[&k].text);
}

#[test]
fn damaged_and_odd_sources_never_panic() {
    let samples = [SERVER_H, DB_H, NETWORKING_C, DB_IMPL_CC];
    for s in samples {
        let b = s.as_bytes();
        // Every prefix cut at a line end, and every 97th byte cut: never a panic.
        for (i, _) in b.iter().enumerate().filter(|(i, c)| **c == b'\n' || i % 97 == 0) {
            let _ = analyze_file("src/cut.h", &b[..i]);
            let _ = analyze_file("src/cut.cc", &b[..i]);
        }
    }
    for odd in ["#if\n", "#endif\n#endif\n", "#else\n}\n", "\\\n\\\n", "#define A(\n", "}}}}{{{{", "template <", "namespace {", "\u{feff}int x;", "struct { int a; } ;", "#include\n", "extern \"C\" {\n", "??", "??/", "%:%", "<:", "'1'2'"] {
        let _ = analyze_file("src/odd.c", odd.as_bytes());
        let _ = analyze_file("src/odd.cpp", odd.as_bytes());
    }
    assert!(analyze_file("src/bin.c", b"int x;\0\0\x01").is_none(), "binary content is not read as C");
}

// ---------------- merge ----------------

#[test]
fn c_structural_merge_and_conflicts() {
    let base = "#include \"server.h\"\n\nvoid pingCommand(client *c) {\n    addReply(c, shared.pong);\n}\n\nvoid echoCommand(client *c) {\n    addReplyBulk(c, c->argv[1]);\n}\n";
    let head = base.replace("    addReply(c, shared.pong);\n", "    if (c->argc > 2) {\n        addReplyErrorArity(c);\n        return;\n    }\n    addReply(c, shared.pong);\n");
    let sub = base.replace("void echoCommand(client *c) {\n", "void echoCommand(client *c) {\n    if (c->argc != 2) return;\n");
    let m = merge_file("src/server.c", Some(base.as_bytes()), Some(head.as_bytes()), Some(sub.as_bytes())).unwrap().unwrap();
    let m = String::from_utf8(m).unwrap();
    assert!(m.contains("addReplyErrorArity") && m.contains("c->argc != 2"));
    // Both sides add a new function at the same place: both kept.
    let h2 = format!("{base}\nvoid timeCommand(client *c) {{\n    addReplyArrayLen(c, 2);\n}}\n");
    let s2 = format!("{base}\nvoid lastsaveCommand(client *c) {{\n    addReplyLongLong(c, server.lastsave);\n}}\n");
    assert!(line_merge(base.as_bytes(), h2.as_bytes(), s2.as_bytes()).is_err());
    let m = String::from_utf8(merge_file("src/server.c", Some(base.as_bytes()), Some(h2.as_bytes()), Some(s2.as_bytes())).unwrap().unwrap()).unwrap();
    assert!(m.contains("timeCommand") && m.contains("lastsaveCommand"), "{m}");
    // The same function added differently: a conflict.
    let s3 = format!("{base}\nvoid timeCommand(client *c) {{\n    addReplyArrayLen(c, 3);\n}}\n");
    assert!(matches!(merge_file("src/server.c", Some(base.as_bytes()), Some(h2.as_bytes()), Some(s3.as_bytes())), Err(MergeError::Conflict(_))));
    // C++ member functions inside a namespace and class merge as units.
    let cb = "namespace leveldb {\n\nclass Cache {\n public:\n  void Erase(const Slice& key) {\n    Remove(key);\n  }\n\n  void Prune() {\n    Clear();\n  }\n};\n\n}  // namespace leveldb\n";
    let ch = cb.replace("    Remove(key);\n", "    MutexLock l(&mutex_);\n    Remove(key);\n");
    let cs = cb.replace("    Clear();\n", "    MutexLock l(&mutex_);\n    Clear();\n");
    let m = String::from_utf8(merge_file("util/cache.cc", Some(cb.as_bytes()), Some(ch.as_bytes()), Some(cs.as_bytes())).unwrap().unwrap()).unwrap();
    assert_eq!(m.matches("MutexLock").count(), 2, "{m}");
}

// ---------------- security, layer 1 ----------------

fn notes(path: &str, old: &str, new: &str) -> Vec<String> {
    let base = snap(&[(path, old)]);
    let n = snap(&[(path, new)]);
    gitta_core::code_security::findings(&base, &n, &[path.to_string()]).into_iter().map(|x| x.rule).collect()
}

#[test]
fn c_security_rules_flagged_and_safe() {
    let p = "src/tool_operate.c";
    let base = "#include <stdio.h>\n\nint run(const char *user, char *buf, size_t n) {\n  return 0;\n}\n";
    let add = |line: &str| base.replace("  return 0;\n", &format!("  {line}\n  return 0;\n"));
    let pairs: &[(&str, &str, &str)] = &[
        ("unsafe-c-buffer", "strcpy(buf, user);", "snprintf(buf, n, \"%s\", user);"),
        ("unsafe-c-buffer", "sprintf(buf, \"user=%s\", user);", "strcpy(buf, \"none\");"),
        ("unsafe-c-buffer", "gets(buf);", "fgets(buf, (int)n, stdin);"),
        ("unsafe-c-buffer", "sscanf(user, \"%s\", buf);", "sscanf(user, \"%63s\", buf);"),
        ("unsafe-c-buffer", "memcpy(buf, user, len);", "memcpy(buf, user, sizeof(hdr));"),
        ("unsafe-c-buffer", "memmove(buf, user, len);", "memmove(buf, user, 64);"),
        ("unsafe-c-buffer", "memcpy(buf, user, len + 1);", "memcpy(buf, user, HDR_LEN + 1);"),
        ("unsafe-c-buffer", "memcpy(buf, user, s->len);", "memcpy(buf, user, len < n ? len : n);"),
        ("unsafe-c-buffer", "memcpy(buf, user, nread);", "memcpy(buf, user, MIN(nread, n));"),
        ("format-string", "printf(user);", "printf(\"%s\", user);"),
        ("format-string", "fprintf(stderr, user);", "fprintf(stderr, \"%s\\n\", user);"),
        ("format-string", "syslog(LOG_ERR, user);", "syslog(LOG_ERR, \"%s\", user);"),
        ("format-string", "printf(msg);", "printf(ok ? \"true\" : \"false\");"),
        ("tls-off", "SSL_set_verify(ssl, SSL_VERIFY_NONE, NULL);", "SSL_CTX_set_verify(ctx, insecure ? SSL_VERIFY_NONE : SSL_VERIFY_PEER, NULL);"),
        ("shell-text", "system(cmd);", "system(\"clear\");"),
        ("shell-text", "FILE *f = popen(cmd, \"r\");", "FILE *f = popen(\"ls\", \"r\");"),
        ("shell-text", "execl(\"/bin/sh\", \"sh\", \"-c\", cmd, (char *)NULL);", "execl(\"/usr/bin/git\", \"git\", \"status\", (char *)NULL);"),
        ("tls-off", "curl_easy_setopt(curl, CURLOPT_SSL_VERIFYPEER, 0L);", "curl_easy_setopt(curl, CURLOPT_SSL_VERIFYPEER, 1L);"),
        ("tls-off", "SSL_CTX_set_verify(ctx, SSL_VERIFY_NONE, NULL);", "SSL_CTX_set_verify(ctx, SSL_VERIFY_PEER, NULL);"),
        ("weak-password-hash", "MD5((unsigned char *)password, strlen(password), digest);", "MD5((unsigned char *)data, len, digest);"),
        ("secret-in-log", "printf(\"token=%s\\n\", access_token);", "printf(\"token set: %d\\n\", access_token != NULL);"),
        ("hardcoded-secret", "static const char *api_key = \"Xk29fj3Lq\";", "static const char *api_key = NULL;"),
    ];
    for (rule, bad, good) in pairs {
        assert!(notes(p, base, &add(bad)).iter().any(|r| r == rule), "{rule} not flagged: {bad}");
        assert!(!notes(p, base, &add(good)).iter().any(|r| r == rule), "{rule} flagged on safe code: {good}");
    }
    // The length was checked just before the copy.
    let checked = base.replace("  return 0;\n", "  if (len > n) return -1;\n  memcpy(buf, user, len);\n  return 0;\n");
    assert!(!notes(p, base, &checked).iter().any(|r| r == "unsafe-c-buffer"));
    // Set from sizeof, or the destination was allocated with it: bounded. A `->` near the length is no check.
    for (pre, flagged) in [("  size_t len = sizeof(hdr);\n", false), ("  char *buf2 = malloc(len + 1);\n", false),
                           ("  len = s->len;\n", true), ("  len = strlen(user);\n", true)] {
        let src = base.replace("  return 0;\n", &format!("{pre}  memcpy(buf, user, len);\n  return 0;\n"));
        assert_eq!(notes(p, base, &src).iter().any(|r| r == "unsafe-c-buffer"), flagged, "{pre}");
    }
    // A line that was already there is not new.
    let old = add("strcpy(buf, user);");
    assert!(notes(p, &old, &old.replace("int run(", "int run2(")).is_empty());
}

#[test]
fn c_security_layer_two_by_name() {
    // A check named like one gains an early allow: a weakened control for code that calls it.
    let base = "int ssl_verify_peer(SSL *ssl) {\n  if (SSL_get_verify_result(ssl) != X509_V_OK)\n    return -1;\n  return 0;\n}\n";
    let weak = "int ssl_verify_peer(SSL *ssl) {\n  return 0;\n}\n";
    let b = snap(&[("lib/vtls/check.c", base)]);
    let a = snap(&[("lib/vtls/check.c", weak)]);
    let mut c = b.clone(); c.insert("lib/vtls/conn.c".into(), b"int connect_tls(SSL *ssl) {\n  return ssl_verify_peer(ssl);\n}\n".to_vec());
    let w = analyze(&b, ("a", &a), ("b", &c));
    assert!(w.iter().any(|x| x.kind == "SECURITY" && x.symbol.as_deref() == Some("ssl_verify_peer")), "{w:?}");
    let f = gitta_core::guard::safety_net(&b, &a, &["lib/vtls/check.c".to_string()]);
    assert!(f.iter().any(|x| x.kind == "SECURITY_WEAKENED"), "{f:?}");
}

// ---------------- guard ----------------

#[test]
fn c_cpp_tests_recognised_and_weakening_found() {
    for p in ["db/db_test.cc", "util/cache_unittest.cpp", "tests/unit/test_sds.c", "test/format-test.cc", "src/FooTest.cpp", "tests/catch/string_tests.cpp"] {
        assert!(gitta_core::guard::is_test(p), "{p}");
    }
    assert!(!gitta_core::guard::is_test("src/latest.c") && !gitta_core::guard::is_test("lib/contest.cc"));
    let base = "#include <gtest/gtest.h>\n\nTEST(DBTest, Empty) {\n  ASSERT_TRUE(db_ != nullptr);\n  EXPECT_EQ(\"NOT_FOUND\", Get(\"foo\"));\n}\n\nTEST(DBTest, ReadWrite) {\n  ASSERT_LEVELDB_OK(Put(\"foo\", \"v1\"));\n}\n";
    let kinds = |p: &str, new: &str| -> Vec<String> {
        gitta_core::guard::safety_net(&snap(&[(p, base)]), &snap(&[(p, new)]), &[p.to_string()]).into_iter().map(|f| f.kind).collect()
    };
    assert!(kinds("db/db_test.cc", &base.replace("TEST(DBTest, Empty)", "TEST(DBTest, DISABLED_Empty)")).contains(&"TEST_SKIPPED".to_string()));
    assert!(kinds("db/db_test.cc", &base.replace("  ASSERT_TRUE(db_ != nullptr);\n", "  GTEST_SKIP() << \"flaky\";\n  ASSERT_TRUE(db_ != nullptr);\n")).contains(&"TEST_SKIPPED".to_string()));
    assert!(kinds("db/db_test.cc", &base.replace("TEST(DBTest, ReadWrite) {", "#if 0\nTEST(DBTest, ReadWrite) {").replace("\"v1\"));\n}\n", "\"v1\"));\n}\n#endif\n")).contains(&"TEST_SKIPPED".to_string()));
    assert!(kinds("db/db_test.cc", &base.replace("  EXPECT_EQ(\"NOT_FOUND\", Get(\"foo\"));\n", "")).contains(&"ASSERTIONS_REMOVED".to_string()));
    // A test file by its content even when its name says nothing (CTest suites often live in plain .cc files).
    assert!(kinds("db/recovery.cc", &base.replace("TEST(DBTest, Empty)", "TEST(DBTest, DISABLED_Empty)")).contains(&"TEST_SKIPPED".to_string()));
    let cm = "enable_testing()\nadd_test(NAME db_test COMMAND db_test)\nadd_test(NAME cache_test COMMAND cache_test)\n";
    let f = gitta_core::guard::safety_net(&snap(&[("CMakeLists.txt", cm)]), &snap(&[("CMakeLists.txt", "enable_testing()\nadd_test(NAME db_test COMMAND db_test)\n")]), &["CMakeLists.txt".to_string()]);
    assert!(f.iter().any(|x| x.kind == "TEST_DELETED"), "{f:?}");
}


#[test]
fn invalid_utf8_near_a_parameter_name_does_not_crash_the_analyzer() {
    // A C/C++ file reaches analyze_file through the hooks and the merge hazard check, which (unlike the structural
    // merge) do not require valid UTF-8. A byte that is not valid UTF-8 just before a parameter name must not panic.
    for src in [
        &b"int f(int\xff x){return x;}\n"[..],
        &b"int g(const char\xc3 *name){return 0;}\n"[..],
        &b"void h(long\xff\xff count, int y){}\n"[..],
    ] {
        for path in ["a.c", "a.cpp", "a.h"] {
            let _ = analyze_file(path, src);   // must return, not panic
        }
    }
    // And through a real merge: both sides add a malformed .cpp, so the hazard analysis runs on it.
    let base = snap(&[("m.cpp", "int f(int x){return x;}\n")]);
    let mut a = base.clone(); a.insert("m.cpp".into(), b"int f(int\xff x){return x + 1;}\n".to_vec());
    let mut b = base.clone(); b.insert("n.cpp".into(), b"int g(int\xff y){return y;}\n".to_vec());
    let _ = merge_hazards(&base, &a, &b);   // must return, not panic
}
