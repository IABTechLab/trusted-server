//! Platform-neutral request handling for Trusted Server.
//!
//! Adapter crates supply stores, outbound HTTP, geo data, and other runtime
//! services through [`platform::RuntimeServices`]. This crate owns routing,
//! policy, integrations, and response transformation without depending on a
//! specific edge provider.
//!
//! # Modules
//!
//! - Auction planning and execution: [`auction`], [`auction_config_types`].
//! - Authentication and signing: [`auth`], [`request_signing`].
//! - Configuration loading and validation: [`config`], [`config_payload`],
//!   [`secret_resolution`], [`settings`], [`settings_data`].
//! - Cache and privacy policy: [`cache_policy`], [`response_privacy`].
//! - Consent, cookies, and identity: [`consent`], [`consent_config`],
//!   [`cookies`], [`ec`], [`tester_cookie`].
//! - Creative selection and rendering: [`creative`], [`creative_opportunities`],
//!   [`price_bucket`].
//! - Request and client context: [`constants`], [`geo`], [`host_header`],
//!   [`http_util`], [`models`].
//! - Integration registry and browser bundles: [`integrations`], [`tsjs`].
//! - `OpenRTB` transport types: [`openrtb`].
//! - Platform service contracts and persistence: [`platform`], [`storage`].
//! - Publisher and first-party proxy routes: [`proxy`], [`publisher`].
//! - Response transformation: [`html_processor`], [`rsc_flight`],
//!   [`streaming_processor`], [`streaming_replacer`].
//! - Shared support types and test helpers: [`error`], [`redacted`],
//!   [`test_support`].

#![cfg_attr(
    test,
    allow(
        clippy::print_stdout,
        clippy::print_stderr,
        clippy::panic,
        clippy::dbg_macro,
        clippy::unwrap_used,
        reason = "tests use direct diagnostics and panic-on-failure helpers"
    )
)]

pub(crate) mod asset_image_optimizer;
pub mod auction;
pub mod auction_config_types;
pub mod auth;
pub mod cache_policy;
pub mod config;
pub mod config_payload;
pub mod consent;
pub mod consent_config;
pub mod constants;
pub mod cookies;
pub mod creative;
pub mod creative_opportunities;
pub mod ec;
pub(crate) mod edge_cookie;
pub mod error;
pub mod geo;
pub mod host_header;
pub(crate) mod host_rewrite;
pub mod html_processor;
pub mod http_util;
pub mod integrations;
pub mod models;
pub mod openrtb;
pub mod platform;
pub mod price_bucket;
pub mod proxy;
pub mod publisher;
pub mod redacted;
pub mod request_signing;
pub mod response_privacy;
pub mod rsc_flight;
pub(crate) mod s3_sigv4;
pub mod secret_resolution;
pub mod settings;
pub mod settings_data;
pub mod storage;
pub mod streaming_processor;
pub mod streaming_replacer;
pub mod test_support;
pub mod tester_cookie;
pub mod tsjs;

#[cfg(test)]
mod migration_guards;
