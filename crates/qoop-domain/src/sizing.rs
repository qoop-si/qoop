//! PTY sizing and ownership.
//!
//! Invariant I6. A resize that takes no client identity lets the
//! last caller win, so a phone attaching at 50 columns reflows the desktop user's shell.
//! Here the only way to ask for a resize is to hand `SizePolicy` a `SizeRequest`, and it
//! answers with a verdict rather than performing an ioctl, so the arbitration cannot be
//! bypassed by a call site that forgot about it.

use crate::ids::ClientId;

/// Smallest size a terminal may be set to. Below this, most applications misbehave and a
/// zero would make the grid arithmetic degenerate.
pub const MIN_COLS: u16 = 2;
pub const MIN_ROWS: u16 = 1;
/// Largest size. The ceiling comes from memory: the engine allocates a fixed 1000 row
/// scrollback chunk, so cost is roughly `1000 * cols * 25` bytes, and 1000 columns is
/// already 25 MiB for one terminal.
pub const MAX_COLS: u16 = 1000;
pub const MAX_ROWS: u16 = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PtySize {
    pub cols: u16,
    pub rows: u16,
}

impl PtySize {
    pub fn in_range(self) -> bool {
        (MIN_COLS..=MAX_COLS).contains(&self.cols) && (MIN_ROWS..=MAX_ROWS).contains(&self.rows)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SizePolicy {
    /// The default. The first writable attacher owns the size.
    Owner(ClientId),
    /// Nobody owns it. Set when the owner leaves and no writable client remains.
    Unowned,
    /// Headless and CI.
    Fixed(PtySize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SizeRequest {
    pub from: ClientId,
    pub writable: bool,
    pub size: PtySize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SizeVerdict {
    /// Apply this to the PTY.
    Apply(PtySize),
    /// The requester is not the owner. A typed refusal rather than silence, so a client can
    /// tell "you are not the owner" from "your message was lost".
    NotOwner,
    /// Read-only clients can never resize, under any policy.
    ObserverMayNotResize,
    /// The policy pins the size.
    PolicyFixed,
    /// Outside the supported range. Bounded here, where the verdict is produced, rather
    /// than at the ioctl: a client can ask for 65535 columns, and the memory cost of a
    /// terminal is roughly linear in its width.
    OutOfRange(PtySize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnershipEvent {
    /// The owner's connection ended. Ownership moves, because a dead owner would otherwise
    /// strand the geometry.
    OwnerDisconnected,
    /// A writable client attached.
    WritableAttached(ClientId),
    /// The current owner explicitly agreed to hand over. There is deliberately no
    /// timeout-based variant: a silent owner keeps ownership, because every same-uid
    /// process has full protocol authority and a timer would let any of them reflow a
    /// human's shell by asking while the desktop client was briefly busy.
    OwnerAgreedTransfer(ClientId),
}

impl SizePolicy {
    pub fn evaluate(&self, req: &SizeRequest) -> SizeVerdict {
        if !req.writable {
            return SizeVerdict::ObserverMayNotResize;
        }
        if !req.size.in_range() {
            return SizeVerdict::OutOfRange(req.size);
        }
        match self {
            SizePolicy::Fixed(_) => SizeVerdict::PolicyFixed,
            SizePolicy::Unowned => SizeVerdict::NotOwner,
            SizePolicy::Owner(owner) if *owner == req.from => SizeVerdict::Apply(req.size),
            SizePolicy::Owner(_) => SizeVerdict::NotOwner,
        }
    }

    /// Ownership transitions. The absence of a timeout arm is the point of this function.
    #[must_use]
    pub fn apply(self, event: OwnershipEvent, longest_attached_writable: Option<ClientId>) -> Self {
        match (self, event) {
            (SizePolicy::Fixed(s), _) => SizePolicy::Fixed(s),
            (SizePolicy::Owner(_), OwnershipEvent::OwnerDisconnected) => {
                match longest_attached_writable {
                    Some(next) => SizePolicy::Owner(next),
                    None => SizePolicy::Unowned,
                }
            }
            (SizePolicy::Unowned, OwnershipEvent::WritableAttached(c)) => SizePolicy::Owner(c),
            (SizePolicy::Owner(o), OwnershipEvent::WritableAttached(_)) => SizePolicy::Owner(o),
            (SizePolicy::Owner(_), OwnershipEvent::OwnerAgreedTransfer(to)) => {
                SizePolicy::Owner(to)
            }
            (SizePolicy::Unowned, OwnershipEvent::OwnerAgreedTransfer(to)) => SizePolicy::Owner(to),
            (SizePolicy::Unowned, OwnershipEvent::OwnerDisconnected) => SizePolicy::Unowned,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(n: &str) -> ClientId {
        ClientId::new(n)
    }
    const BIG: PtySize = PtySize {
        cols: 180,
        rows: 50,
    };
    const SMALL: PtySize = PtySize { cols: 50, rows: 20 };

    #[test]
    fn a_smaller_non_owner_cannot_resize() {
        // A small client must not reflow a large one's shell, as a unit test rather than a hope.
        let policy = SizePolicy::Owner(c("desktop"));
        let verdict = policy.evaluate(&SizeRequest {
            from: c("phone"),
            writable: true,
            size: SMALL,
        });
        assert_eq!(verdict, SizeVerdict::NotOwner);
    }

    #[test]
    fn the_owner_can_resize() {
        let policy = SizePolicy::Owner(c("desktop"));
        assert_eq!(
            policy.evaluate(&SizeRequest {
                from: c("desktop"),
                writable: true,
                size: BIG
            }),
            SizeVerdict::Apply(BIG)
        );
    }

    #[test]
    fn an_observer_can_never_resize_even_if_it_is_the_owner() {
        let policy = SizePolicy::Owner(c("desktop"));
        assert_eq!(
            policy.evaluate(&SizeRequest {
                from: c("desktop"),
                writable: false,
                size: SMALL
            }),
            SizeVerdict::ObserverMayNotResize
        );
    }

    #[test]
    fn a_silent_owner_keeps_ownership() {
        // Gate G2.13. There is no event that expresses "the owner did not answer", so the
        // property is enforced by the absence of an arm rather than by a rule someone has
        // to remember.
        let policy = SizePolicy::Owner(c("desktop"));
        let after = policy
            .clone()
            .apply(OwnershipEvent::WritableAttached(c("agent")), None);
        assert_eq!(after, policy);
    }

    #[test]
    fn a_size_outside_the_supported_range_is_refused_even_from_the_owner() {
        let policy = SizePolicy::Owner(c("desktop"));
        for size in [
            PtySize { cols: 0, rows: 24 },
            PtySize { cols: 80, rows: 0 },
            PtySize {
                cols: u16::MAX,
                rows: 24,
            },
            PtySize {
                cols: 80,
                rows: u16::MAX,
            },
        ] {
            assert_eq!(
                policy.evaluate(&SizeRequest {
                    from: c("desktop"),
                    writable: true,
                    size
                }),
                SizeVerdict::OutOfRange(size),
                "{size:?} must be refused"
            );
        }
    }

    #[test]
    fn ownership_moves_when_the_owner_disconnects() {
        let policy = SizePolicy::Owner(c("desktop"));
        assert_eq!(
            policy
                .clone()
                .apply(OwnershipEvent::OwnerDisconnected, Some(c("second"))),
            SizePolicy::Owner(c("second"))
        );
        assert_eq!(
            policy.apply(OwnershipEvent::OwnerDisconnected, None),
            SizePolicy::Unowned
        );
    }

    #[test]
    fn an_unowned_terminal_is_claimed_by_the_next_writable_client() {
        assert_eq!(
            SizePolicy::Unowned.apply(OwnershipEvent::WritableAttached(c("a")), None),
            SizePolicy::Owner(c("a"))
        );
    }
}
