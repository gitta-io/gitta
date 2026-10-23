use gitta_core::merge::{line_merge, merge_file, MergeError};
use gitta_core::{analyze, Snapshot, Warning};

fn snap(files: &[(&str, &str)]) -> Snapshot { files.iter().map(|(p, t)| (p.to_string(), t.as_bytes().to_vec())).collect() }

fn high_hazards(w: &[Warning]) -> Vec<&Warning> {
    w.iter().filter(|x| x.kind == "HAZARD" && x.severity.as_deref() == Some("high") && x.referenced_by.as_deref() == Some("b")).collect()
}

struct Case<'a> { lib_path: &'static str, base_lib: &'static str, changed_lib: &'a str, logged_lib: &'a str,
              extra_base: Vec<(&'static str, &'static str)>, caller_path: &'static str, caller: &'static str }

fn run(c: Case) {
    let mut base_files = vec![(c.lib_path, c.base_lib)]; base_files.extend(c.extra_base.iter().cloned());
    let base = snap(&base_files);
    let mut a = base.clone(); a.insert(c.lib_path.into(), c.changed_lib.as_bytes().to_vec());
    let mut b = base.clone(); b.insert(c.caller_path.into(), c.caller.as_bytes().to_vec());
    let w = analyze(&base, ("a", &a), ("b", &b));
    let h = high_hazards(&w);
    assert!(!h.is_empty(), "expected high HAZARD, got {w:?}");
    assert!(h.iter().all(|x| x.path == c.caller_path && x.changed_by.as_deref() == Some("a")));
    let mut a2 = base.clone(); a2.insert(c.lib_path.into(), c.logged_lib.as_bytes().to_vec());
    let w2 = analyze(&base, ("a", &a2), ("b", &b));
    assert!(high_hazards(&w2).is_empty(), "unexpected high HAZARD: {w2:?}");
}

fn merges(path: &str, base: &str, head: &str, sub: &str, must: &[&str]) {
    assert!(line_merge(base.as_bytes(), head.as_bytes(), sub.as_bytes()).is_err(), "line merge should conflict");
    let m = merge_file(path, Some(base.as_bytes()), Some(head.as_bytes()), Some(sub.as_bytes())).expect("MERGED").expect("content");
    let m = String::from_utf8(m).unwrap();
    for s in must { assert!(m.contains(s), "missing {s} in\n{m}"); }
}

fn conflicts(path: &str, base: &str, head: &str, sub: &str) {
    let r = merge_file(path, Some(base.as_bytes()), Some(head.as_bytes()), Some(sub.as_bytes()));
    assert!(matches!(r, Err(MergeError::Conflict(_))), "expected conflict, got {r:?}");
}

// ---------------- Go ----------------
const GO_BASE: &str = "package util\n\nimport \"fmt\"\n\n// Compute doubles x.\nfunc Compute(x int) int {\n\ty := x * 2\n\treturn y\n}\n\ntype Box struct {\n\tN int\n}\n\nfunc (b *Box) Size() int {\n\treturn b.N\n}\n\nfunc show() { fmt.Println(\"x\") }\n";

#[test]
fn go_hazard_and_no_hazard() {
    run(Case {
        lib_path: "pkg/util/util.go", base_lib: GO_BASE,
        changed_lib: &GO_BASE.replace("func Compute(x int) int {", "func Compute(x int, scale int) int {"),
        logged_lib: &GO_BASE.replace("\ty := x * 2\n", "\tfmt.Println(\"computing\")\n\ty := x * 2\n"),
        extra_base: vec![], caller_path: "pkg/util/report.go",
        caller: "package util\n\nfunc Report() int {\n\treturn Compute(3)\n}\n",
    });
    // Other package through the import alias; rename counts as removal.
    let base = snap(&[("pkg/util/util.go", GO_BASE)]);
    let mut a = base.clone(); a.insert("pkg/util/util.go".into(), GO_BASE.replace("Compute(", "Calc(").into_bytes());
    let mut b = base.clone(); b.insert("cmd/app/main.go".into(), b"package main\n\nimport \"example.com/proj/pkg/util\"\n\nfunc main() {\n\t_ = util.Compute(1)\n}\n".to_vec());
    assert!(!high_hazards(&analyze(&base, ("a", &a), ("b", &b))).is_empty());
}

#[test]
fn go_merge() {
    let h = format!("{GO_BASE}\nfunc Head() int {{\n\treturn 1\n}}\n");
    let s = format!("{GO_BASE}\nfunc Sub() int {{\n\treturn 2\n}}\n");
    merges("pkg/util/util.go", GO_BASE, &h, &s, &["func Head()", "func Sub()"]);
    let h = format!("{GO_BASE}\nfunc (b *Box) Head() int {{\n\treturn 1\n}}\n");
    let s = format!("{GO_BASE}\nfunc (b *Box) Sub() int {{\n\treturn 2\n}}\n");
    merges("pkg/util/util.go", GO_BASE, &h, &s, &["Head()", "Sub()"]);
    conflicts("pkg/util/util.go", GO_BASE, &GO_BASE.replace("x * 2", "x * 3"), &GO_BASE.replace("x * 2", "x * 4"));
}

// ---------------- Rust ----------------
const RS_BASE: &str = "use std::fmt;\n\n/// Doubles x.\npub fn compute(x: i32) -> i32 {\n    let y = x * 2;\n    y\n}\n\n#[derive(Debug)]\npub struct Boxed {\n    pub n: i32,\n}\n\nimpl Boxed {\n    pub fn size(&self) -> i32 {\n        self.n\n    }\n}\n\nimpl fmt::Display for Boxed {\n    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {\n        write!(f, \"{}\", self.n)\n    }\n}\n";

#[test]
fn rust_hazard_and_no_hazard() {
    run(Case {
        lib_path: "src/util.rs", base_lib: RS_BASE,
        changed_lib: &RS_BASE.replace("pub fn compute(x: i32) -> i32 {", "pub fn compute(x: i32, scale: i32) -> i32 {"),
        logged_lib: &RS_BASE.replace("    let y = x * 2;\n", "    println!(\"computing\");\n    let y = x * 2;\n"),
        extra_base: vec![("src/lib.rs", "pub mod util;\n")], caller_path: "src/report.rs",
        caller: "use crate::util::compute;\n\npub fn report() -> i32 {\n    compute(3)\n}\n",
    });
    // Path call and implicit-return change.
    let base = snap(&[("src/util.rs", RS_BASE)]);
    let mut a = base.clone(); a.insert("src/util.rs".into(), RS_BASE.replace("    y\n}", "    y + 1\n}").into_bytes());
    let mut b = base.clone(); b.insert("src/report.rs".into(), b"pub fn report() -> i32 {\n    crate::util::compute(3)\n}\n".to_vec());
    assert!(!high_hazards(&analyze(&base, ("a", &a), ("b", &b))).is_empty());
}

#[test]
fn rust_merge() {
    let h = format!("{RS_BASE}\npub fn head() -> i32 {{\n    1\n}}\n");
    let s = format!("{RS_BASE}\npub fn sub() -> i32 {{\n    2\n}}\n");
    merges("src/util.rs", RS_BASE, &h, &s, &["pub fn head()", "pub fn sub()"]);
    let anchor = "        self.n\n    }\n";
    let h = RS_BASE.replacen(anchor, &format!("{anchor}\n    pub fn head(&self) -> i32 {{\n        1\n    }}\n"), 1);
    let s = RS_BASE.replacen(anchor, &format!("{anchor}\n    pub fn sub(&self) -> i32 {{\n        2\n    }}\n"), 1);
    merges("src/util.rs", RS_BASE, &h, &s, &["pub fn head(&self)", "pub fn sub(&self)", "impl fmt::Display"]);
    conflicts("src/util.rs", RS_BASE, &RS_BASE.replace("x * 2", "x * 3"), &RS_BASE.replace("x * 2", "x * 4"));
}

// ---------------- Java ----------------
const JAVA_BASE: &str = "package com.acme.util;\n\nimport java.util.List;\n\npublic class Util {\n    private int n = 1;\n\n    public static int compute(int x) {\n        int y = x * 2;\n        return y;\n    }\n\n    public int size() {\n        return n;\n    }\n}\n";

#[test]
fn java_hazard_and_no_hazard() {
    let p = "src/main/java/com/acme/util/Util.java";
    run(Case {
        lib_path: p, base_lib: JAVA_BASE,
        changed_lib: &JAVA_BASE.replace("public static int compute(int x) {", "public static int compute(int x, int scale) {"),
        logged_lib: &JAVA_BASE.replace("        int y = x * 2;\n", "        System.out.println(\"computing\");\n        int y = x * 2;\n"),
        extra_base: vec![], caller_path: "src/main/java/com/acme/util/Report.java",
        caller: "package com.acme.util;\n\npublic class Report {\n    public int report() {\n        return Util.compute(3);\n    }\n}\n",
    });
    // Other package through an import.
    let base = snap(&[(p, JAVA_BASE)]);
    let mut a = base.clone(); a.insert(p.into(), JAVA_BASE.replace("compute(int x)", "calc(int x)").into_bytes());
    let mut b = base.clone(); b.insert("src/main/java/com/acme/app/App.java".into(),
        b"package com.acme.app;\n\nimport com.acme.util.Util;\n\npublic class App {\n    int go() { return Util.compute(1); }\n}\n".to_vec());
    assert!(!high_hazards(&analyze(&base, ("a", &a), ("b", &b))).is_empty());
}

#[test]
fn java_merge() {
    let p = "src/main/java/com/acme/util/Util.java";
    let anchor = "        return n;\n    }\n";
    let h = JAVA_BASE.replacen(anchor, &format!("{anchor}\n    public int head() {{\n        return 1;\n    }}\n"), 1);
    let s = JAVA_BASE.replacen(anchor, &format!("{anchor}\n    public int sub() {{\n        return 2;\n    }}\n"), 1);
    merges(p, JAVA_BASE, &h, &s, &["public int head()", "public int sub()"]);
    conflicts(p, JAVA_BASE, &JAVA_BASE.replace("x * 2", "x * 3"), &JAVA_BASE.replace("x * 2", "x * 4"));
}

// ---------------- C# ----------------
const CS_BASE: &str = "using System;\n\nnamespace Shop.Pricing\n{\n    public static class Vat\n    {\n        public const double Rate = 0.25;\n\n        public static double NetToGross(double net)\n        {\n            var gross = net * (1 + Rate);\n            return gross;\n        }\n\n        public static double Twice(this double x) => x * 2;\n    }\n}\n";
const CS_LIB: &str = "src/Shop.Pricing/Vat.cs";

fn cs_case(changed: &str, caller_path: &str, caller: &str) -> Vec<Warning> {
    let base = snap(&[(CS_LIB, CS_BASE)]);
    let mut a = base.clone(); a.insert(CS_LIB.into(), changed.as_bytes().to_vec());
    let mut b = base.clone(); b.insert(caller_path.into(), caller.as_bytes().to_vec());
    analyze(&base, ("a", &a), ("b", &b))
}

#[test]
fn csharp_hazard_and_no_hazard() {
    run(Case {
        lib_path: CS_LIB, base_lib: CS_BASE,
        changed_lib: &CS_BASE.replace("NetToGross(double net)", "NetToGross(double net, int scale)"),
        logged_lib: &CS_BASE.replace("            var gross", "            Console.WriteLine(\"computing\");\n            var gross"),
        extra_base: vec![], caller_path: "src/Shop.Pricing/Report.cs",
        caller: "namespace Shop.Pricing;\n\npublic class Report\n{\n    public double Total() => Vat.NetToGross(3.0);\n}\n",
    });
    // Another namespace through `using`; a rename counts as removal.
    let app = "using Shop.Pricing;\n\nnamespace Shop.App\n{\n    public class App\n    {\n        public double Go() { return Vat.NetToGross(1.0); }\n    }\n}\n";
    assert!(!high_hazards(&cs_case(&CS_BASE.replace("NetToGross(", "ToGross("), "src/Shop.App/App.cs", app)).is_empty());
    // A using alias, and a constant whose value changed.
    let aliased = "using V = Shop.Pricing.Vat;\n\nnamespace Shop.App;\n\npublic class Label\n{\n    public string Text() => $\"{V.Rate * 100}% VAT\";\n}\n";
    assert!(!high_hazards(&cs_case(&CS_BASE.replace("0.25", "0.12"), "src/Shop.App/Label.cs", aliased)).is_empty());
    // An expression-bodied extension method whose result changed, called on a value.
    let ext = "using Shop.Pricing;\n\nnamespace Shop.App;\n\npublic class Doubler\n{\n    public double Go(double v) => v.Twice();\n}\n";
    assert!(!high_hazards(&cs_case(&CS_BASE.replace("x * 2", "x * 3"), "src/Shop.App/Doubler.cs", ext)).is_empty());
    // Code that does not use the changed symbol is not warned about.
    let other = "namespace Shop.App;\n\npublic class Greeter\n{\n    public string Hi() => \"hi\";\n}\n";
    let w = cs_case(&CS_BASE.replace("NetToGross(double net)", "NetToGross(double net, int scale)"), "src/Shop.App/Greeter.cs", other);
    assert!(w.iter().all(|x| x.kind != "HAZARD"), "{w:?}");
}

#[test]
fn csharp_partial_class_property_and_record() {
    // A partial class split over two files: agent b's new member in the other part calls the changed method.
    let p1 = "namespace Shop.Orders;\n\npublic partial class Order\n{\n    public decimal Net { get; set; }\n\n    public decimal Total()\n    {\n        return Net * 1.25m;\n    }\n}\n";
    let p2 = "namespace Shop.Orders;\n\npublic partial class Order\n{\n    public string Id { get; set; } = \"\";\n}\n";
    let base = snap(&[("Shop.Orders/Order.cs", p1), ("Shop.Orders/Order.Display.cs", p2)]);
    let mut a = base.clone(); a.insert("Shop.Orders/Order.cs".into(), p1.replace("1.25m", "1.12m").into_bytes());
    let mut b = base.clone(); b.insert("Shop.Orders/Order.Display.cs".into(),
        p2.replace("= \"\";\n", "= \"\";\n\n    public string Label() => $\"{Id}: {Total()}\";\n").into_bytes());
    let w = analyze(&base, ("a", &a), ("b", &b));
    let h = high_hazards(&w);
    assert!(h.iter().any(|x| x.symbol.as_deref() == Some("Shop.Orders.Order.Total")), "{h:?}");
    // A property renamed; new code elsewhere reads it.
    let mut a = base.clone(); a.insert("Shop.Orders/Order.cs".into(), p1.replace("Net { get", "NetAmount { get").replace("return Net", "return NetAmount").into_bytes());
    let mut b = base.clone(); b.insert("Shop.Web/Show.cs".into(),
        b"using Shop.Orders;\n\nnamespace Shop.Web;\n\npublic static class Show\n{\n    public static decimal Net(Order o) => o.Net;\n}\n".to_vec());
    assert!(!high_hazards(&analyze(&base, ("a", &a), ("b", &b))).is_empty());
    // Record positional properties.
    let r = "namespace Shop.Orders;\n\npublic record Line(int Qty, decimal Price);\n";
    let base = snap(&[("Shop.Orders/Line.cs", r)]);
    let mut a = base.clone(); a.insert("Shop.Orders/Line.cs".into(), r.replace("int Qty", "int Quantity").into_bytes());
    let mut b = base.clone(); b.insert("Shop.Web/Sum.cs".into(),
        b"using Shop.Orders;\n\nnamespace Shop.Web;\n\npublic static class Sum\n{\n    public static int Of(Line l) => l.Qty;\n}\n".to_vec());
    assert!(!high_hazards(&analyze(&base, ("a", &a), ("b", &b))).is_empty());
}

#[test]
fn csharp_symbols_and_modules() {
    let info = gitta_core::analyze_file(CS_LIB, CS_BASE.as_bytes()).expect("parses");
    assert_eq!(info.module, "Shop.Pricing");
    let names: Vec<String> = gitta_core::provided_symbols(&snap(&[]), &snap(&[(CS_LIB, CS_BASE)])).into_iter().collect();
    for want in ["Shop.Pricing.Vat", "Shop.Pricing.Vat.NetToGross", "Shop.Pricing.Vat.Rate", "Shop.Pricing.Vat.Twice"] {
        assert!(names.contains(&want.to_string()), "{want} missing in {names:?}");
    }
    let nested = "namespace Shop\n{\n    namespace Pricing\n    {\n        public struct Money { public decimal Amount; }\n        public interface IRate { decimal Of(Money m); }\n        public enum Kind { Net, Gross }\n    }\n}\n";
    let names: Vec<String> = gitta_core::provided_symbols(&snap(&[]), &snap(&[("Shop/Money.cs", nested)])).into_iter().collect();
    for want in ["Shop.Pricing.Money.Amount", "Shop.Pricing.IRate.Of", "Shop.Pricing.Kind.Gross"] { assert!(names.contains(&want.to_string()), "{want} missing in {names:?}"); }
    assert_eq!(gitta_core::module_name("src/Shop.Pricing/Models/Vat.cs"), "Shop.Pricing.Models");
    let imports = gitta_core::imports_of("a.cs", b"using System.Text;\nusing static System.Math;\nusing M = Shop.Money;\nclass A {}\n");
    assert!(imports.contains("System.Text") && imports.contains("System.Math") && imports.contains("Shop"), "{imports:?}");
}

#[test]
fn csharp_merge() {
    let anchor = "            return gross;\n        }\n";
    let h = CS_BASE.replacen(anchor, &format!("{anchor}\n        public static int Head()\n        {{\n            return 1;\n        }}\n"), 1);
    let s = CS_BASE.replacen(anchor, &format!("{anchor}\n        public static int Sub()\n        {{\n            return 2;\n        }}\n"), 1);
    merges(CS_LIB, CS_BASE, &h, &s, &["public static int Head()", "public static int Sub()", "Twice"]);
    conflicts(CS_LIB, CS_BASE, &CS_BASE.replace("1 + Rate", "2 + Rate"), &CS_BASE.replace("1 + Rate", "3 + Rate"));
}

// ---------------- Kotlin ----------------
const KT_BASE: &str = "package shop.pricing\n\nconst val RATE = 0.25\n\nfun netToGross(net: Double): Double {\n    val gross = net * (1 + RATE)\n    return gross\n}\n\nfun Double.twice(): Double = this * 2\n\ndata class Line(val qty: Int, val price: Double)\n\nclass Cart(private val lines: List<Line>) {\n    fun total(): Double {\n        return lines.sumOf { it.qty * it.price }\n    }\n\n    companion object {\n        fun empty() = Cart(listOf())\n    }\n}\n";
const KT_LIB: &str = "src/main/kotlin/shop/pricing/Vat.kt";

fn kt_case(changed: &str, caller_path: &str, caller: &str) -> Vec<Warning> {
    let base = snap(&[(KT_LIB, KT_BASE)]);
    let mut a = base.clone(); a.insert(KT_LIB.into(), changed.as_bytes().to_vec());
    let mut b = base.clone(); b.insert(caller_path.into(), caller.as_bytes().to_vec());
    analyze(&base, ("a", &a), ("b", &b))
}

#[test]
fn kotlin_hazard_and_no_hazard() {
    run(Case {
        lib_path: KT_LIB, base_lib: KT_BASE,
        changed_lib: &KT_BASE.replace("fun netToGross(net: Double): Double {", "fun netToGross(net: Double, scale: Int): Double {"),
        logged_lib: &KT_BASE.replace("    val gross", "    println(\"computing\")\n    val gross"),
        extra_base: vec![], caller_path: "src/main/kotlin/shop/pricing/Report.kt",
        caller: "package shop.pricing\n\nfun report(): Double {\n    return netToGross(3.0)\n}\n",
    });
    // Another package through an aliased import; a rename counts as removal.
    let app = "package shop.app\n\nimport shop.pricing.netToGross as gross\n\nfun go() = gross(1.0)\n";
    assert!(!high_hazards(&kt_case(&KT_BASE.replace("netToGross(", "toGross("), "src/main/kotlin/shop/app/App.kt", app)).is_empty());
    // A top-level constant whose value changed, used in the same package.
    let label = "package shop.pricing\n\nfun label() = \"${RATE * 100}% VAT\"\n";
    assert!(!high_hazards(&kt_case(&KT_BASE.replace("0.25", "0.12"), "src/main/kotlin/shop/pricing/Label.kt", label)).is_empty());
    // An extension function whose result changed, called on a value.
    let ext = "package shop.app\n\nimport shop.pricing.twice\n\nfun doubled(v: Double) = v.twice()\n";
    assert!(!high_hazards(&kt_case(&KT_BASE.replace("this * 2", "this * 3"), "src/main/kotlin/shop/app/Ext.kt", ext)).is_empty());
    // A companion object function, reached through a wildcard import.
    let comp = "package shop.app\n\nimport shop.pricing.*\n\nfun start() = Cart.empty()\n";
    assert!(!high_hazards(&kt_case(&KT_BASE.replace("Cart(listOf())", "Cart(listOf(Line(1, 0.0)))"), "src/main/kotlin/shop/app/Start.kt", comp)).is_empty());
    // Code that does not use the changed symbol is not warned about.
    let other = "package shop.app\n\nfun hi() = \"hi\"\n";
    let w = kt_case(&KT_BASE.replace("fun netToGross(net: Double): Double {", "fun netToGross(net: Double, scale: Int): Double {"), "src/main/kotlin/shop/app/Hi.kt", other);
    assert!(w.iter().all(|x| x.kind != "HAZARD"), "{w:?}");
}

#[test]
fn kotlin_data_class_property_rename() {
    let user = "package shop.app\n\nimport shop.pricing.Line\n\nfun count(l: Line): Int {\n    return l.qty\n}\n";
    let w = kt_case(&KT_BASE.replace("val qty: Int", "val quantity: Int").replace("it.qty", "it.quantity"), "src/main/kotlin/shop/app/Count.kt", user);
    assert!(high_hazards(&w).iter().any(|x| x.symbol.as_deref() == Some("shop.pricing.Line.qty")), "{w:?}");
}

#[test]
fn kotlin_symbols_and_modules() {
    let info = gitta_core::analyze_file(KT_LIB, KT_BASE.as_bytes()).expect("parses");
    assert_eq!(info.module, "shop.pricing");
    let names: Vec<String> = gitta_core::provided_symbols(&snap(&[]), &snap(&[(KT_LIB, KT_BASE)])).into_iter().collect();
    for want in ["shop.pricing.RATE", "shop.pricing.netToGross", "shop.pricing.twice", "shop.pricing.Line.qty", "shop.pricing.Cart.total", "shop.pricing.Cart.empty"] {
        assert!(names.contains(&want.to_string()), "{want} missing in {names:?}");
    }
    let more = "package shop.pricing\n\nobject Registry {\n    val items = mutableListOf<Int>()\n}\n\ninterface Rate {\n    fun of(x: Double): Double\n}\n\nenum class Kind {\n    NET, GROSS\n}\n\ntypealias Price = Double\n";
    let names: Vec<String> = gitta_core::provided_symbols(&snap(&[]), &snap(&[("src/main/kotlin/shop/pricing/More.kt", more)])).into_iter().collect();
    for want in ["shop.pricing.Registry.items", "shop.pricing.Rate.of", "shop.pricing.Kind.GROSS", "shop.pricing.Price"] { assert!(names.contains(&want.to_string()), "{want} missing in {names:?}"); }
    assert_eq!(gitta_core::module_name("app/src/main/kotlin/shop/pricing/Vat.kt"), "shop.pricing");
    assert!(gitta_core::analyze_file("build.gradle.kts", b"plugins {\n    kotlin(\"jvm\") version \"2.0.0\"\n}\n\nval ver = \"1.0\"\n").is_some());
}

#[test]
fn kotlin_merge() {
    let anchor = "        return lines.sumOf { it.qty * it.price }\n    }\n";
    let h = KT_BASE.replacen(anchor, &format!("{anchor}\n    fun head(): Int {{\n        return 1\n    }}\n"), 1);
    let s = KT_BASE.replacen(anchor, &format!("{anchor}\n    fun sub(): Int {{\n        return 2\n    }}\n"), 1);
    merges(KT_LIB, KT_BASE, &h, &s, &["fun head()", "fun sub()", "companion object"]);
    conflicts(KT_LIB, KT_BASE, &KT_BASE.replace("1 + RATE", "2 + RATE"), &KT_BASE.replace("1 + RATE", "3 + RATE"));
}

#[test]
fn realistic_csharp_and_kotlin_files_parse() {
    let cs = "#nullable enable\nusing System.Collections.Generic;\nusing System.Linq;\n\nnamespace Shop.Orders;\n\n/// <summary>An order.</summary>\n[Serializable]\npublic sealed class Repo<T> : IRepo<T> where T : class\n{\n    #region fields\n    private readonly List<T> _items = new();\n    #endregion\n\n    public event EventHandler? Changed;\n\n    public T this[int i] => _items[i];\n\n    public async Task<int> CountAsync(CancellationToken ct = default)\n    {\n        await Task.Delay(1, ct);\n        return _items.Where(x => x != null).Count();\n    }\n\n    public static Repo<T> operator +(Repo<T> a, T b) { a._items.Add(b); return a; }\n}\n";
    let info = gitta_core::analyze_file("src/Shop.Orders/Repo.cs", cs.as_bytes()).expect("C# parses");
    assert_eq!(info.module, "Shop.Orders");
    assert!(info.items.keys().any(|k| format!("{k:?}").contains("CountAsync")), "{:?}", info.items.keys());
    let kt = "@file:JvmName(\"Orders\")\npackage shop.orders\n\nimport kotlinx.coroutines.flow.Flow\n\n@JvmInline\nvalue class Id(val raw: String)\n\nsealed interface Event {\n    data class Placed(val id: Id) : Event\n    object Cancelled : Event\n}\n\nsuspend fun handle(e: Event): String = when (e) {\n    is Event.Placed -> \"placed ${e.id.raw}\"\n    Event.Cancelled -> \"cancelled\"\n}\n\nfun <T : Comparable<T>> List<T>.top(n: Int = 3): List<T> =\n    sortedDescending().take(n)\n";
    let info = gitta_core::analyze_file("src/main/kotlin/shop/orders/Orders.kt", kt.as_bytes()).expect("Kotlin parses");
    assert_eq!(info.module, "shop.orders");
    assert!(info.items.values().any(|i| i.ext && i.text.contains("top(")));
}

#[test]
fn clean_overlaps_that_cannot_change_meaning_are_quiet() {
    use gitta_core::{quiet_overlap, Warning};
    let w = |path: &str, sym: Option<&str>| Warning { kind: "OVERLAP".into(), path: path.into(), symbol: sym.map(String::from), changed_by: None,
        referenced_by: None, severity: None, agents: vec!["a".into(), "b".into()], security: None };
    for p in ["README.md", "CHANGELOG.md", "docs/v2/intro.mdx", "docs/mint.json", "package.json", "package-lock.json", ".github/workflows/ci.yaml", ".gitignore"] {
        assert!(quiet_overlap(&w(p, None), b"x"), "{p}");
    }
    assert!(quiet_overlap(&w("internal/github/util_test.go", Some("def TestUserHasPushPermission")), b""));
    assert!(quiet_overlap(&w("tests/test_openai.py", Some("def test_simple")), b""));
    // Code outside tests keeps its warning: two clean edits of one object can add the same key twice.
    assert!(!quiet_overlap(&w("src/index.ts", Some("assign SCRAPE_TOOL")), b""));
    assert!(!quiet_overlap(&w("src/index.ts", None), b""));
    let imp = w("src/tools/builds.ts", Some("import import { apiVersion } from \"../utils.js\";"));
    assert!(quiet_overlap(&imp, b"import { apiVersion } from \"../utils.js\";\nx();\n"));
    assert!(!quiet_overlap(&imp, b"import { apiVersion } from \"../utils.js\";\nimport { apiVersion } from \"../utils.js\";\n"));
    assert!(!quiet_overlap(&Warning { kind: "HAZARD".into(), ..w("README.md", None) }, b""));
}

// ---------------- Object and dict literals ----------------
#[test]
fn insertions_at_the_same_place_in_code_conflict() {
    // (path, base, line both sides insert after, head's lines, submitted lines). Keeping both insertions would
    // write `audit();` (or `cache,`, or the WHERE clause) twice, so each must conflict, as in Git.
    let cases = [
        ("src/handler.ts", "export const handler = async () => {\n  start();\n  finish();\n};\n", "  start();\n", "  audit();\n", "  audit();\n  notify();\n"),
        ("src/routes.ts", "export const routes = {\n  home: () => {\n    start();\n    finish();\n  },\n};\n", "    start();\n", "    audit();\n", "    audit();\n    notify();\n"),
        ("src/deps.ts", "export const deps = {\n  db,\n};\n", "  db,\n", "  cache,\n", "  cache,\n  queue,\n"),
        ("app/query.py", "QUERY = \"\"\"\nSELECT {cols}\nFROM t\n\"\"\"\n", "FROM t\n", "WHERE a = 1\n", "WHERE a = 1\nORDER BY b\n"),
    ];
    for (path, base, at, h, s) in cases {
        conflicts(path, base, &base.replace(at, &format!("{at}{h}")), &base.replace(at, &format!("{at}{s}")));
    }
}

#[test]
fn keys_added_at_the_same_place_in_one_literal_combine() {
    let ts = "export const limits = {\n  retries: 3,\n};\n";
    let (h, s) = (ts.replace("3,\n", "3,\n  cache: {\n    ttl: 60,\n  },\n"), ts.replace("3,\n", "3,\n  backoff: 2,\n"));
    merges("src/limits.ts", ts, &h, &s, &["  retries: 3,\n  cache: {\n    ttl: 60,\n  },\n  backoff: 2,\n};\n"]);
    let class = "export class Client {\n  static limits = {\n    retries: 3,\n  };\n}\n";
    let (h, s) = (class.replace("3,\n", "3,\n    timeout: 5,\n"), class.replace("3,\n", "3,\n    backoff: 2,\n"));
    merges("src/client.ts", class, &h, &s, &["    timeout: 5,\n    backoff: 2,\n"]);
    let py = "LIMITS = {\n    \"retries\": 3,\n}\n";
    let (h, s) = (py.replace("3,\n", "3,\n    \"timeout\": 5,\n"), py.replace("3,\n", "3,\n    \"backoff\": 2,\n"));
    merges("app/limits.py", py, &h, &s, &["    \"timeout\": 5,\n    \"backoff\": 2,\n}\n"]);
    // The same key added with two values still conflicts.
    conflicts("src/limits.ts", ts, &ts.replace("3,\n", "3,\n  backoff: 1,\n"), &ts.replace("3,\n", "3,\n  backoff: 2,\n"));
}
