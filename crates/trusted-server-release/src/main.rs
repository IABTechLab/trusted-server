// Task 2 utilities remain private until Task 3 wires authenticated preflight.
#[allow(dead_code)]
mod documents;
mod error;

use error::ReleaseError;
use error_stack::Report;

fn main() -> Result<(), Report<ReleaseError>> {
    Err(Report::new(ReleaseError::Pending))
}
