//! Consumer-side verification of attested Trusted Server releases.
//!
//! Everything here is private to the CLI. The embedded consumer policy and the
//! compiled constants in [`policy`] are the only verifier authority; downloaded
//! release metadata, operator configuration and forwarded gh flags never
//! redefine them.

mod bounds;
mod documents;
mod environment;
mod error;
mod gh;
mod policy;
mod process;
mod source_policy;

#[cfg(test)]
mod tests;
