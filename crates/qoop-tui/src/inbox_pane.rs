//! What the desktop's Inbox reads around the rows: qass and the hands to it, the computers not
//! reached, and when the person's day began. Shared helpers; the terminal interface shows qass's
//! questions on its own screen (`qass_screen`) and each agent's on its pane (`tabq`).

use std::collections::BTreeSet;

use qoop_runtime::control::{AgentRow, AskInfo, WorkspaceRow};

use crate::answers::HandInput;
use crate::inbox::Context;
use crate::mux::machine_of;

/// What the client knows about qass and the hands to it, read off its lists: This Mac's qass
/// (the first agent in its space), its switch, the machines that refused a hand as too old, the
/// remote machines' names, this Mac's hand id (read once from `hand.dir`, when a hand from or to
/// another machine can matter), and the hands noted at `now`. Idempotent: called with every new list and before
/// every frame.
pub fn update_hand(
    hand: &mut HandInput,
    spaces: &[WorkspaceRow],
    rows: &[AgentRow],
    cards: &[AskInfo],
    no_hands: &BTreeSet<usize>,
    watching: Option<bool>,
    now: i64,
) {
    let qass_spaces: BTreeSet<i64> = spaces
        .iter()
        .filter(|w| w.name == crate::sidebar::QASS_SPACE && machine_of(w.id) == 0)
        .map(|w| w.id)
        .collect();
    hand.qass = rows
        .iter()
        .find(|r| r.agent && qass_spaces.contains(&r.workspace))
        .map(|r| (r.terminal, r.generation));
    hand.watching = watching;
    hand.no_hands = no_hands.clone();
    hand.machines = spaces
        .iter()
        .filter(|w| machine_of(w.id) != 0)
        .filter_map(|w| {
            let (machine, _) = w.name.split_once(':')?;
            Some((machine_of(w.id), machine.to_string()))
        })
        .collect();
    if hand.hand_id.is_none()
        && rows
            .iter()
            .any(|r| r.qass_away.is_some() || machine_of(r.workspace) != 0)
    {
        hand.hand_id = hand
            .dir
            .as_deref()
            .and_then(|dir| qoop_runtime::paths::hand_id_in(dir).ok());
    }
    hand.set_carded(cards);
    crate::answers::note_qass(hand, rows, now);
}

/// The computers the link to is down, by their machine numbers.
pub fn lost_machines(spaces: &[WorkspaceRow]) -> BTreeSet<usize> {
    spaces
        .iter()
        .filter(|w| w.link == Some(qoop_runtime::control::LinkState::Down))
        .map(|w| machine_of(w.id))
        .collect()
}

/// What the Inbox needs around the rows: when the person's day began, the machines not
/// reached, the machines whose qoop sends no menu, qass's spaces.
pub fn context(spaces: &[WorkspaceRow], day_start: i64, old: BTreeSet<usize>) -> Context {
    Context {
        day_start,
        lost: lost_machines(spaces),
        old,
        qass_spaces: spaces
            .iter()
            .filter(|w| w.name == crate::sidebar::QASS_SPACE)
            .map(|w| w.id)
            .collect(),
    }
}

/// When the person's local day began, kept in `cache` while `now` is inside it: a day is never
/// shorter than 23 hours, so the start is looked up again only after that (and when the clock
/// is set back).
pub fn day_start(cache: &mut i64, now: i64) -> i64 {
    if now < *cache || now - *cache >= 23 * 3600 {
        let zone = qoop_runtime::schedule::system_zone();
        let wall = (zone.local)(now);
        *cache = qoop_runtime::schedule::instant_of(
            zone,
            qoop_runtime::schedule::Naive::new(wall.year, wall.month, wall.day, 0, 0),
        );
    }
    *cache
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_start_of_the_day_is_looked_up_only_when_the_day_may_have_changed() {
        let mut cache = 0;
        let now = 1_795_400_000;
        let start = day_start(&mut cache, now);
        assert!(start <= now && now - start < 25 * 3600, "{start}");
        // Inside the day: the kept value, whatever is in the cache.
        let mut kept = now - 3600;
        assert_eq!(day_start(&mut kept, now), now - 3600);
        // A day on, or the clock set back: looked up again.
        let mut old = now - 24 * 3600;
        assert_eq!(day_start(&mut old, now), start);
        let mut ahead = now + 3600;
        assert_eq!(day_start(&mut ahead, now), start);
    }
}
