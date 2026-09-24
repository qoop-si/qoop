//! The qoro daemon runtime.

#![cfg(unix)]

pub mod agents;
pub mod avatar;
pub mod blocking;
pub mod client;
pub mod control;
pub mod daemon;
pub mod eventlog;
pub mod lock;
pub mod paths;
pub mod spill;
pub mod store;
pub mod terminal;

pub use daemon::Daemon;
pub use lock::StartupLock;
pub use paths::{RUNTIME_DIR_ENV, runtime_dir, socket_path};
