//! Terminal state, behind a seam.

#![forbid(unsafe_code)]

mod alacritty;
pub mod control_string;
pub mod engine;
pub mod images;
pub mod keyboard;

pub use alacritty::{AlacrittyEngine, SCROLLBACK_ROWS};
pub use control_string::{ControlStringFilter, MAX_CONTROL_STRING};
pub use engine::{
    Colour, Damage, LineDamage, Snapshot, Span, Style, TerminalEffect, TerminalEngine,
    TerminalModes,
};
pub use images::{CapturedImage, IMAGE_DEADLINE, ImageProtocol, ImageSniffer, MAX_IMAGE_BYTES};
pub use keyboard::{Decoded, KeyCode, KeyEvent, KeyKind, Modifiers};
