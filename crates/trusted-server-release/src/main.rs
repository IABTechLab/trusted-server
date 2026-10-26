// These reviewed primitives have no release command until their callers land.
#[allow(dead_code, reason = "release command assembly is staged in Phase 2")]
mod documents;
mod error;
#[allow(dead_code, reason = "source closure caller is staged in Phase 2")]
mod git_objects;
#[allow(dead_code, reason = "ordinary adapter command assembly is pending")]
mod ordinary;
#[allow(dead_code, reason = "confined execution callers are staged in Phase 2")]
mod process;

use std::io::Write as _;
use std::process::ExitCode;

fn main() -> ExitCode {
    let _ = writeln!(std::io::stderr(), "{}", error::ReleaseError::Usage);
    ExitCode::FAILURE
}
