//! Pure SQL parsing, quoting, validation, and keyword definitions for tursotui.
//!
//! This crate has zero runtime dependencies — no database, no async, no UI.
//! All functions are pure and deterministic.

pub mod keywords;
pub mod parser;
pub mod query_kind;
pub mod quoting;
pub mod validation;
