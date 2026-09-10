//! Where a byte stream's escape parser is, carried from one chunk into the next.
//!
//! Both directions need this. Input needs it to split a paste without cutting a
//! sequence in half; output needs it to bound a control string before it reaches the
//! parser. One scanner, so the two directions cannot disagree about what a sequence is.

/// Where a byte stream's escape parser is, carried from one chunk into the next.
///
/// The first version scanned backwards from a candidate boundary with a 64 byte lookback,
/// which is a bounded-lookback design, and a bounded lookback can desync on a long
/// sequence. A resumable scanner has no ceiling to be wrong about: the state at the
/// end of chunk N is the state at the start of chunk N+1, and a boundary is safe exactly
/// when that state is `Ground`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EscapeState {
    #[default]
    Ground,
    /// Saw ESC, waiting for the next byte to decide what kind of sequence this is.
    Escape,
    /// CSI: parameters until a final byte in 0x40 to 0x7e.
    Csi,
    /// A string sequence (OSC, DCS, APC, PM, SOS), terminated by ST or, for OSC, BEL.
    ///
    /// There is no "saw ESC" flag any more. ESC leaves this state from the transition
    /// table above, so `ESC \` ends the string by way of `Escape` rather than by a flag
    /// that had to be kept in step with it.
    String { bel_terminates: bool },
    /// A two byte sequence, for example ESC ( B. One more byte to go.
    TwoByte,
}

impl EscapeState {
    /// Advance the scanner over one byte.
    pub fn step(self, b: u8) -> Self {
        // Three bytes mean the same thing in every state, and leaving them out was not a
        // simplification, it was a hole. CAN and SUB cancel whatever sequence is in
        // progress; ESC abandons it and starts a new one. A scanner that treated ESC as an
        // ordinary parameter byte believed it was at ground after `ESC [ 1 ESC ]` while the
        // parser behind it was inside an OSC, so every bound the scanner is responsible for
        // was skipped by a three byte prefix. Measured: 4 MiB fed, 4 MiB retained, and the
        // discard counter still reading zero.
        //
        // The other direction loses output rather than memory: a CAN inside an OSC returns
        // the parser to ground while the scanner stays in `String`, so ordinary text is
        // discarded as if it were a control string until a stray terminator appears.
        match b {
            0x18 | 0x1a => return EscapeState::Ground,
            0x1b => return EscapeState::Escape,
            _ => {}
        }
        match self {
            EscapeState::Ground => EscapeState::Ground,
            EscapeState::Escape => match b {
                b'[' => EscapeState::Csi,
                b']' => EscapeState::String {
                    bel_terminates: true,
                },
                b'P' | b'_' | b'^' | b'X' => EscapeState::String {
                    bel_terminates: false,
                },
                // ESC ( , ESC ) , ESC # and friends take one more byte.
                b'(' | b')' | b'*' | b'+' | b'#' | b' ' | b'%' => EscapeState::TwoByte,
                _ => EscapeState::Ground,
            },
            EscapeState::Csi => {
                if (0x40..=0x7e).contains(&b) {
                    EscapeState::Ground
                } else {
                    EscapeState::Csi
                }
            }
            EscapeState::String { bel_terminates } => {
                if bel_terminates && b == 0x07 {
                    EscapeState::Ground
                } else {
                    EscapeState::String { bel_terminates }
                }
            }
            EscapeState::TwoByte => EscapeState::Ground,
        }
    }

    pub fn is_ground(self) -> bool {
        matches!(self, EscapeState::Ground)
    }
}
