//! Common functionality for the trusted server.
//!
//! This crate provides shared types, utilities, and abstractions used by both
//! the Fastly edge implementation and local development/testing environments.
//!
//! # Modules
//!
//! - [`constants`]: Application-wide constants and configuration values
//! - [`cookies`]: Cookie parsing and generation utilities
//! - [`error`]: Error types and error handling utilities
//! - [`gdpr`]: GDPR consent management and TCF string parsing
//! - [`models`]: Data models for ad serving and callbacks
//! - [`prebid`]: Prebid integration and real-time bidding support
//! - [`privacy`]: Privacy utilities and helpers
//! - [`settings`]: Configuration management and validation
//! - [`synthetic`]: Synthetic ID generation using HMAC
//! - [`templates`]: Handlebars template handling
//! - [`test_support`]: Testing utilities and mocks
//! - [`why`]: Debugging and introspection utilities

pub mod constants;
pub mod cookies;
pub mod error;
pub mod gdpr;
pub mod models;
pub mod prebid;
pub mod privacy;
pub mod settings;
pub mod synthetic;
pub mod templates;
pub mod test_support;
pub mod why;
