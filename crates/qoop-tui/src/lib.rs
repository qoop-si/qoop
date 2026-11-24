//! The terminal interface: spaces and agents down the left, tabs across the top, and
//! the focused terminal filling the rest.
//!
//! Milestone 12. Everything that decides what appears on screen is a pure function of what
//! the daemon said, so the parts that matter can be tested without a screen, and the loop
//! that drives them is small enough to read in one sitting.

pub mod answer;
pub mod answers;
pub mod asks;
pub mod board;
pub mod chat_view;
pub mod colorlevel;
pub mod config;
pub mod copy;
pub mod graphics;
pub mod hyperlink;
pub mod icons;
pub mod inbox;
pub mod inbox_pane;
pub mod input;
pub mod keys;
pub mod layout;
pub mod ledger;
pub mod link;
pub mod microphone;
pub mod mux;
pub mod notify;
pub mod palette;
pub mod picture;
pub mod places;
pub mod plugins;
pub mod present;
pub mod qass_screen;
pub mod rail;
pub mod run;
pub mod schedules;
pub mod screen;
pub mod seen;
pub mod selection;
pub mod sidebar;
pub mod tabq;
pub mod tabs;
pub mod theme;
pub mod width;

pub use run::{run, run_stream, run_with_notice};
