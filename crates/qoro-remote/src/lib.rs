//! Reaching a daemon on another machine.
//!
//! ADR-010. Shell out to the system `ssh`, behind a trait so that ssh, a direct connection
//! and a future relay never leak into anything above this. Two decisions in that sentence
//! are load bearing.
//!
//! The system `ssh`, not a library, because the user's own config is the thing that makes
//! their jump hosts, their keys, their `Match` blocks and their agent forwarding work, and
//! reimplementing that is a project. The cost is that failures arrive as text on stderr,
//! which is why text matching here is a hint layer and never a branch.
//!
//! And version skew is gated on the protocol codec generation, never on a version string. A
//! remote whose codec is compatible is not restarted by a client update, because restarting
//! it kills the agents running in it, and "your agents restarted because you updated your
//! laptop" is not an acceptable consequence of a routine update.

#![forbid(unsafe_code)]

pub mod bootstrap;
pub mod ssh;
pub mod transport;

pub use bootstrap::{Bootstrap, BootstrapError, RemoteShell, marker_command};
pub use ssh::SshTransport;
pub use transport::{LocalTransport, RemoteTransport, TransportError};

use serde::{Deserialize, Serialize};

/// What a remote daemon says about itself when asked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteHello {
    /// The application version. Shown to people, never used to decide anything.
    pub version: String,
    /// What the compatibility decision is actually made on.
    pub codec_generation: u32,
    pub platform: String,
}

/// Why a remote could not be used.
///
/// Typed, and asserted by type rather than by message (gate G6.4). A caller that matched on
/// a message would break the first time somebody improved the wording, and a caller that
/// could not tell these apart would retry the ones that cannot succeed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RemoteError {
    #[error("the remote daemon speaks codec generation {found}, and {required} is required")]
    CodecTooOld { found: u32, required: u32 },
    #[error("the remote daemon speaks codec generation {found}, which is newer than {supported}")]
    CodecTooNew { found: u32, supported: u32 },
    #[error("no daemon is installed on the remote")]
    NotInstalled,
    #[error("the remote answered something that is not a daemon: {0}")]
    NotADaemon(String),
    #[error("the transport failed: {0}")]
    Transport(String),
}

/// Whether this remote can be used as it is, restarted, or not used at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compatibility {
    /// Use it. Its running children are not touched.
    AsIs,
    /// Below the floor. It has to be restarted, and its children will not survive that.
    NeedsRestart,
}

/// The oldest codec generation this build can talk to.
pub const CODEC_FLOOR: u32 = qoro_proto::CODEC_GENERATION;

/// Decide on the codec generation, never on the version string.
///
/// A newer remote is refused rather than restarted: it is somebody else's daemon, possibly
/// with somebody else's agents in it, and downgrading it is not a decision a client update
/// gets to make.
pub fn compatibility(hello: &RemoteHello) -> Result<Compatibility, RemoteError> {
    if hello.codec_generation > CODEC_FLOOR {
        return Err(RemoteError::CodecTooNew {
            found: hello.codec_generation,
            supported: CODEC_FLOOR,
        });
    }
    if hello.codec_generation < CODEC_FLOOR {
        return Ok(Compatibility::NeedsRestart);
    }
    Ok(Compatibility::AsIs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello(generation: u32) -> RemoteHello {
        RemoteHello {
            version: "9.9.9".into(),
            codec_generation: generation,
            platform: "linux".into(),
        }
    }

    /// G6.2: a compatible remote is not restarted, however different its version string is.
    #[test]
    fn a_compatible_remote_is_left_alone_whatever_its_version_says() {
        let mut h = hello(CODEC_FLOOR);
        h.version = "0.0.1-ancient".into();
        assert_eq!(compatibility(&h), Ok(Compatibility::AsIs));
        h.version = "99.0.0-from-the-future".into();
        assert_eq!(compatibility(&h), Ok(Compatibility::AsIs));
    }

    #[test]
    fn a_remote_below_the_floor_is_restarted() {
        assert_eq!(
            compatibility(&hello(CODEC_FLOOR - 1)),
            Ok(Compatibility::NeedsRestart)
        );
    }

    /// A newer remote is refused, not downgraded. It may have somebody else's agents in it.
    #[test]
    fn a_newer_remote_is_refused_rather_than_restarted() {
        assert_eq!(
            compatibility(&hello(CODEC_FLOOR + 1)),
            Err(RemoteError::CodecTooNew {
                found: CODEC_FLOOR + 1,
                supported: CODEC_FLOOR
            })
        );
    }

    /// G6.4: the error is asserted by type, not by message.
    #[test]
    fn version_mismatch_is_a_typed_error() {
        let err = compatibility(&hello(CODEC_FLOOR + 1)).expect_err("refused");
        assert!(matches!(err, RemoteError::CodecTooNew { .. }));
        // And the message is free to change without breaking a caller.
        assert!(!format!("{err}").is_empty());
    }
}
