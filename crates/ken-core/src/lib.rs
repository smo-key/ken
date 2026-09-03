//! ken-core — Ken's domain library: project lifecycle, scanning, format
//! extraction, indexing, search, and file watching. Shared by the Tauri app
//! and the `ken-mcp` sidecar so both operate on the same data the same way.

pub mod assistant;
pub mod automation;
pub mod bg_hydrate;
pub mod chat;
pub mod chunker;
pub mod cloud;
pub mod db;
pub mod digest;
pub mod embedder;
pub mod engine;
pub mod error;
pub mod extract;
pub mod family;
pub mod family_sync;
pub mod features;
pub mod federation;
pub mod fsops;
pub mod hooks;
pub mod import;
pub mod kenignore;
pub mod knowledge_model;
pub mod local_llm;
pub mod memory;
pub mod model;
pub mod ocr;
pub mod pipeline;
pub mod proc;
pub mod profiler;
pub mod project;
pub mod record;
pub mod pty_registry;
pub mod recipe;
pub mod refresh;
pub mod research;
pub mod runner;
pub mod registry;
pub mod routing;
pub mod scan;
pub mod search;
pub mod settings;
pub mod sync;
pub mod tasks;
pub mod transcript;
pub mod user_state;
pub mod watch;
pub mod workspace;
pub mod workspace_digest;
pub mod workspace_kg_db;

pub use error::{Error, Result};
