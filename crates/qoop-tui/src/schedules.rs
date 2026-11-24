//! The schedules the daemon keeps, as this client reads them, and the words every client says
//! their timing in. qass's ledger lists them; the desktop's schedule sheet builds its timing
//! from the same presets, so the two cannot disagree about what a preset means.

use qoop_runtime::control::{ControlRequest, ControlResponse, ScheduleInfo};
use qoop_runtime::schedule::{Cron, Naive, Zone, next_slot, parse_at};

/// The id of the request that reads the list, and the prefix of the ones that write it.
pub const READ_ID: &str = "schedules-read";

/// How a schedule repeats, as the form's chips say it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Once,
    Weekdays,
    EveryDay,
    EveryTwoHours,
    Custom,
    /// A watch: a command checked every few minutes, its new lines start the run.
    OnChange,
}

impl Preset {
    pub const ALL: [Preset; 6] = [
        Preset::Once,
        Preset::Weekdays,
        Preset::EveryDay,
        Preset::EveryTwoHours,
        Preset::Custom,
        Preset::OnChange,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Preset::Once => "once",
            Preset::Weekdays => "weekdays",
            Preset::EveryDay => "every day",
            Preset::EveryTwoHours => "every 2h",
            Preset::Custom => "custom",
            Preset::OnChange => "on change",
        }
    }
}

/// When a form's schedule fires, as the daemon takes it: a cron line, one moment, or a
/// command to watch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Timing {
    Cron(String),
    At(i64),
    Watch(String),
}

/// `H:MM` or `HH:MM` as an hour and a minute.
fn clock(text: &str) -> Result<(u32, u32), String> {
    let wrong = || "the time is HH:MM, like 09:00".to_string();
    let (hour, minute) = text.trim().split_once(':').ok_or_else(wrong)?;
    let digits = |s: &str, len: std::ops::RangeInclusive<usize>| {
        len.contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
    };
    if !digits(hour, 1..=2) || !digits(minute, 2..=2) {
        return Err(wrong());
    }
    let (hour, minute) = (
        hour.parse::<u32>().map_err(|_| wrong())?,
        minute.parse::<u32>().map_err(|_| wrong())?,
    );
    if hour > 23 || minute > 59 {
        return Err(wrong());
    }
    Ok((hour, minute))
}

/// What a preset, its time (or its text: the custom cron line, the command of on change) and
/// the clock make of the timing: `once` is the next time the clock reads that time, the
/// repeats are the cron lines the daemon names `weekdays 09:00`, `every day 09:00` and
/// `every 2h`, on change the command as typed.
pub fn timing(
    preset: Preset,
    time: &str,
    cron: &str,
    now: i64,
    zone: Zone,
) -> Result<Timing, String> {
    match preset {
        Preset::Once => {
            let (hour, minute) = clock(time)?;
            parse_at(&format!("{hour:02}:{minute:02}"), now, zone).map(Timing::At)
        }
        Preset::Weekdays => clock(time).map(|(h, m)| Timing::Cron(format!("{m} {h} * * 1-5"))),
        Preset::EveryDay => clock(time).map(|(h, m)| Timing::Cron(format!("{m} {h} * * *"))),
        Preset::EveryTwoHours => Ok(Timing::Cron("0 */2 * * *".to_string())),
        Preset::Custom => {
            let text = cron.split_whitespace().collect::<Vec<_>>().join(" ");
            Cron::parse(&text).map_err(|e| format!("{text:?}: {e}"))?;
            Ok(Timing::Cron(text))
        }
        Preset::OnChange => {
            let command = cron.trim();
            if command.is_empty() {
                return Err("write the command to check, like gh issue list".to_string());
            }
            Ok(Timing::Watch(command.to_string()))
        }
    }
}

/// The first moment `timing` fires after `now`, the way the daemon will find it.
pub fn first_run(timing: &Timing, now: i64, zone: Zone) -> Option<i64> {
    match timing {
        Timing::At(at) => Some(*at),
        Timing::Cron(text) => next_slot(&Cron::parse(text).ok()?, now, zone),
        // Not a run: its first check notes what is there, which the form says in words.
        Timing::Watch(_) => None,
    }
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

/// A moment the way the panel says it: the time while it is today, `tomorrow 09:00`, the
/// weekday inside the week (`mon 08:30`), else the date (`oct 24 17:00`).
pub fn moment(at: i64, now: i64, zone: Zone) -> String {
    let (wall, today) = ((zone.local)(at), (zone.local)(now));
    let days = |n: Naive| n.secs().div_euclid(86_400);
    let ahead = days(wall) - days(today);
    let time = wall.clock();
    match ahead {
        0 => time,
        1 => format!("tomorrow {time}"),
        2..=6 => format!("{} {time}", DAYS[(days(wall) + 4).rem_euclid(7) as usize]),
        _ => format!("{} {} {time}", MONTHS[(wall.month - 1) as usize], wall.day),
    }
}

/// The program a launcher's command line starts: its first word that is not `NAME=value` or `env`,
/// without its folder.
fn program_of(command: &str) -> &str {
    command
        .split_whitespace()
        .find(|w| !w.contains('=') && *w != "env")
        .map_or("", |w| w.rsplit('/').next().unwrap_or(w))
}

/// The agents a form offers: the four a scheduled run can start by name, then the owner's own
/// `[agents]` lines that run one of them (a run is started with a message, which only those
/// take), qass's own pick first.
pub fn agents(launchers: &[crate::config::Launcher], qass: &str) -> Vec<String> {
    let mut all: Vec<String> = crate::config::QASS_AGENTS
        .iter()
        .map(|a| a.to_string())
        .chain(
            launchers
                .iter()
                .filter(|l| crate::config::QASS_AGENTS.contains(&program_of(&l.command)))
                .map(|l| l.name.clone()),
        )
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    all.retain(|a| seen.insert(a.clone()));
    if let Some(at) = all.iter().position(|a| a == qass) {
        all[..=at].rotate_right(1);
    }
    all
}

/// The schedules as this client keeps them.
#[derive(Debug, Default)]
pub struct Pane {
    /// Every schedule, oldest first, as the daemon last said.
    pub list: Vec<ScheduleInfo>,
    /// The daemon has answered a read once.
    pub loaded: bool,
    /// The daemon predates schedules.
    pub unsupported: bool,
    /// The read is owed: a change event came, or the first read has not been asked.
    pub stale: bool,
}

impl Pane {
    /// Schedules not yet read: the first read is owed.
    pub fn new() -> Pane {
        Pane {
            stale: true,
            ..Pane::default()
        }
    }

    /// Take a fresh read.
    pub fn take(&mut self, list: Vec<ScheduleInfo>) {
        self.list = list;
        self.loaded = true;
        self.unsupported = false;
    }

    /// The read to send, when one is owed and none is in flight.
    pub fn read_request(&mut self) -> Option<ControlRequest> {
        if !self.stale {
            return None;
        }
        self.stale = false;
        Some(ControlRequest::ScheduleRead {
            id: READ_ID.to_string(),
        })
    }

    /// The daemon's answer to the read. True when the screen changed.
    pub fn answered(&mut self, response: &ControlResponse) -> bool {
        match response {
            ControlResponse::Schedules { id, schedules } if id == READ_ID => {
                self.take(schedules.clone());
                true
            }
            ControlResponse::Failed { id, reason }
                // Only an older daemon has none: a read that timed out keeps what was shown.
                if (id == READ_ID || id.is_empty())
                    && qoop_runtime::control::too_old_for(reason, Some("ScheduleRead")) =>
            {
                self.unsupported = true;
                self.loaded = false;
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A zone that reads the clock as UTC, so a test knows what time it is.
    fn utc() -> Zone {
        Zone {
            local: Naive::from_secs,
            unix: Naive::secs,
        }
    }

    /// 2026-10-22 (a Thursday) at `hour`:`minute` UTC.
    fn at(hour: u32, minute: u32) -> i64 {
        Naive::new(2026, 10, 22, hour, minute).secs()
    }

    #[test]
    fn a_moment_is_the_time_today_then_the_day() {
        let now = at(8, 0);
        assert_eq!(moment(at(9, 0), now, utc()), "09:00");
        assert_eq!(moment(at(9, 0) + 86_400, now, utc()), "tomorrow 09:00");
        // Thursday 22 oct: three days on is a Sunday.
        assert_eq!(moment(at(8, 30) + 3 * 86_400, now, utc()), "sun 08:30");
        assert_eq!(moment(at(17, 0) + 9 * 86_400, now, utc()), "oct 31 17:00");
    }

    #[test]
    fn each_chip_becomes_the_timing_the_daemon_names() {
        let now = at(8, 0);
        let go = |p, time: &str, cron: &str| timing(p, time, cron, now, utc());
        assert_eq!(
            go(Preset::Weekdays, "9:05", ""),
            Ok(Timing::Cron("5 9 * * 1-5".to_string()))
        );
        assert_eq!(
            go(Preset::EveryDay, "09:00", ""),
            Ok(Timing::Cron("0 9 * * *".to_string()))
        );
        assert_eq!(
            go(Preset::EveryTwoHours, "", ""),
            Ok(Timing::Cron("0 */2 * * *".to_string()))
        );
        assert_eq!(
            go(Preset::Custom, "", "  30   6 * *  1 "),
            Ok(Timing::Cron("30 6 * * 1".to_string()))
        );
        // Once is the next time the clock reads that time: later today, else tomorrow.
        assert_eq!(go(Preset::Once, "17:00", ""), Ok(Timing::At(at(17, 0))));
        assert_eq!(
            go(Preset::Once, "07:00", ""),
            Ok(Timing::At(at(7, 0) + 86_400))
        );
    }

    #[test]
    fn a_time_or_cron_line_that_is_not_one_is_refused_with_a_reason() {
        let now = at(8, 0);
        for bad in ["", "9", "25:00", "09:60", "9:5", "ab:cd", "09:00:00"] {
            let said = timing(Preset::EveryDay, bad, "", now, utc()).unwrap_err();
            assert!(said.contains("HH:MM"), "{bad:?}: {said}");
        }
        assert!(timing(Preset::Custom, "", "61 * * * *", now, utc()).is_err());
        assert!(timing(Preset::Custom, "", "", now, utc()).is_err());
        // The two hour chip has no time to get wrong.
        assert!(timing(Preset::EveryTwoHours, "junk", "", now, utc()).is_ok());
    }

    #[test]
    fn the_first_run_is_the_next_slot_after_now() {
        let now = at(8, 0);
        let weekdays = timing(Preset::Weekdays, "09:00", "", now, utc()).unwrap();
        assert_eq!(first_run(&weekdays, now, utc()), Some(at(9, 0)));
        // After nine on a Thursday the next weekday slot is Friday's.
        assert_eq!(
            first_run(&weekdays, at(9, 30), utc()),
            Some(at(9, 0) + 86_400)
        );
        let twice = timing(Preset::EveryTwoHours, "", "", now, utc()).unwrap();
        assert_eq!(first_run(&twice, now, utc()), Some(at(10, 0)));
        assert_eq!(
            first_run(&Timing::At(at(17, 0)), now, utc()),
            Some(at(17, 0))
        );
    }

    #[test]
    fn the_agents_are_the_four_and_the_owners_own_that_run_one_with_qasss_first() {
        let launchers = vec![
            crate::config::Launcher {
                name: "fast".into(),
                command: "FOO=1 /usr/bin/claude --fast".into(),
            },
            crate::config::Launcher {
                name: "shell".into(),
                command: "bash".into(),
            },
        ];
        let all = agents(&launchers, "claude");
        assert_eq!(all, ["claude", "codex", "gemini", "opencode", "fast"]);
        assert_eq!(agents(&launchers, "fast")[0], "fast");
        assert_eq!(agents(&launchers, "codex")[0], "codex");
    }

    #[test]
    fn an_older_daemon_has_no_schedules_and_a_read_is_asked_once() {
        let mut p = Pane::new();
        assert!(p.read_request().is_some() && p.read_request().is_none());
        let old = ControlResponse::Failed {
            id: READ_ID.into(),
            reason: "unknown variant `ScheduleRead`, expected one of `X`".into(),
        };
        assert!(p.answered(&old) && p.unsupported && !p.loaded);
        assert!(p.answered(&ControlResponse::Schedules {
            id: READ_ID.into(),
            schedules: Vec::new(),
        }));
        assert!(p.loaded && !p.unsupported);
    }

    /// An older daemon has no schedules: the read says so and the ledger lists none.
    #[test]
    fn an_older_daemon_has_no_schedules_and_the_ledger_lists_none() {
        let mut p = Pane::new();
        p.take(Vec::new());
        assert!(p.loaded);
        let said = "unknown variant `ScheduleRead`, expected one of".to_string();
        assert!(p.answered(&ControlResponse::Failed {
            id: String::new(),
            reason: said
        }));
        assert!(p.unsupported && !p.loaded && p.list.is_empty());
    }
}
