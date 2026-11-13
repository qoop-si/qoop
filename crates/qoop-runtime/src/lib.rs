//! The qoop daemon runtime.

#![cfg(unix)]
// `say!`, never `eprintln!`, which panics on a full disk (`say`, `clippy.toml`).
#![deny(clippy::disallowed_macros)]

pub mod agent_view;
pub mod agents;
pub mod asks;
pub mod avatar;
pub mod blocking;
pub mod changes;
mod claims;
pub mod client;
pub mod config;
pub mod control;
pub mod daemon;
pub mod handoff;
pub mod idle;
pub mod legacy;
pub mod lock;
pub mod menu;
pub mod messages;
pub mod microphone;
pub mod paths;
pub mod release;
pub mod say;
pub mod schedule;
mod schedules;
mod sessions;
pub mod shows;
pub mod spill;
pub mod state;
pub mod store;
mod tasks;
pub mod telemetry;
pub mod terminal;
mod tunnel;

pub use daemon::Daemon;
pub use lock::StartupLock;
pub use paths::{RUNTIME_DIR_ENV, runtime_dir, socket_path};
/// What a process group costs in CPU nanoseconds and resident bytes, for a client that
/// shows it: the same platform split the daemon's own process reads use.
pub use qoop_pty::sys::{ProcessUsage, group_processes};
pub use qoop_pty::teardown::Pgid;
