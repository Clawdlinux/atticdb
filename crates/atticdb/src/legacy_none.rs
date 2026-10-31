//! The rename guards where zerodb never ran. zerodb was Unix only, so no old home, variable
//! or app entry can be in the way here. Each guard passes, and `migrate` has nothing to do.

use crate::connect::Harness;
use std::path::Path;
use std::process::Command;

/// Passes: no old variable has a meaning here.
pub fn check_env() -> Result<(), String> {
    Ok(())
}

/// Leaves the child's environment as is.
pub(crate) fn clear_env(command: &mut Command) {
    let _ = command;
}

/// Passes: no zerodb data can be in the home's way.
pub(crate) fn check_home(dir: &Path, default: bool) -> Result<(), String> {
    let _ = (dir, default);
    Ok(())
}

/// Passes for every command.
pub fn guard(command: &str) -> Result<(), String> {
    let _ = command;
    Ok(())
}

/// No zerodb parts to report.
pub(crate) fn found_lines(harness: Harness) -> Vec<String> {
    let _ = harness;
    Vec::new()
}

/// `atticdb migrate`: says there is nothing to move and returns 0.
pub fn migrate(plan: bool) -> u8 {
    let _ = plan;
    println!("atticdb migrate: nothing to migrate on Windows; zerodb never ran here.");
    0
}
