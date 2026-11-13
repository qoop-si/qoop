//! The hooks the measured runs use. Built without the `bench` feature every one is empty
//! and `Frame` holds nothing, so the product path carries none of the measuring.

#[cfg(feature = "bench")]
pub use crate::bench::{Frame, note, painted};

#[cfg(not(feature = "bench"))]
mod off {
    /// A frame's CPU clock, when measuring.
    pub struct Frame;

    impl Frame {
        #[inline]
        pub fn start() -> Self {
            Frame
        }
        #[inline]
        pub fn pause(&mut self) {}
        #[inline]
        pub fn resume(&mut self) {}
        #[inline]
        pub fn cpu_us(&self) -> u64 {
            0
        }
    }

    /// A pane finished painting.
    #[inline]
    pub fn painted(_frame: Frame, _pane: usize, _shown: bool, _window: &mut gpui::Window) {}

    /// A line for the measurement log; the closure is never called.
    #[inline]
    pub fn note(_line: impl FnOnce() -> serde_json::Value) {}
}

#[cfg(not(feature = "bench"))]
pub use off::{Frame, note, painted};
