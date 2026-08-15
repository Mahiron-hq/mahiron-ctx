//! Core engine and interface layers for `mahiron-ctx`.
//!
//! The engine (`engine`) owns every decision about what is discovered, filtered,
//! classified, transformed and composed. The command-line interface (`cli`) and the
//! MCP server (`mcp`) are two consumers of that engine and hold no packaging logic
//! of their own.

pub mod cleanup;
pub mod cli;
pub mod compress;
pub mod config;
pub mod delivery;
pub mod engine;
pub mod error;
pub mod mcp;
pub mod output;
pub mod paths;
pub mod remote;
pub mod report;
pub mod tokens;

#[cfg(feature = "watch")]
pub mod watch;

pub use error::{Error, Result};

/// Version of the structural schema used by the machine-parsable output formats.
///
/// Independent of the crate's own release version: it changes only when the shape of
/// the XML/JSON documents changes, and a non-additive change to that shape is a
/// breaking change to this value.
pub const OUTPUT_SCHEMA_VERSION: &str = "1.0";

/// Release version of the tool itself, as published through every distribution channel.
pub const TOOL_VERSION: &str = env!("CARGO_PKG_VERSION");
