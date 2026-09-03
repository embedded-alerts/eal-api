//! Shared library surface for Embedded Alerts API binaries and contract tests.

pub mod alert_store;
pub mod alerts;
#[rustfmt::skip]
pub mod auth;
pub mod error;
pub mod indexing;
pub mod migrations;
pub mod query_embedding;
#[rustfmt::skip]
pub mod store;
pub mod tenant;
pub mod worker_auth;
