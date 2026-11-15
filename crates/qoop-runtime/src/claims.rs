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

/// The most acts one `QassActs` answer carries, and how far back one reads, in seconds.
const ACTS_MAX: usize = 200;
const ACTS_BACK: i64 = 7 * 24 * 3600;
/// How many ended cards are read to find the ones qass closed.
/// ponytail: the newest 1000 ended cards; a week with more ends loses its oldest closes.
const ENDED_READ: usize = 1000;
/// The note qass closes a card it handled itself with (`escalate.sh done E<n> qass`).
const QASS_NOTE: &str = "qass";

/// What qass did on its own since `since` (`QassActs`), clamped to [`ACTS_BACK`]: every write
/// it made to an agent (`prompted.log`, each record and not only the newest per tab), the
/// cards it closed with its own note, the tasks it closed verified.
pub(crate) async fn qass_acts(
    id: String,
    since: i64,
    store: &StoreHandle,
) -> crate::control::ControlResponse {
    let since = since.max(now() - ACTS_BACK);
    let space = crate::asks::qass_space(store).await;
    let read = store
        .run(move |s| {
            let qass: BTreeSet<u32> = match space {
                Some(space) => s
                    .open_terminals(space)?
                    .into_iter()
                    .filter_map(|t| u32::try_from(t.id).ok())
                    .collect(),
                None => BTreeSet::new(),
            };
            let mut verified = Vec::new();
            for task in s.closed_tasks_since(since, ACTS_MAX)? {
                if task.outcome.as_deref() != Some("verified") {
                    continue;
                }
                let closed = s.task_events_of(task.id, "closed", 1)?;
                if let Some(event) = closed.first()
                    && event.body.starts_with("verified by qass")
                {
                    verified.push((task.id, event.at));
                }
            }
            Ok((qass, s.ended_asks(ENDED_READ)?, verified))
        })
        .await;
    match read {
        Ok((qass, asks, verified)) => {
            // ponytail: qass's terminals are the ones its space holds now, as in the scan.
            let log = crate::paths::qass_data_dir()
                .and_then(|dir| std::fs::read_to_string(dir.join("prompted.log")).ok())
                .unwrap_or_default();
            crate::control::ControlResponse::QassActs {
                id,
                acts: acts(&log, &qass, &asks, &verified, since),
            }
        }
        Err(e) => crate::control::ControlResponse::Failed {
            id,
            reason: e.to_string(),
        },
    }
}

/// The acts from their three sources at or after `since`, newest first, at most [`ACTS_MAX`]:
/// `log` is `prompted.log` (`<unix> <from terminal> T<n>` or `<machine>/T<n>`), kept for a
/// record sent from one of `qass`'s terminals; `asks` the ended cards; `verified` each task
/// qass closed verified, with when.
fn acts(
    log: &str,
    qass: &BTreeSet<u32>,
    asks: &[qoop_store::AskRow],
    verified: &[(i64, i64)],
    since: i64,
) -> Vec<crate::control::QassAct> {
    use crate::control::QassAct;
    let tab = |s: &str| s.strip_prefix('T').and_then(|n| n.parse::<u32>().ok());
    let mut acts = Vec::new();
    for line in log.lines() {
        let mut w = line.split_whitespace();
        let (Some(at), Some(from), Some(to)) = (w.next(), w.next(), w.next()) else {
            continue;
        };
        let (Ok(at), Ok(from)) = (at.parse::<i64>(), from.parse::<u32>()) else {
            continue;
        };
        let remote = to
            .split_once('/')
            .is_some_and(|(machine, t)| !machine.is_empty() && tab(t).is_some());
        if at < since
            || !qass.contains(&from)
            || to.chars().any(char::is_control)
            || (tab(to).is_none() && !remote)
        {
            continue;
        }
        acts.push(QassAct {
            at,
            kind: "wrote".to_string(),
            terminal: tab(to),
            card: None,
            task: None,
            text: format!("wrote to {to}"),
        });
    }
    for card in asks {
        let by_qass =
            card.status == "closed" && card.note.as_deref().map(str::trim) == Some(QASS_NOTE);
        let Some(at) = card.closed_at.filter(|at| by_qass && *at >= since) else {
            continue;
        };
        acts.push(QassAct {
            at,
            kind: "closed".to_string(),
            terminal: card.terminal.and_then(|t| u32::try_from(t).ok()),
            card: Some(card.card.clone()),
            task: card.task,
            text: format!("closed {}", card.card),
        });
    }
    for &(task, at) in verified.iter().filter(|(_, at)| *at >= since) {
        acts.push(QassAct {
            at,
            kind: "verified".to_string(),
            terminal: None,
            card: None,
            task: Some(task),
            text: format!("verified task {task}"),
        });
    }
    acts.sort_by_key(|a| std::cmp::Reverse(a.at));
    acts.truncate(ACTS_MAX);
    acts
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

    fn ended(card: &str, status: &str, note: &str, closed_at: i64) -> qoop_store::AskRow {
        qoop_store::AskRow {
            id: 1,
            card: card.into(),
            agent: Some("T14".into()),
            terminal: Some(14),
            session: None,
            task: Some(11),
            status: status.into(),
            note: Some(note.into()),
            body: "{}".into(),
            answer: None,
            option: None,
            opened_at: 0,
            closed_at: Some(closed_at),
        }
    }

    fn said(acts: &[crate::control::QassAct]) -> Vec<(i64, &str, &str)> {
        acts.iter()
            .map(|a| (a.at, a.kind.as_str(), a.text.as_str()))
            .collect()
    }

    #[test]
    fn every_write_qass_made_is_an_act_and_no_one_elses() {
        // Two writes to one tab are two acts, not the newest only; a worker's write and a
        // line that is no record are none.
        let log = "100 9 T14\n200 9 T14\n300 12 T14\n400 9 ubox/T3\nnot a record\n500 9 E7\n";
        let acts = acts(log, &BTreeSet::from([9]), &[], &[], 0);
        assert_eq!(
            said(&acts),
            [
                (400, "wrote", "wrote to ubox/T3"),
                (200, "wrote", "wrote to T14"),
                (100, "wrote", "wrote to T14"),
            ]
        );
        assert_eq!(acts[1].terminal, Some(14));
        assert_eq!(acts[0].terminal, None, "a remote's tab is no tab here");
    }

    #[test]
    fn a_card_is_qass_s_act_only_when_closed_with_its_own_note() {
        let asks = [
            ended("E41", "closed", "qass", 100),
            ended("E42", "closed", "stale", 200),
            ended("E43", "answered", "qass", 300),
        ];
        let acts = acts("", &BTreeSet::new(), &asks, &[], 0);
        assert_eq!(said(&acts), [(100, "closed", "closed E41")]);
        assert_eq!(acts[0].card.as_deref(), Some("E41"));
        assert_eq!((acts[0].terminal, acts[0].task), (Some(14), Some(11)));
    }

    #[test]
    fn a_task_qass_verified_is_an_act() {
        let acts = acts("", &BTreeSet::new(), &[], &[(11, 100)], 0);
        assert_eq!(said(&acts), [(100, "verified", "verified task 11")]);
        assert_eq!(acts[0].task, Some(11));
    }

    #[test]
    fn acts_come_newest_first_from_every_source_and_none_before_since() {
        let acts = acts(
            "50 9 T1\n300 9 T2\n",
            &BTreeSet::from([9]),
            &[ended("E1", "closed", "qass", 200)],
            &[(5, 250), (6, 60)],
            100,
        );
        assert_eq!(
            said(&acts),
            [
                (300, "wrote", "wrote to T2"),
                (250, "verified", "verified task 5"),
                (200, "closed", "closed E1"),
            ]
        );
        let many = "1 9 T1\n".repeat(ACTS_MAX + 5);
        assert_eq!(
            super::acts(&many, &BTreeSet::from([9]), &[], &[], 0).len(),
            ACTS_MAX
        );
    }
}
