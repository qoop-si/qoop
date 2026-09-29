//! The qoro daemon runtime.

#![cfg(unix)]

pub mod agents;
pub mod avatar;
pub mod blocking;
pub mod client;
pub mod config;
pub mod control;
pub mod daemon;
pub mod eventlog;
pub mod idle;
pub mod lock;
pub mod paths;
pub mod release;
pub mod spill;
pub mod store;
pub mod terminal;

pub use daemon::Daemon;
pub use lock::StartupLock;
pub use paths::{RUNTIME_DIR_ENV, runtime_dir, socket_path};
/// What a process group costs in CPU nanoseconds and resident bytes, for a client that
/// shows it: the same platform split the daemon's own process reads use.
pub use qoro_pty::sys::{ProcessUsage, group_processes};
pub use qoro_pty::teardown::Pgid;
