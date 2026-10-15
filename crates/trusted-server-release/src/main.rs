// Document, policy and environment utilities stay unwired until source
// authentication lets preflight dispatch them.
#[allow(dead_code)]
mod documents;
mod error;
#[allow(dead_code)]
mod policy;
#[allow(dead_code)]
mod preflight;

use error::ReleaseError;
use error_stack::Report;

fn main() -> Result<(), Report<ReleaseError>> {
    Err(Report::new(ReleaseError::Pending))
}
