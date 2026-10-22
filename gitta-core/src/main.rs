//! gitta-core CLI. `gitta-core analyze` reads JSON on stdin:
//!   {"base": {path: text}, "a": {path: text}, "b": {path: text}, "names": ["a", "b"]}
//! and prints the warnings as a JSON array. Paths absent from a side are deleted on that side;
//! only changed paths need to be included (unchanged files may be omitted from all three).
use gitta_core::{analyze, Snapshot};
use std::io::Read;

fn snap(v: &serde_json::Value) -> Snapshot {
    v.as_object().map(|m| m.iter().map(|(k, t)| (k.clone(), t.as_str().unwrap_or("").as_bytes().to_vec())).collect()).unwrap_or_default()
}

fn main() {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).expect("stdin");
    let req: serde_json::Value = serde_json::from_str(&input).expect("json");
    if cmd == "merge" {
        // {"path": p, "base": text|null, "head": text|null, "submitted": text|null}
        let g = |k: &str| req[k].as_str();
        println!("{}", gitta_core::merge::merge_json(g("path").unwrap_or(""), g("base"), g("head"), g("submitted")));
        return;
    }
    if cmd == "merge-snapshots" {
        // {"base": {path: text}, "head": {...}, "submitted": {...}}; absent path = absent/deleted on that side.
        let t0 = std::time::Instant::now();
        let r = gitta_core::merge::merge_snapshots(&snap(&req["base"]), &snap(&req["head"]), &snap(&req["submitted"]));
        let us = t0.elapsed().as_micros() as u64;
        println!("{}", match r {
            Ok(m) => serde_json::json!({"status": "MERGED", "micros": us, "files": m.iter().map(|(k, v)| (k.clone(), serde_json::Value::String(String::from_utf8_lossy(v).into_owned()))).collect::<serde_json::Map<String, serde_json::Value>>()}),
            Err((st, code)) => serde_json::json!({"status": st, "code": code, "micros": us}),
        });
        return;
    }
    if cmd != "analyze" { eprintln!("usage: gitta-core analyze|merge < request.json"); std::process::exit(2); }
    let names: Vec<String> = req["names"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or(vec!["a".into(), "b".into()]);
    let (base, a, b) = (snap(&req["base"]), snap(&req["a"]), snap(&req["b"]));
    // GITTA_BENCH=N: repeat the analysis N times (profiling aid).
    let reps: usize = std::env::var("GITTA_BENCH").ok().and_then(|x| x.parse().ok()).unwrap_or(1);
    for _ in 1..reps { let _ = analyze(&base, (&names[0], &a), (&names[1], &b)); }
    let w = analyze(&base, (&names[0], &a), (&names[1], &b));
    println!("{}", serde_json::to_string(&w).unwrap());
}
