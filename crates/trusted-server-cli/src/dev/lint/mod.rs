//! `ts dev lint` subcommand group: linters for source/config/docs.
//!
//! Subcommands:
//! - `domains`: URL-host linter (this design).

pub mod domains;

#[cfg(test)]
pub(crate) mod test_support;
