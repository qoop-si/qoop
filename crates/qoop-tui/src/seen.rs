//! When this interface tells the daemon a person has looked at an agent's finish.
//!
//! The daemon keeps one "done" per agent for every surface (`AgentRow::status`); it clears
//! when a person has seen it. Seen means the finish was on screen for a person: its terminal
//! is the one under the keyboard, the window is in front (a focus report says so; with none
//! ever sent, the person's own keys stand in), they pressed a key here in the last minute,
//! the screen is not locked, and all that held for a moment. Moving a selection or opening a
//! list is never a look. A client that cannot tell never says seen; the row then clears when
//! the person types in it.

use std::time::{Duration, Instant};

/// How long the finish has to stay in front of the person.
pub const HOLD: Duration = Duration::from_millis(1500);

/// How recent the person's last key has to be for them to be at this screen.
pub const INPUT_WINDOW: Duration = Duration::from_secs(60);

/// A finish under the keyboard, and since when it has been in front of the person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Look {
    pub terminal: u32,
    pub stop: String,
    pub since: Instant,
    pub told: bool,
}

/// Whether a person is in front of this interface now: its window not known to be behind
/// another, their key within [`INPUT_WINDOW`], and the screen unlocked.
pub fn present(focus: Option<bool>, keyed: Option<Instant>, now: Instant, locked: bool) -> bool {
    focus != Some(false)
        && keyed.is_some_and(|at| now.saturating_duration_since(at) < INPUT_WINDOW)
        && !locked
}

/// The next step for the finish now under the keyboard (`terminal` and its `stop`, or none),
/// given whether a person is in front: the look kept, started or dropped, and whether to tell
/// the daemon now. A look is told once.
pub fn step(
    look: &mut Option<Look>,
    finish: Option<(u32, &str)>,
    present: bool,
    now: Instant,
) -> bool {
    let Some((terminal, stop)) = finish.filter(|_| present) else {
        *look = None;
        return false;
    };
    match look {
        Some(l) if l.terminal == terminal && l.stop == stop => {
            if l.told || now.saturating_duration_since(l.since) < HOLD {
                return false;
            }
            l.told = true;
            true
        }
        _ => {
            *look = Some(Look {
                terminal,
                stop: stop.to_string(),
                since: now,
                told: false,
            });
            false
        }
    }
}

/// Whether the screen is locked. Only a local macOS interface can tell; an interface over
/// ssh is on another machine's screen, and reads unlocked (its keys still have to be recent).
pub fn locked() -> bool {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("SSH_CONNECTION").is_none() && mac::locked()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::{c_char, c_void};

    #[link(name = "CoreGraphics", kind = "framework")]
    // SAFETY: the signature is CoreGraphics' own (`CGSession.h`); it takes nothing and returns
    // a dictionary the caller owns, or null outside a login session.
    unsafe extern "C" {
        fn CGSessionCopyCurrentDictionary() -> *const c_void;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    // SAFETY: the signatures are CoreFoundation's own (`CFDictionary.h`, `CFString.h`,
    // `CFNumber.h`, `CFBase.h`).
    unsafe extern "C" {
        fn CFDictionaryGetValue(dict: *const c_void, key: *const c_void) -> *const c_void;
        fn CFStringCreateWithCString(
            alloc: *const c_void,
            text: *const c_char,
            encoding: u32,
        ) -> *const c_void;
        fn CFBooleanGetValue(boolean: *const c_void) -> u8;
        fn CFRelease(object: *const c_void);
    }

    const UTF8: u32 = 0x0800_0100;

    /// The session dictionary's `CGSSessionScreenIsLocked`: present and true while locked.
    pub fn locked() -> bool {
        // SAFETY: each object made here is released once, after its last use; the value read
        // out of the dictionary is borrowed from it and read before the dictionary is released;
        // a null from either call is checked before it is used.
        unsafe {
            let session = CGSessionCopyCurrentDictionary();
            if session.is_null() {
                return false;
            }
            let key = CFStringCreateWithCString(
                std::ptr::null(),
                c"CGSSessionScreenIsLocked".as_ptr(),
                UTF8,
            );
            let locked = !key.is_null() && {
                let value = CFDictionaryGetValue(session, key);
                !value.is_null() && CFBooleanGetValue(value) != 0
            };
            if !key.is_null() {
                CFRelease(key);
            }
            CFRelease(session);
            locked
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_person_is_in_front_only_with_a_recent_key_an_unhidden_window_and_no_lock() {
        let now = Instant::now() + INPUT_WINDOW;
        let recent = Some(now - Duration::from_secs(5));
        assert!(present(Some(true), recent, now, false));
        assert!(
            present(None, recent, now, false),
            "no focus report: the keys stand in"
        );
        assert!(
            !present(Some(false), recent, now, false),
            "another window in front"
        );
        assert!(!present(Some(true), None, now, false), "never a key here");
        assert!(
            !present(Some(true), Some(now - INPUT_WINDOW), now, false),
            "a minute ago"
        );
        assert!(!present(Some(true), recent, now, true), "locked");
    }

    #[test]
    fn a_finish_is_told_once_after_it_held() {
        let t0 = Instant::now();
        let mut look = None;
        assert!(
            !step(&mut look, Some((4, "1.1")), true, t0),
            "it just came on screen"
        );
        assert!(!step(&mut look, Some((4, "1.1")), true, t0 + HOLD / 2));
        assert!(
            step(&mut look, Some((4, "1.1")), true, t0 + HOLD),
            "held: told"
        );
        assert!(
            !step(&mut look, Some((4, "1.1")), true, t0 + HOLD * 3),
            "told once"
        );
        // A newer finish starts its own look.
        assert!(!step(&mut look, Some((4, "1.2")), true, t0 + HOLD * 3));
        assert!(step(&mut look, Some((4, "1.2")), true, t0 + HOLD * 4));
    }

    #[test]
    fn a_look_that_breaks_starts_over() {
        let t0 = Instant::now();
        let mut look = None;
        step(&mut look, Some((4, "1.1")), true, t0);
        // The person left for a moment: the hold starts again when they are back.
        assert!(!step(&mut look, Some((4, "1.1")), false, t0 + HOLD / 2));
        assert!(look.is_none());
        assert!(!step(&mut look, Some((4, "1.1")), true, t0 + HOLD));
        assert!(!step(
            &mut look,
            Some((4, "1.1")),
            true,
            t0 + HOLD * 2 - Duration::from_millis(1)
        ));
        assert!(step(&mut look, Some((4, "1.1")), true, t0 + HOLD * 2));
        // The keyboard moved to another tab: no look at the first.
        let mut look = None;
        step(&mut look, Some((4, "1.1")), true, t0);
        assert!(!step(&mut look, None, true, t0 + HOLD));
        assert!(look.is_none());
    }

    #[test]
    fn the_lock_probe_answers() {
        // Whatever the machine's state, the probe returns rather than crashing.
        let _ = locked();
    }
}
