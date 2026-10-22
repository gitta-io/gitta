//! The in-process line merge must agree with `git merge-file`: when both merge, the bytes are identical;
//! the in-process merge may only be MORE conservative (conflict where Git merged), never merge where Git conflicts.
use gitta_core::merge::{diff3, line_merge};

fn rng(seed: &mut u64) -> u64 { *seed ^= *seed << 13; *seed ^= *seed >> 7; *seed ^= *seed << 17; *seed }

fn mutate(base: &[String], seed: &mut u64) -> Vec<String> {
    let mut v = base.to_vec();
    for _ in 0..(rng(seed) % 3 + 1) {
        let n = v.len();
        let at = if n == 0 { 0 } else { (rng(seed) % (n as u64 + 1)) as usize };
        match rng(seed) % 3 {
            0 => v.insert(at.min(n), format!("new{}\n", rng(seed) % 1000)),
            1 if n > 0 => { v.remove(at.min(n - 1)); }
            _ if n > 0 => { let i = at.min(n - 1); v[i] = format!("chg{}\n", rng(seed) % 1000); }
            _ => {}
        }
    }
    v
}

#[test]
fn agrees_with_git_merge_file() {
    let mut seed = 0x9E3779B97F4A7C15u64;
    let (mut both, mut conservative, mut git_conflicts) = (0, 0, 0);
    for _ in 0..3000 {
        let n = (rng(&mut seed) % 12) as usize;
        let base: Vec<String> = (0..n).map(|i| format!("line{}\n", i)).collect();
        let (h, s) = (mutate(&base, &mut seed), mutate(&base, &mut seed));
        let j = |v: &[String]| v.concat().into_bytes();
        let (b, h, s) = (j(&base), j(&h), j(&s));
        let ours = diff3(&b, &h, &s, false);
        let git = line_merge(&b, &h, &s);
        match (&ours, &git) {
            (Ok(a), Ok(g)) => { assert_eq!(a, g, "different merge result"); both += 1; }
            (Err(_), Ok(_)) => conservative += 1,
            (Ok(a), Err(_)) => panic!("merged where git conflicts:\nbase {:?}\nhead {:?}\nsub {:?}\nours {:?}", String::from_utf8_lossy(&b), String::from_utf8_lossy(&h), String::from_utf8_lossy(&s), String::from_utf8_lossy(a)),
            (Err(_), Err(_)) => git_conflicts += 1,
        }
    }
    eprintln!("identical merges {both}, more conservative {conservative}, both conflict {git_conflicts}");
    assert!(conservative * 20 < both, "too conservative: {conservative} vs {both}");
}
