//! The recurring claim scan. qass sweeps the done claims at its Start, but one stranded after
//! that, in a qass conversation that lives for days, was never looked at again (33 open claims,
//! p50 99.7 h, on 2026-10-28). So while watching is on, every [`SCAN_EVERY`] the daemon lists
//! the claims older than [`STRANDED_AFTER`] that nothing has picked up, and leaves that one
//! line in qass's folder ([`CLAIMS_FILE`]). The watcher says it, as it says every other event,
//! and qass runs "Done, checked" on each. Nothing here closes a claim or turns watching on.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use qoop_store::TaskRow;

use crate::store::{StoreHandle, now};

/// How often the scan runs while watching is on.
const SCAN_EVERY: i64 = 4 * 3600;
/// How old a claim is before nobody looking at it counts as stranded: under a day it is still
/// the watcher's first claim line's to settle.
const STRANDED_AFTER: i64 = 24 * 3600;
/// How often the loop looks whether a scan is due.
const LOOK_EVERY: Duration = Duration::from_secs(5 * 60);

/// The line waiting for the watcher, in qass's folder; watch.sh says it and takes it away.
const CLAIMS_FILE: &str = "claims";
/// When the last scan ran, in qass's folder: a daemon restart (every install) does not scan
/// again before its time.
const SCANNED_FILE: &str = "claims-scanned";

/// Whether a scan is due at `now`: watching on, and none for [`SCAN_EVERY`]. While watching is
/// off nothing runs and the clock keeps the last scan.
fn scan_due(last: Option<i64>, now: i64, watching: bool) -> bool {
    watching && last.is_none_or(|at| now - at >= SCAN_EVERY)
}

/// The newest write qass made to each terminal of this Mac, from `prompted.log` records
/// (`<unix> <from terminal> T<n>`) sent from one of `qass`'s terminals. A remote's
/// (`<name>/T<n>`) is not a terminal here.
fn qass_writes(log: &str, qass: &BTreeSet<u32>) -> BTreeMap<i64, i64> {
    let mut newest = BTreeMap::new();
    for line in log.lines() {
        let mut w = line.split_whitespace();
        let (Some(at), Some(from), Some(to)) = (w.next(), w.next(), w.next()) else {
            continue;
        };
        let (Ok(at), Ok(from)) = (at.parse::<i64>(), from.parse::<u32>()) else {
            continue;
        };
        let Some(to) = to.strip_prefix('T').and_then(|t| t.parse::<i64>().ok()) else {
            continue;
        };
        if qass.contains(&from) {
            let last = newest.entry(to).or_insert(at);
            *last = (*last).max(at);
        }
    }
    newest
}

/// The open claims older than [`STRANDED_AFTER`] at `now` with no disposition, oldest first. A
/// claim has one when it is closed, a live card names its task (`carded`), or qass wrote to its
/// terminal after the claim (`written`, from [`qass_writes`]). Any one settles it.
fn stranded<'a>(
    tasks: &'a [TaskRow],
    carded: &BTreeSet<i64>,
    written: &BTreeMap<i64, i64>,
    now: i64,
) -> Vec<&'a TaskRow> {
    let mut found: Vec<&TaskRow> = tasks
        .iter()
        .filter(|t| t.closed_at.is_none() && t.outcome.is_none())
        .filter(|t| t.claimed_at.is_some_and(|at| now - at > STRANDED_AFTER))
        .filter(|t| !carded.contains(&t.id))
        .filter(|t| {
            let claimed = t.claimed_at.unwrap_or_default();
            t.terminal
                .and_then(|n| written.get(&n))
                .is_none_or(|at| *at <= claimed)
        })
        .collect();
    found.sort_by_key(|t| (t.claimed_at, t.id));
    found
}

/// The one line qass is told, or none when nothing is stranded.
fn claims_line(found: &[&TaskRow]) -> Option<String> {
    if found.is_empty() {
        return None;
    }
    let ids: Vec<String> = found.iter().map(|t| t.id.to_string()).collect();
    Some(format!(
        "claims {}: done claims over 24 h old with no check, card or prompt since; run \"Done, \
         checked\" for each (qass skill)",
        ids.join(" ")
    ))
}

/// What the scan reads from the store and qass's folder, then the line it gives. Tabs the
/// owner asked qass to leave alone are left out, as the watcher leaves them out. `Err` when the
/// store could not be read: no line is not "nothing stranded" then.
async fn scan(
    store: &StoreHandle,
    dir: &std::path::Path,
    now: i64,
) -> Result<Option<String>, String> {
    let space = crate::asks::qass_space(store).await;
    let (tasks, carded, (skip_spaces, skip_tabs), qass) = store
        .run(move |s| {
            let carded: BTreeSet<i64> = s.live_asks()?.into_iter().filter_map(|a| a.task).collect();
            let qass: BTreeSet<u32> = match space {
                Some(space) => s
                    .open_terminals(space)?
                    .into_iter()
                    .filter_map(|t| u32::try_from(t.id).ok())
                    .collect(),
                None => BTreeSet::new(),
            };
            Ok((s.open_tasks()?, carded, s.qass_skips()?, qass))
        })
        .await
        .map_err(|e| e.to_string())?;
    let tasks: Vec<TaskRow> = tasks
        .into_iter()
        .filter(|t| t.terminal.is_none_or(|n| !skip_tabs.contains(&n)))
        .filter(|t| t.workspace.is_none_or(|w| !skip_spaces.contains(&w)))
        .collect();
    // ponytail: qass's terminals are the ones its space holds now; a write from a qass tab
    // closed since does not count, so such a claim is listed once more.
    let log = std::fs::read_to_string(dir.join("prompted.log")).unwrap_or_default();
    let written = qass_writes(&log, &qass);
    Ok(claims_line(&stranded(&tasks, &carded, &written, now)))
}

/// Write `body` to `name` in `dir` beside and renamed over, so the watcher never reads half.
fn write_whole(dir: &std::path::Path, name: &str, body: &str) -> std::io::Result<()> {
    let fresh = dir.join(format!(".{name}.{}.new", std::process::id()));
    std::fs::write(&fresh, format!("{body}\n"))?;
    std::fs::rename(&fresh, dir.join(name))
}

/// Run the scan for the life of the daemon. Only with a qass folder: a person without qass gets
/// no file.
pub(crate) fn scan_stranded_claims(store: &StoreHandle) {
    let store = store.clone();
    tokio::spawn(async move {
        let mut last = crate::paths::qass_data_dir()
            .and_then(|dir| std::fs::read_to_string(dir.join(SCANNED_FILE)).ok())
            .and_then(|s| s.trim().parse::<i64>().ok());
        loop {
            tokio::time::sleep(LOOK_EVERY).await;
            let Some(dir) = crate::paths::qass_data_dir().filter(|d| d.is_dir()) else {
                continue;
            };
            let now = now();
            if !scan_due(last, now, crate::paths::qass_watching()) {
                continue;
            }
            // A store that could not be read is said and tried again at the next look, never
            // counted as a scan that found nothing.
            match scan(&store, &dir, now).await {
                Err(why) => {
                    crate::say!("qoopd: claims scan could not read the store: {why}");
                    continue;
                }
                Ok(line) => {
                    last = Some(now);
                    let _ = write_whole(&dir, SCANNED_FILE, &now.to_string());
                    if let Some(line) = line {
                        let _ = write_whole(&dir, CLAIMS_FILE, &line);
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: i64 = 3600;

    fn claim(id: i64, terminal: Option<i64>, claimed_at: i64) -> TaskRow {
        TaskRow {
            id,
            workspace: Some(1),
            terminal,
            goal: "g".into(),
            stage: "verifying".into(),
            opened_at: 0,
            updated_at: claimed_at,
            claimed_at: Some(claimed_at),
            closed_at: None,
            outcome: None,
            folder: None,
        }
    }

    /// One live run on a fake clock: a look every five minutes from `from` to `to`, and at each
    /// scan the stranded ids, as the loop does.
    fn run(
        from: i64,
        to: i64,
        watching: impl Fn(i64) -> bool,
        tasks: &[TaskRow],
    ) -> Vec<(i64, Vec<i64>)> {
        let mut last = None;
        let mut scans = Vec::new();
        let mut now = from;
        while now <= to {
            if scan_due(last, now, watching(now)) {
                last = Some(now);
                let ids = stranded(tasks, &BTreeSet::new(), &BTreeMap::new(), now)
                    .iter()
                    .map(|t| t.id)
                    .collect();
                scans.push((now, ids));
            }
            now += LOOK_EVERY.as_secs() as i64;
        }
        scans
    }

    #[test]
    fn the_scan_repeats_every_four_hours_within_one_run() {
        let scans = run(0, 13 * H, |_| true, &[]);
        let at: Vec<i64> = scans.iter().map(|(at, _)| *at).collect();
        assert_eq!(at, [0, 4 * H, 8 * H, 12 * H]);
        // A scan kept from before a restart is not run again before its time.
        assert!(!scan_due(Some(10 * H), 13 * H, true));
        assert!(scan_due(Some(10 * H), 14 * H, true));
    }

    #[test]
    fn a_claim_stranded_after_start_is_picked_up_once_it_is_a_day_old() {
        // Claimed two hours into the run: young at the first scans, listed from the first
        // scan past its day.
        let tasks = [claim(7, Some(52), 2 * H)];
        let scans = run(0, 40 * H, |_| true, &tasks);
        let first = scans
            .iter()
            .find(|(_, ids)| !ids.is_empty())
            .expect("listed");
        assert_eq!(*first, (28 * H, vec![7]));
        assert!(
            scans
                .iter()
                .filter(|(at, _)| *at < 26 * H)
                .all(|(_, ids)| ids.is_empty())
        );
    }

    #[test]
    fn nothing_is_scanned_while_watching_is_off() {
        let tasks = [claim(7, Some(52), 0)];
        assert!(run(30 * H, 60 * H, |_| false, &tasks).is_empty());
        // Back on: the next look scans, and finds the claim.
        let scans = run(30 * H, 31 * H, |now| now >= 30 * H + H / 2, &tasks);
        assert_eq!(scans, [(30 * H + H / 2, vec![7])]);
    }

    #[test]
    fn a_claim_drops_off_on_each_of_its_three_dispositions() {
        let now = 30 * H;
        let mut closed = claim(1, Some(51), 0);
        closed.closed_at = Some(H);
        closed.outcome = Some("verified".into());
        let tasks = [
            closed,                     // closed
            claim(2, Some(52), 0),      // a live card names it
            claim(3, Some(53), 0),      // qass prompted it after the claim
            claim(4, Some(54), 2 * H),  // prompted before the claim: still stranded
            claim(5, Some(55), 0),      // written to by a worker, not qass: still stranded
            claim(6, None, H),          // no tab, nothing about it: stranded
            claim(8, Some(58), 20 * H), // under a day: not yet
        ];
        let carded = BTreeSet::from([2]);
        let log = "\
            100 9 T53\n\
            3700 9 T54\n\
            5000 12 T55\n\
            9000 9 ubox/T53\n\
            not a record\n";
        let written = qass_writes(log, &BTreeSet::from([9]));
        let ids: Vec<i64> = stranded(&tasks, &carded, &written, now)
            .iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(ids, [5, 6, 4]);
        let line = claims_line(&stranded(&tasks, &carded, &written, now)).expect("a line");
        assert!(line.starts_with("claims 5 6 4: "), "{line}");
        assert_eq!(claims_line(&[]), None, "nothing stranded says nothing");
    }
}
