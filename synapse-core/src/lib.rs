//! Synapse engine — the tauri-free, fully-offline core.
//!
//! Everything the product does is a view over one append-only event stream.
//! This crate owns that stream and its projections; the desktop GUI (`src-tauri`)
//! and the CLI (`synapse-cli`) are thin front-ends over it.

pub mod agents;
pub mod events;
pub mod explain;
pub mod policy;
pub mod recap;
pub mod scan;
pub mod snapshots;
pub mod store;
pub mod timemachine;
pub mod twin;
pub mod watcher;
