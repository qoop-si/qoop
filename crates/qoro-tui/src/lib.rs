//! The terminal interface: spaces and agents down the left, tabs across the top, and
//! the focused terminal filling the rest.
//!
//! Milestone 12. Everything that decides what appears on screen is a pure function of what
//! the daemon said, so the parts that matter can be tested without a screen, and the loop
//! that drives them is small enough to read in one sitting.

pub mod keys;
pub mod palette;
pub mod run;
pub mod screen;
pub mod selection;
pub mod sidebar;
pub mod tabs;
pub mod theme;
pub mod width;

pub use run::{run, run_with_notice};
