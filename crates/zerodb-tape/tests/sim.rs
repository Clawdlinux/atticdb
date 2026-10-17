#![allow(clippy::unwrap_used)]

use zerodb_tape::sim;

#[test]
fn seeded_crash_recovery() {
    let first = std::env::var("ZERODB_SIM_SEED").map_or(0, |s| s.parse::<u64>().unwrap());
    let count = std::env::var("ZERODB_SIM_SEEDS").map_or_else(
        |_| if std::env::var_os("ZERODB_SIM_SEED").is_some() { 1 } else { 500 },
        |s| s.parse::<u64>().unwrap(),
    );
    assert!(count > 0, "ZERODB_SIM_SEEDS must be positive");
    for offset in 0..count {
        let seed = first.wrapping_add(offset);
        if let Err(error) = std::panic::catch_unwind(|| sim::run_seed(seed)) {
            eprintln!(
                "simulation seed={seed}\nrepro: ZERODB_SIM_SEED={seed} cargo test --features sim --test sim -- --nocapture"
            );
            std::panic::resume_unwind(error);
        }
    }
    eprintln!("simulation passed: {count} seeds starting at {first}");
}
