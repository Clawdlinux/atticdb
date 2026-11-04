#![cfg(feature = "sim")]
#![allow(clippy::unwrap_used)]

#[test]
// Dev dependencies also enable sim. Debug schedules stay opt-in with --ignored.
#[cfg_attr(debug_assertions, ignore = "slow in debug")]
fn seeded_crash_recovery() {
    let var = |name| std::env::var(name).ok().map(|s| s.parse::<u64>().unwrap());
    let first = var("ATTICDB_SIM_SEED");
    let count = var("ATTICDB_SIM_SEEDS").unwrap_or(if first.is_some() { 1 } else { 500 });
    let first = first.unwrap_or(0);
    assert!(count > 0, "ATTICDB_SIM_SEEDS must be positive");
    let threads = atticdb_tape::sim::sim_threads();
    let swept =
        atticdb_tape::sim::try_each_seed(first, count, threads, atticdb_tape::sim::run_seed);
    if let Err((seed, error)) = swept {
        eprintln!(
            "simulation seed={seed}\nrepro: ATTICDB_SIM_SEED={seed} cargo test -p atticdb-tape --release --features sim --test sim"
        );
        std::panic::resume_unwind(error);
    }
    eprintln!("simulation passed: {count} seeds starting at {first} on {threads} threads");
}
