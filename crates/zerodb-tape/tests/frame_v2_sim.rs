#[test]
fn v2_fault_matrix_is_exercised_for_each_seed() {
    for seed in 0..64 {
        zerodb_tape::sim::frame_v2_faults(seed);
    }
}
