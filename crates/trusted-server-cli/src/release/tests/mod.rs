//! Private release verification tests, registered explicitly so focused runs
//! (`release::tests::documents`, `release::tests::gh`,
//! `release::tests::verification`) select real cases.

mod documents;
mod gh;
mod verification;

mod offline;
mod online;
