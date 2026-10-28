//! Common functionality for the trusted server.
//!
//! This crate provides shared types, utilities, and abstractions used by every
//! adapter, on edge platforms and in local development and testing.
//!
//! # Modules
//!
//! - [`constants`]: Application-wide constants and configuration values
//! - [`cookies`]: Cookie parsing and generation utilities
//! - [`error`]: Error types and error handling utilities
//! - [`consent`]: The consent context the permission signal modules fill
//! - [`geo`]: Geographic location utilities and DMA code extraction
//! - [`settings`]: Configuration management and validation
//! - [`streaming_replacer`]: Streaming URL replacement for large responses
//! - [`ec`]: Edge Cookie (EC) identity subsystem — ID generation, consent gating, lifecycle
//! - [`test_support`]: Testing utilities and mocks
//! - [`tester_cookie`]: Optional tester-cookie endpoint helpers

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

pub mod addressing;
pub mod asset_cache;
pub(crate) mod asset_image_optimizer;
pub mod auction;
pub mod auction_config_types;
pub mod cache_policy;
pub mod closed_paths;
pub mod config;
pub mod config_payload;
pub mod consent;
pub mod consent_config;
pub mod constants;
pub mod cookies;
pub mod creative;
pub mod creative_opportunities;
pub mod ec;
pub mod edge_cookie;
pub mod error;
pub mod evidence;
pub mod geo;
pub mod host_header;
pub mod host_rewrite;
pub mod html_processor;
pub mod http_util;
pub mod inspect;
pub mod integrations;
pub mod middleware;
pub mod module_context;
pub mod module_name;
pub(crate) mod module_secrets;
pub mod origin_auth;
pub mod permission_signal;
pub mod permissions;
pub mod platform;
pub mod prefix;
pub mod price_bucket;
pub mod provider_table;
pub mod proxy;
pub mod publisher;
pub mod redacted;
pub mod request_signing;
pub(crate) mod response_header_rewrite;
pub mod response_privacy;
pub mod robots_txt;
pub mod secret_resolution;
pub mod service_version;
pub mod settings;
pub mod settings_data;
pub mod source;
pub mod streaming_processor;
pub mod streaming_replacer;
pub mod tdl;
pub mod test_support;
pub mod tester_cookie;
pub mod tsjs;
pub mod tsjs_bundle;
