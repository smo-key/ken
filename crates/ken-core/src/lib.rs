//! ken-core — Ken's domain library: project lifecycle, scanning, format
//! extraction, indexing, search, and file watching. Shared by the Tauri app
//! and the `ken-mcp` sidecar so both operate on the same data the same way.

pub mod chat;
pub mod db;
pub mod engine;
pub mod error;
pub mod extract;
pub mod hooks;
pub mod project;
pub mod pty_registry;
pub mod recipe;
pub mod refresh;
pub mod runner;
pub mod registry;
pub mod scan;
pub mod sync;
pub mod watch;

pub use error::{Error, Result};
