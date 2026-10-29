//! Shared building blocks for all Flixnet crates: configuration, version info and,
//! in later milestones, ids, the event bus and the job runtime.

pub mod auth;
pub mod config;
pub mod events;
pub mod jobs;
pub mod maintenance;
pub mod scheduler;

pub use config::Config;

/// Product name reported by the API.
pub const NAME: &str = "Flixnet";

/// Server version (shared workspace version).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Current public API version; all routes live under `/api/{API_VERSION}`.
pub const API_VERSION: &str = "v1";
