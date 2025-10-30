//! Common functionality for the trusted server.
//!
//! This crate provides shared types, utilities, and abstractions used by both
//! the Fastly edge implementation and local development/testing environments.
//!
//! # Modules
//!
//! - [`auth`]: Basic authentication enforcement helpers
//! - [`advertiser`]: Ad serving and advertiser integration functionality
//! - [`constants`]: Application-wide constants and configuration values
//! - [`cookies`]: Cookie parsing and generation utilities
//! - [`error`]: Error types and error handling utilities
//! - [`gdpr`]: GDPR consent management and TCF string parsing
//! - [`geo`]: Geographic location utilities and DMA code extraction
//! - [`models`]: Data models for ad serving and callbacks
//! - [`prebid`]: Prebid integration and real-time bidding support
//! - [`prebid_proxy`]: Prebid Server proxy for first-party ad serving
//! - [`privacy`]: Privacy utilities and helpers
//! - [`settings`]: Configuration management and validation
//! - [`streaming_replacer`]: Streaming URL replacement for large responses
//! - [`synthetic`]: Synthetic ID generation using HMAC
//! - [`templates`]: Handlebars template handling
//! - [`test_support`]: Testing utilities and mocks
//! - [`why`]: Debugging and introspection utilities

pub mod ad;
pub mod auth;
pub mod backend;
pub mod constants;
pub mod cookies;
pub mod creative;
pub mod error;
pub mod geo;
pub mod html_processor;
pub mod http_util;
pub mod models;
pub mod openrtb;
pub mod prebid_proxy;
pub mod proxy;
pub mod publisher;
pub mod settings;
pub mod settings_data;
pub mod streaming_processor;
pub mod streaming_replacer;
pub mod synthetic;
pub mod test_support;
pub mod tsjs;
