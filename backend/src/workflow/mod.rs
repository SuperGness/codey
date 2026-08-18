//! Durable workflow orchestration for Codey.
//!
//! The module deliberately keeps SQLite behind synchronous `Journal` calls. Async
//! callers use [`engine::WorkflowService`], which moves every database operation onto the
//! blocking pool and never holds a SQLite connection across an `.await`.

pub mod app_server;
pub mod artifacts;
pub mod domain;
pub mod engine;
pub mod host;
pub mod journal;
pub mod policy;
pub mod proxy_client;
pub mod recovery;
pub mod scheduler;

pub use host::WorkflowHost;
