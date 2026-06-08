//! Common functionality for the trusted server.
//!
//! This crate provides shared types, utilities, and abstractions used by both
//! the Fastly edge implementation and local development/testing environments.
//!
//! # Modules
//!
//! - [`auth`]: Basic authentication enforcement helpers
//! - [`constants`]: Application-wide constants and configuration values
//! - [`cookies`]: Cookie parsing and generation utilities
//! - [`error`]: Error types and error handling utilities
//! - [`consent`]: Consent signal extraction and logging
//! - [`geo`]: Geographic location utilities and DMA code extraction
//! - [`models`]: Data models for ad serving and callbacks
//! - [`integrations::prebid`]: Prebid integration and real-time bidding support
//! - [`settings`]: Configuration management and validation
//! - [`streaming_replacer`]: Streaming URL replacement for large responses
//! - [`ec`]: Edge Cookie (EC) identity subsystem — ID generation, consent gating, lifecycle
//! - [`test_support`]: Testing utilities and mocks

#![cfg_attr(
    test,
    allow(
        clippy::print_stdout,
        clippy::print_stderr,
        clippy::panic,
        clippy::dbg_macro,
        clippy::unwrap_used,
    )
)]

pub(crate) mod asset_image_optimizer;
pub mod auction;
pub mod auction_config_types;
pub mod auth;
pub mod backend;
#[doc(hidden)]
pub mod compat;
pub mod consent;
pub mod consent_config;
pub mod constants;
pub mod cookies;
pub mod creative;
pub mod ec;
pub mod error;
pub mod geo;
pub(crate) mod host_header;
pub(crate) mod host_rewrite;
pub mod html_processor;
pub mod http_util;
pub mod integrations;
pub mod models;
pub mod openrtb;
pub mod platform;
pub mod proxy;
pub mod publisher;
pub mod redacted;
pub mod request_signing;
pub mod rsc_flight;
pub(crate) mod s3_sigv4;
pub mod settings;
pub mod settings_data;
pub mod storage;
pub mod streaming_processor;
pub mod streaming_replacer;
pub mod test_support;
pub mod tsjs;

#[cfg(test)]
mod migration_guards;
