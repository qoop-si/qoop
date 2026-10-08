//! PTY spawning, containment and teardown.
//!
//! Unix only for v1. Windows is out of scope until ADR-021 exists with its own spike, and
//! nothing here pretends otherwise.

#![cfg(unix)]

pub mod procwatch;
pub mod sys;
pub mod teardown;
pub mod writeq;

pub use teardown::{Containment, Member, Pgid, Pid, Signal, Step};
pub use writeq::{WritePriority, WriteQueue, WriteQueueError, chunk_paste};
