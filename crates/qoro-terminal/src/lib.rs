//! Terminal state, behind a seam.

#![forbid(unsafe_code)]

mod alacritty;
pub mod control_string;
pub mod engine;
pub mod keyboard;

pub use alacritty::{AlacrittyEngine, SCROLLBACK_ROWS};
pub use control_string::{ControlStringFilter, MAX_CONTROL_STRING};
pub use engine::{Damage, LineDamage, Snapshot, TerminalEffect, TerminalEngine, TerminalModes};
pub use keyboard::{Decoded, KeyCode, KeyEvent, KeyKind, Modifiers};
