//! C and C++ include settings: resolution and header language. The test changes gitta-core's process-wide C/C++
//! settings, so it runs in its own test binary, apart from the tests that read them.
use gitta_core::analyze_file;
use gitta_core::c_cpp;

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
