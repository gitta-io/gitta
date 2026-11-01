// Summaries: what the team server receives instead of code. A summary sent as JSON gives the same warnings as
// the snapshots it came from, and holds names and paths only.
use gitta_core::{analyze, analyze_summaries, summarize, Snapshot, Summary};

fn snap(files: &[(&str, &str)]) -> Snapshot { files.iter().map(|(p, t)| (p.to_string(), t.as_bytes().to_vec())).collect() }

const PRICING: &str = "VAT_RATE = 0.25\n\n\ndef net_to_gross(net):\n    return round(net * (1 + VAT_RATE), 2)\n";

#[test]
fn summaries_over_json_give_the_same_warnings_and_no_code() {
    let base = snap(&[("shop/pricing.py", PRICING)]);
    let mut a = base.clone();
    a.insert("shop/pricing.py".into(), PRICING.replace("return round(net * (1 + VAT_RATE), 2)", "return int(net * (1 + VAT_RATE) * 100)").into_bytes());
    let mut b = base.clone();
    b.insert("shop/label.py".into(), b"from shop.pricing import net_to_gross\n\n\ndef label(x):\n    return f\"{net_to_gross(x)} kr\"\n".to_vec());

    let direct = analyze(&base, ("a", &a), ("b", &b));
    let wire = |s: Summary| -> Summary { serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap() };
    let (sa, sb) = (wire(summarize(&base, &a)), wire(summarize(&base, &b)));
    let fmt = |w: &[gitta_core::Warning]| serde_json::to_string(w).unwrap();
    assert_eq!(fmt(&analyze_summaries(("a", &sa), ("b", &sb))), fmt(&direct));
    assert!(direct.iter().any(|w| w.kind == "HAZARD" && w.path == "shop/label.py"), "{direct:?}");

    let json = serde_json::to_string(&sa).unwrap() + &serde_json::to_string(&sb).unwrap();
    for code in ["return int(", "round(net", "(1 + VAT_RATE)", "def label", "import net_to_gross", " kr"] {
        assert!(!json.contains(code), "summary holds source text {code:?}: {json}");
    }
}
