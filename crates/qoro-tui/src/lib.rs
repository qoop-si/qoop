//! The terminal interface: an index of terminals on the left, one of them on the right.
//!
//! Milestone 12. Everything that decides what appears on screen is a pure function of what
//! the daemon said, so the parts that matter can be tested without a screen, and the loop
//! that drives them is small enough to read in one sitting.

pub mod index;
pub mod keys;
pub mod run;
pub mod screen;

pub use run::run;
