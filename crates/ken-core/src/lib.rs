//! ken-core — Ken's domain library: project lifecycle, scanning, format
//! extraction, indexing, search, and file watching. Shared by the Tauri app
//! and the `ken-mcp` sidecar so both operate on the same data the same way.

pub mod db;
pub mod error;
pub mod extract;
pub mod project;
pub mod registry;
pub mod scan;
pub mod watch;

pub use error::{Error, Result};
