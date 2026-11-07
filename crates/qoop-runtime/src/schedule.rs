//! When a schedule fires: the cron text, the wall clock it is read on, and the one decision
//! the daemon's loop takes from a row and the time.
//!
//! Everything here is pure. The clock is an argument (`now`, unix seconds) and the time zone
//! is a [`Zone`], a pair of plain functions, so a test drives a fake clock through a fake
//! zone with a daylight saving change in it and the daemon uses the machine's own
//! ([`system_zone`]).
//!
//! A cron slot is a minute of the LOCAL wall clock, so the walk that finds the next one
//! counts local minutes, never unix minutes: a unix minute walk cannot say "09:00" across a
//! daylight saving change. A wall minute is then mapped back to an instant, with two rules
//! for the two odd cases. A minute that is repeated when the clock goes back (01:30 twice)
//! is one slot, the first time it is shown, because the walk goes on from the wall clock
//! and never revisits the hour. A minute that does not exist when the clock goes forward
//! (02:30 on the night it jumps to 03:00) is a slot at the first minute after the gap, as
//! cronie runs a job caught in the gap.

use qoop_store::ScheduleRow;

/// A calendar minute on a wall clock, with no zone: what a cron field is compared with.
/// Ordered as a clock reads: year, month, day, hour, minute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Naive {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
}

/// Days since 1970-01-01 of a civil date (proleptic Gregorian).
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = i64::from(year) - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (i64::from(month) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date of a day count since 1970-01-01.
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = (yoe + era * 400 + i64::from(month <= 2)) as i32;
    (year, month, day)
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl Naive {
    pub fn new(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> Self {
        Self {
            year,
            month,
            day,
            hour,
            minute,
        }
    }

    /// The wall clock that reads `secs` seconds after 1970-01-01 00:00, seconds cut off.
    pub fn from_secs(secs: i64) -> Self {
        let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
        let in_day = secs.rem_euclid(86_400);
        Self::new(
            year,
            month,
            day,
            (in_day / 3600) as u32,
            (in_day % 3600 / 60) as u32,
        )
    }

    /// The seconds after 1970-01-01 00:00 this wall clock reads, as if it were UTC.
    pub fn secs(self) -> i64 {
        days_from_civil(self.year, self.month, self.day) * 86_400
            + i64::from(self.hour) * 3600
            + i64::from(self.minute) * 60
    }

    /// 0 for Sunday to 6 for Saturday.
    fn weekday(self) -> u32 {
        // 1970-01-01 was a Thursday.
        (days_from_civil(self.year, self.month, self.day) + 4).rem_euclid(7) as u32
    }

    /// `09:05`.
    pub fn clock(self) -> String {
        format!("{:02}:{:02}", self.hour, self.minute)
    }
}

/// A time zone as two plain functions, which is all a schedule needs of one.
#[derive(Clone, Copy)]
pub struct Zone {
    /// The wall clock showing at a unix instant.
    pub local: fn(i64) -> Naive,
    /// A unix instant at which the wall clock shows this, as `mktime` answers with
    /// `tm_isdst = -1`: for a repeated or a missing minute it is some instant near it, and
    /// [`instant_of`] settles which.
    pub unix: fn(Naive) -> i64,
}

/// The machine's own zone: `tzset`, `localtime_r` and `mktime`, read at every call so a
/// zone changed while the daemon runs is seen at the next look.
pub fn system_zone() -> Zone {
    // SAFETY: `tzset` is POSIX, in every libc, with this signature, though the `libc` crate
    // does not declare it.
    unsafe extern "C" {
        fn tzset();
    }

    /// `tzset` writes process state, so one call at a time.
    static ZONE: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn local(at: i64) -> Naive {
        let _held = ZONE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stamp = at as libc::time_t;
        // SAFETY: `tm` is plain data, `stamp` and `tm` outlive the call, and no other thread
        // is inside the time zone functions (the lock above).
        let tm = unsafe {
            tzset();
            let mut tm: libc::tm = std::mem::zeroed();
            if libc::localtime_r(&stamp, &mut tm).is_null() {
                return Naive::from_secs(at);
            }
            tm
        };
        Naive::new(
            tm.tm_year + 1900,
            (tm.tm_mon + 1) as u32,
            tm.tm_mday as u32,
            tm.tm_hour as u32,
            tm.tm_min as u32,
        )
    }

    fn unix(wall: Naive) -> i64 {
        let _held = ZONE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // SAFETY: as above.
        unsafe {
            tzset();
            let mut tm: libc::tm = std::mem::zeroed();
            tm.tm_year = wall.year - 1900;
            tm.tm_mon = wall.month as i32 - 1;
            tm.tm_mday = wall.day as i32;
            tm.tm_hour = wall.hour as i32;
            tm.tm_min = wall.minute as i32;
            tm.tm_isdst = -1;
            libc::mktime(&mut tm) as i64
        }
    }

    Zone { local, unix }
}

/// The instant a wall minute is reached: the first one when the clock shows it twice, and
/// the first minute after the gap when it never shows it.
pub fn instant_of(zone: Zone, wall: Naive) -> i64 {
    let guess = (zone.unix)(wall);
    if (zone.local)(guess) == wall {
        // `mktime` may have picked the second time round of a repeated minute.
        return [3600, 1800]
            .into_iter()
            .map(|back| guess - back)
            .find(|earlier| (zone.local)(*earlier) == wall)
            .unwrap_or(guess);
    }
    // In a gap: the first second the wall clock reads at or after `wall`. No change of
    // clock is wider than a few hours, so the answer is inside this bracket.
    let (mut before, mut after) = (guess - 6 * 3600, guess + 6 * 3600);
    while after - before > 1 {
        let middle = before + (after - before) / 2;
        if (zone.local)(middle) >= wall {
            after = middle;
        } else {
            before = middle;
        }
    }
    after
}

/// A cron line: five fields, `minute hour day-of-month month day-of-week`, each `*`, a number,
/// a range `a-b`, a list `a,b` or a step `*/n` or `a-b/n`. No names. A day of the week is 0
/// to 7 with 0 and 7 both Sunday. When both day fields are restricted a day matches either,
/// as every cron does; when one is `*` only the other counts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cron {
    minutes: u64,
    hours: u32,
    days: u32,
    months: u16,
    weekdays: u8,
    day_any: bool,
    weekday_any: bool,
}

/// One field: the bits it sets between `low` and `high`, and whether it starts with `*`.
fn field(text: &str, low: u32, high: u32) -> Result<(u64, bool), String> {
    let number = |s: &str| -> Result<u32, String> {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!("{s:?} is not a number"));
        }
        s.parse::<u32>()
            .ok()
            .filter(|n| (low..=high).contains(n))
            .ok_or_else(|| format!("{s} is not between {low} and {high}"))
    };
    let mut bits = 0u64;
    for part in text.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((range, step)) => (range, Some(step)),
            None => (part, None),
        };
        let (from, to) = match range.split_once('-') {
            _ if range == "*" => (low, high),
            Some((from, to)) => (number(from)?, number(to)?),
            None if step.is_some() => (number(range)?, high),
            None => (number(range)?, number(range)?),
        };
        if from > to {
            return Err(format!("{range} counts backwards"));
        }
        let step = match step {
            Some(step) => step
                .parse::<u32>()
                .ok()
                .filter(|n| *n >= 1)
                .ok_or_else(|| format!("{step} is not a step"))?,
            None => 1,
        };
        for value in (from..=to).step_by(step as usize) {
            bits |= 1 << value;
        }
    }
    Ok((bits, text.starts_with('*')))
}

impl Cron {
    /// Read a cron line, refusing one that has no slot in the next five years (31 February).
    pub fn parse(text: &str) -> Result<Self, String> {
        let fields: Vec<&str> = text.split_whitespace().collect();
        let [minute, hour, day, month, weekday] = fields[..] else {
            return Err(
                "a cron line has five fields: minute hour day-of-month month day-of-week".into(),
            );
        };
        let (minutes, _) = field(minute, 0, 59).map_err(|e| format!("minute: {e}"))?;
        let (hours, _) = field(hour, 0, 23).map_err(|e| format!("hour: {e}"))?;
        let (days, day_any) = field(day, 1, 31).map_err(|e| format!("day of month: {e}"))?;
        let (months, _) = field(month, 1, 12).map_err(|e| format!("month: {e}"))?;
        let (weekdays, weekday_any) =
            field(weekday, 0, 7).map_err(|e| format!("day of week: {e}"))?;
        // Sunday is 0 and 7.
        let weekdays = ((weekdays | weekdays >> 7) & 0x7f) as u8;
        let cron = Self {
            minutes,
            hours: hours as u32,
            days: days as u32,
            months: months as u16,
            weekdays,
            day_any,
            weekday_any,
        };
        // Days only, from a fixed date: a pattern that matches no day in five years (a leap
        // year is always among them) matches none.
        let start = days_from_civil(2024, 1, 1);
        let ever = (start..start + 5 * 366).any(|day| {
            let (year, month, day) = civil_from_days(day);
            cron.day_matches(Naive::new(year, month, day, 0, 0))
        });
        if !ever {
            return Err("that never comes: no day of the year matches it".into());
        }
        Ok(cron)
    }

    fn day_matches(&self, wall: Naive) -> bool {
        if self.months & (1 << wall.month) == 0 {
            return false;
        }
        let day = self.days & (1 << wall.day) != 0;
        let weekday = self.weekdays & (1 << wall.weekday()) != 0;
        if self.day_any || self.weekday_any {
            day && weekday
        } else {
            day || weekday
        }
    }
}

/// The first slot of `cron` strictly after `after` (unix seconds), looking at most five years
/// ahead.
pub fn next_slot(cron: &Cron, after: i64, zone: Zone) -> Option<i64> {
    let start = (zone.local)(after);
    let last_year = start.year + 5;
    let mut wall = Naive::from_secs(start.secs() + 60);
    while wall.year <= last_year {
        // Whole months, days and hours that cannot match are stepped over.
        if cron.months & (1 << wall.month) == 0 {
            let (year, month) = match wall.month {
                12 => (wall.year + 1, 1),
                month => (wall.year, month + 1),
            };
            wall = Naive::from_secs(days_from_civil(year, month, 1) * 86_400);
        } else if !cron.day_matches(wall) {
            wall = Naive::from_secs((wall.secs().div_euclid(86_400) + 1) * 86_400);
        } else if cron.hours & (1 << wall.hour) == 0 {
            wall = Naive::from_secs((wall.secs().div_euclid(3600) + 1) * 3600);
        } else if cron.minutes & (1 << wall.minute) == 0 {
            wall = Naive::from_secs(wall.secs() + 60);
        } else {
            let slot = instant_of(zone, wall);
            // A wall minute whose first showing is already past is the second time round
            // of a repeated hour: not a slot.
            if slot > after {
                return Some(slot);
            }
            wall = Naive::from_secs(wall.secs() + 60);
        }
    }
    None
}

/// What the loop does with a schedule at a time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Nothing to do now or later: paused, or a one-off that has run.
    Idle,
    /// Look again at this instant.
    Wait(i64),
    /// Claim this slot and start the agent.
    Fire(i64),
    /// Claim this slot without starting anything, for this reason.
    Skip(i64, String),
}

/// How far back a repeat is caught up: the owner who was away a weekend still gets Monday's
/// run, one who was away longer does not get a pile of old ones.
const REPEAT_WINDOW: i64 = 7 * 86_400;

/// The same for a one-off.
const ONE_OFF_WINDOW: i64 = 86_400;

/// A slot this recent is on time, not a catch-up: the loop wakes a little late (a minute at
/// most, see `schedules.rs`), and a `*/15` run must not be taken for a missed one.
const ON_TIME: i64 = 120;

/// A catch-up is skipped when the next slot is nearer than this: the next run covers it.
const NEAR: i64 = 3600;

/// A last slot this far ahead of the clock means the clock was set back.
const CLOCK_BACK: i64 = 86_400;

/// Whether the row's last slot is from the future by more than a day: the clock was set back
/// since it fired. Its last slot would hold every slot back until the clock caught up, so the
/// row is moved to now with a skip, and counts from there.
fn clock_jumped(row: &ScheduleRow, now: i64) -> bool {
    row.last_slot.is_some_and(|last| last > now + CLOCK_BACK)
}

/// The last slot of `cron` in `(from, now]`.
fn latest_slot(cron: &Cron, from: i64, now: i64, zone: Zone) -> Option<i64> {
    let (mut latest, mut at) = (None, from);
    while let Some(slot) = next_slot(cron, at, zone).filter(|slot| *slot <= now) {
        latest = Some(slot);
        at = slot;
    }
    latest
}

/// How often a watch's command is run, in seconds. A check is a short process, never an agent:
/// an agent starts only when the output has lines the last check did not.
pub const WATCH_EVERY: i64 = 300;

/// When a watch is next checked: at once when it never was, then every [`WATCH_EVERY`]. A slot
/// further ahead than that is a clock that was set back, and is due now. Missed checks are not
/// caught up one by one: one check sees everything that is new.
fn watch_due(row: &ScheduleRow, now: i64) -> i64 {
    let due = row
        .last_slot
        .map_or(row.created_at, |slot| slot + WATCH_EVERY);
    if due > now + WATCH_EVERY { now } else { due }
}

/// What to do with `row` at `now`.
///
/// A slot that came due is fired when it is on time, and, when it was missed (the Mac slept,
/// qoop was down), only the newest missed one is, within a week for a repeat and a day for a
/// one-off, and not when the next slot is under an hour away. Every slot the row's last one
/// has not covered is covered by the claim of the newest, so a fired or skipped slot is never
/// seen again.
pub fn decide(row: &ScheduleRow, now: i64, zone: Zone) -> Decision {
    if row.paused {
        return Decision::Idle;
    }
    if row.watch.is_some() {
        let due = watch_due(row, now);
        return if due <= now {
            Decision::Fire(now)
        } else {
            Decision::Wait(due)
        };
    }
    if let Some(at) = row.at {
        return decide_once(row, at, now);
    }
    let Some(cron) = row.cron.as_deref().and_then(|c| Cron::parse(c).ok()) else {
        return Decision::Idle;
    };
    if clock_jumped(row, now) {
        return Decision::Skip(now, "the clock was set back".into());
    }
    // A row made under a clock that was then set back would wait for that clock: made now.
    let base = row
        .last_slot
        .unwrap_or(i64::MIN)
        .max(row.created_at.min(now));
    let floor = now - REPEAT_WINDOW;
    let from = base.max(floor);
    match latest_slot(&cron, from, now, zone) {
        Some(latest) => {
            let missed = now - latest > ON_TIME;
            let near = next_slot(&cron, latest, zone).is_some_and(|next| next - now < NEAR);
            if missed && near {
                Decision::Skip(latest, "the next run is less than an hour away".into())
            } else {
                Decision::Fire(latest)
            }
        }
        // Slots fell out of the window unseen: covered, so they are not looked for again.
        None if base < floor && next_slot(&cron, base, zone).is_some_and(|next| next <= floor) => {
            Decision::Skip(floor, "it was due more than 7 days ago".into())
        }
        None => next_slot(&cron, from, zone).map_or(Decision::Idle, Decision::Wait),
    }
}

fn decide_once(row: &ScheduleRow, at: i64, now: i64) -> Decision {
    // Done once a slot at or after `at` was claimed or skipped, or the schedule was resumed
    // after it (a resume banks nothing).
    if row.last_slot.is_some_and(|last| last >= at) {
        return Decision::Idle;
    }
    if now < at {
        Decision::Wait(at)
    } else if now - at <= ONE_OFF_WINDOW {
        Decision::Fire(at)
    } else {
        Decision::Skip(at, "it was due more than a day ago".into())
    }
}

/// When `row` next comes due, for a list: none when paused or done.
pub fn next_at(row: &ScheduleRow, now: i64, zone: Zone) -> Option<i64> {
    if row.paused {
        return None;
    }
    if row.watch.is_some() {
        return Some(watch_due(row, now));
    }
    match (row.at, row.cron.as_deref()) {
        (Some(at), _) => row.last_slot.is_none_or(|last| last < at).then_some(at),
        (None, Some(cron)) => Cron::parse(cron)
            .ok()
            .and_then(|cron| next_slot(&cron, now, zone)),
        (None, None) => None,
    }
}

const DAYS: [&str; 7] = [
    "sundays",
    "mondays",
    "tuesdays",
    "wednesdays",
    "thursdays",
    "fridays",
    "saturdays",
];

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// The words every client shows for a schedule's timing: `every day 09:00`, `weekdays 09:00`,
/// `mondays 08:30`, `every 2h`, `every 15m`, `once oct 24 17:00`, else the cron text itself.
pub fn when_words(row: &ScheduleRow, zone: Zone) -> String {
    if row.watch.is_some() {
        return format!("on change, every {}m", WATCH_EVERY / 60);
    }
    if let Some(at) = row.at {
        let wall = (zone.local)(at);
        return format!(
            "once {} {} {}",
            MONTHS[(wall.month - 1) as usize],
            wall.day,
            wall.clock()
        );
    }
    let text = row.cron.clone().unwrap_or_default();
    let fields: Vec<&str> = text.split_whitespace().collect();
    let [minute, hour, day, month, weekday] = fields[..] else {
        return text;
    };
    let number = |s: &str| {
        s.parse::<u32>()
            .ok()
            .filter(|_| s.bytes().all(|b| b.is_ascii_digit()))
    };
    let step = |s: &str| s.strip_prefix("*/").and_then(number);
    if let (Some(minute), Some(hour), "*", "*") = (number(minute), number(hour), day, month) {
        let clock = format!("{hour:02}:{minute:02}");
        match weekday {
            "*" => return format!("every day {clock}"),
            "1-5" => return format!("weekdays {clock}"),
            _ => {
                if let Some(d) = number(weekday).filter(|d| *d <= 7) {
                    return format!("{} {clock}", DAYS[(d % 7) as usize]);
                }
            }
        }
    }
    if (day, month, weekday) == ("*", "*", "*") {
        if minute == "0"
            && let Some(n) = step(hour)
        {
            return format!("every {n}h");
        }
        if hour == "*"
            && let Some(n) = step(minute)
        {
            return format!("every {n}m");
        }
    }
    text
}

/// A moment a person typed for a one-off, as a unix instant: `HH:MM` (today, or tomorrow when
/// that has passed), `YYYY-MM-DD HH:MM`, or `+30s`, `+10m`, `+2h` from `now`. A dated or
/// relative moment already past is refused.
pub fn parse_at(text: &str, now: i64, zone: Zone) -> Result<i64, String> {
    let text = text.trim();
    let clock = |s: &str| -> Option<(u32, u32)> {
        let (hour, minute) = s.split_once(':')?;
        let digits =
            |s: &str| s.len() <= 2 && !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        if !digits(hour) || !digits(minute) || minute.len() != 2 {
            return None;
        }
        let (hour, minute) = (hour.parse::<u32>().ok()?, minute.parse::<u32>().ok()?);
        (hour < 24 && minute < 60).then_some((hour, minute))
    };
    if let Some(rest) = text.strip_prefix('+') {
        let Some((unit_at, unit)) = rest.char_indices().last() else {
            return Err(format!("{text} is not +30s, +10m or +2h"));
        };
        let digits = &rest[..unit_at];
        let seconds = match unit {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            _ => return Err(format!("{text} is not +30s, +10m or +2h")),
        };
        let n = digits
            .parse::<i64>()
            .ok()
            .filter(|n| *n >= 1 && digits.bytes().all(|b| b.is_ascii_digit()))
            .ok_or_else(|| format!("{text} is not +30s, +10m or +2h"))?;
        return n
            .checked_mul(seconds)
            .map(|span| now + span)
            .ok_or_else(|| format!("{text} is too far away"));
    }
    if let Some((hour, minute)) = clock(text) {
        let today = (zone.local)(now);
        let at = instant_of(
            zone,
            Naive::new(today.year, today.month, today.day, hour, minute),
        );
        if at >= now {
            return Ok(at);
        }
        let tomorrow = Naive::from_secs(today.secs() + 86_400);
        return Ok(instant_of(
            zone,
            Naive::new(tomorrow.year, tomorrow.month, tomorrow.day, hour, minute),
        ));
    }
    let dated = text.split_once(' ').and_then(|(date, time)| {
        let mut parts = date.split('-');
        let year = parts.next()?.parse::<i32>().ok()?;
        let month = parts.next()?.parse::<u32>().ok()?;
        let day = parts.next()?.parse::<u32>().ok()?;
        let (hour, minute) = clock(time.trim())?;
        let real = (1..=12).contains(&month) && (1..=days_in_month(year, month)).contains(&day);
        (parts.next().is_none() && real).then(|| Naive::new(year, month, day, hour, minute))
    });
    let Some(wall) = dated else {
        return Err(format!(
            "{text} is not HH:MM, YYYY-MM-DD HH:MM, +30s, +10m or +2h"
        ));
    };
    let at = instant_of(zone, wall);
    if at < now {
        return Err(format!("{text} has already passed"));
    }
    Ok(at)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unix instant from a UTC date and time.
    fn utc(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> i64 {
        Naive::new(year, month, day, hour, minute).secs()
    }

    /// UTC plus three hours all year.
    fn plus3() -> Zone {
        Zone {
            local: |at| Naive::from_secs(at + 3 * 3600),
            unix: |wall| wall.secs() - 3 * 3600,
        }
    }

    /// New York in 2026: UTC-5, and UTC-4 from 2026-03-08 02:00 local to 2026-11-01 02:00
    /// local (daylight time). Its `unix` is as adversarial as `mktime` may be: a repeated
    /// minute answers its second showing, a missing one is read with the old offset.
    fn new_york() -> Zone {
        const SPRING: i64 = 1_772_953_200; // 2026-03-08 07:00 UTC
        const FALL: i64 = 1_793_512_800; // 2026-11-01 06:00 UTC
        fn offset(at: i64) -> i64 {
            if (SPRING..FALL).contains(&at) {
                -4 * 3600
            } else {
                -5 * 3600
            }
        }
        Zone {
            local: |at| Naive::from_secs(at + offset(at)),
            unix: |wall| {
                let standard = wall.secs() + 5 * 3600;
                let daylight = wall.secs() + 4 * 3600;
                let shows = |at: i64| Naive::from_secs(at + offset(at)) == wall;
                if shows(standard) || !shows(daylight) {
                    standard
                } else {
                    daylight
                }
            },
        }
    }

    fn cron(text: &str) -> Cron {
        Cron::parse(text).unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    fn repeat(text: &str, created_at: i64, last_slot: Option<i64>) -> ScheduleRow {
        ScheduleRow {
            id: 1,
            name: "t".into(),
            cron: Some(text.into()),
            at: None,
            agent: "claude".into(),
            cwd: "/tmp".into(),
            space: None,
            prompt: "p".into(),
            paused: false,
            created_at,
            last_slot,
            last_at: None,
            last_result: None,
            last_terminal: None,
            watch: None,
        }
    }

    fn once(at: i64, created_at: i64) -> ScheduleRow {
        ScheduleRow {
            cron: None,
            at: Some(at),
            ..repeat("", created_at, None)
        }
    }

    #[test]
    fn calendar_arithmetic_agrees_with_known_dates() {
        assert_eq!(utc(1970, 1, 1, 0, 0), 0);
        assert_eq!(utc(2026, 3, 8, 7, 0), 1_772_953_200);
        assert_eq!(utc(2026, 11, 1, 6, 0), 1_793_512_800);
        let wall = Naive::from_secs(utc(2024, 2, 29, 23, 59));
        assert_eq!(wall, Naive::new(2024, 2, 29, 23, 59));
        assert_eq!(Naive::new(2026, 10, 22, 0, 0).weekday(), 4, "a Thursday");
    }

    #[test]
    fn cron_fields_read_lists_ranges_and_steps() {
        let c = cron("0,30 9-11 * * 1-5");
        assert_eq!(c.minutes, 1 | 1 << 30);
        assert_eq!(c.hours, 0b111 << 9);
        assert!(c.day_any && !c.weekday_any);
        assert_eq!(cron("*/15 * * * *").minutes.count_ones(), 4);
        assert_eq!(cron("0 */6 * * *").hours, 1 | 1 << 6 | 1 << 12 | 1 << 18);
        assert_eq!(cron("0 9 * * 7").weekdays, 1, "7 is Sunday too");
        for bad in [
            "",
            "* * * *",
            "* * * * * *",
            "60 * * * *",
            "*/0 * * * *",
            "5-1 * * * *",
            "a * * * *",
            "0 24 * * *",
            "0 0 0 * *",
            "0 0 31 2 *",
        ] {
            assert!(Cron::parse(bad).is_err(), "{bad:?} was accepted");
        }
        assert!(Cron::parse("0 0 29 2 *").is_ok(), "a leap day comes");
    }

    #[test]
    fn a_restricted_day_of_month_and_week_match_either() {
        // The 13th, or a Friday.
        let c = cron("0 0 13 * 5");
        let zone = plus3();
        let mut at = utc(2026, 10, 1, 0, 0);
        let mut days = Vec::new();
        for _ in 0..6 {
            at = next_slot(&c, at, zone).unwrap();
            days.push(Naive::from_secs(at + 3 * 3600).day);
        }
        assert_eq!(days, [2, 9, 13, 16, 23, 30]);
        // With one of them `*` only the other counts.
        let c = cron("0 0 13 * *");
        let first = next_slot(&c, utc(2026, 10, 1, 0, 0), zone).unwrap();
        assert_eq!(Naive::from_secs(first + 3 * 3600).day, 13);
    }

    #[test]
    fn the_next_slot_is_strictly_after_and_on_the_local_clock() {
        let zone = plus3();
        let daily = cron("0 9 * * *");
        // 09:00 local is 06:00 UTC.
        let slot = utc(2026, 10, 22, 6, 0);
        assert_eq!(next_slot(&daily, slot - 1, zone), Some(slot));
        assert_eq!(next_slot(&daily, slot, zone), Some(slot + 86_400));
    }

    #[test]
    fn the_fall_back_hour_is_one_slot_per_wall_minute() {
        // 2026-11-01: 01:30 happens twice in New York.
        let zone = new_york();
        let half = cron("30 * * * *");
        let before = utc(2026, 11, 1, 4, 30); // 00:30 EDT
        let first = next_slot(&half, before, zone).unwrap();
        assert_eq!(
            first,
            utc(2026, 11, 1, 5, 30),
            "01:30 EDT, the first showing"
        );
        // The second 01:30 (EST, 06:30 UTC) is not a slot; 02:30 EST is.
        let after = next_slot(&half, first, zone).unwrap();
        assert_eq!(after, utc(2026, 11, 1, 7, 30), "02:30 EST");
        // A look that starts inside the second pass finds the same next slot.
        let inside = utc(2026, 11, 1, 6, 10);
        assert_eq!(
            next_slot(&half, inside, zone),
            Some(utc(2026, 11, 1, 7, 30))
        );
    }

    #[test]
    fn a_slot_in_the_spring_gap_comes_at_the_first_minute_after_it() {
        // 2026-03-08: 02:00 to 03:00 does not exist in New York.
        let zone = new_york();
        let daily = cron("30 2 * * *");
        let eve = utc(2026, 3, 7, 12, 0);
        let slot = next_slot(&daily, eve, zone).unwrap();
        assert_eq!(slot, utc(2026, 3, 8, 7, 0), "03:00 EDT");
        assert_eq!((zone.local)(slot), Naive::new(2026, 3, 8, 3, 0));
        // The day after it is 02:30 EDT as usual.
        assert_eq!(next_slot(&daily, slot, zone), Some(utc(2026, 3, 9, 6, 30)));
        // A slot at 03:00 itself is not doubled.
        let both = cron("0,30 2,3 * * *");
        let a = next_slot(&both, eve, zone).unwrap();
        assert_eq!(
            a,
            utc(2026, 3, 8, 7, 0),
            "02:00 and 02:30 both land on 03:00"
        );
        assert_eq!(
            next_slot(&both, a, zone),
            Some(utc(2026, 3, 8, 7, 30)),
            "03:30 follows once, not 03:00 again"
        );
    }

    #[test]
    fn a_slot_on_time_fires_even_a_few_seconds_late() {
        let zone = plus3();
        let slot = utc(2026, 10, 22, 6, 0); // 09:00 local
        let row = repeat("*/15 * * * *", slot - 3600, Some(slot - 900));
        assert_eq!(decide(&row, slot + 5, zone), Decision::Fire(slot));
        assert_eq!(decide(&row, slot - 1, zone), Decision::Wait(slot));
        // The hourly one too.
        let hourly = repeat("0 * * * *", slot - 7200, Some(slot - 3600));
        assert_eq!(decide(&hourly, slot + 59, zone), Decision::Fire(slot));
    }

    #[test]
    fn a_fired_slot_never_fires_again() {
        let zone = plus3();
        let slot = utc(2026, 10, 22, 6, 0);
        let mut row = repeat("0 9 * * *", slot - 86_400 * 3, None);
        let Decision::Fire(fired) = decide(&row, slot + 3, zone) else {
            panic!("it was due");
        };
        // The claim wrote the slot; a restart looks again at the same time and later.
        row.last_slot = Some(fired);
        for now in [slot + 3, slot + 600, slot + 80_000] {
            assert_eq!(
                decide(&row, now, zone),
                Decision::Wait(slot + 86_400),
                "at {now}"
            );
        }
    }

    #[test]
    fn three_missed_dailies_fire_the_newest_once() {
        let zone = plus3();
        let last = utc(2026, 10, 19, 6, 0);
        let row = repeat("0 9 * * *", last - 86_400, Some(last));
        // Down for three days; back at 11:00 local.
        let now = utc(2026, 10, 22, 8, 0);
        assert_eq!(
            decide(&row, now, zone),
            Decision::Fire(utc(2026, 10, 22, 6, 0))
        );
    }

    #[test]
    fn a_repeat_missed_for_more_than_a_week_is_skipped() {
        let zone = plus3();
        // Monthly, on the 1st at 09:00 local; last ran September 1st, back on October 9th.
        let last = utc(2026, 9, 1, 6, 0);
        let row = repeat("0 9 1 * *", last - 86_400, Some(last));
        let now = utc(2026, 10, 9, 8, 0);
        let Decision::Skip(slot, why) = decide(&row, now, zone) else {
            panic!("a month old run was caught up");
        };
        assert!(why.contains("7 days"), "{why}");
        // The skip is claimed, and the October run is not looked for again.
        let mut row = row;
        row.last_slot = Some(slot);
        assert_eq!(
            decide(&row, now + 60, zone),
            Decision::Wait(utc(2026, 11, 1, 6, 0))
        );
        // A week and a day down is still "the Monday run" for a weekly one.
        let weekly = repeat("0 9 * * 1", last - 86_400, Some(utc(2026, 10, 5, 6, 0)));
        assert_eq!(
            decide(&weekly, utc(2026, 10, 13, 6, 0), zone),
            Decision::Fire(utc(2026, 10, 12, 6, 0))
        );
    }

    #[test]
    fn a_catch_up_is_skipped_when_the_next_slot_is_under_an_hour_away() {
        let zone = plus3();
        let last = utc(2026, 10, 22, 5, 0);
        let row = repeat("0 * * * *", last - 3600, Some(last));
        // 10:20 local: the 10:00 run was missed, 11:00 is 40 minutes away.
        let now = utc(2026, 10, 22, 7, 20);
        assert!(
            matches!(decide(&row, now, zone), Decision::Skip(slot, _) if slot == utc(2026, 10, 22, 7, 0))
        );
        // A daily one missed by two hours is not near its next run.
        let daily = repeat(
            "0 9 * * *",
            last - 86_400 * 2,
            Some(utc(2026, 10, 21, 6, 0)),
        );
        assert_eq!(
            decide(&daily, utc(2026, 10, 22, 8, 0), zone),
            Decision::Fire(utc(2026, 10, 22, 6, 0))
        );
    }

    #[test]
    fn a_clock_set_back_moves_the_row_to_now_and_counts_from_there() {
        let zone = plus3();
        let now = utc(2026, 10, 22, 8, 0);
        let mut row = repeat("0 9 * * *", now - 86_400, Some(now + 3 * 86_400));
        // Not fired, not left waiting a week for the clock to catch up: moved.
        let Decision::Skip(slot, why) = decide(&row, now, zone) else {
            panic!("a last slot three days ahead was believed");
        };
        assert_eq!(slot, now);
        assert!(why.contains("clock"), "{why}");
        // Claimed, the next slot is waited for and fires when it comes.
        row.last_slot = Some(slot);
        assert_eq!(
            decide(&row, now + 60, zone),
            Decision::Wait(utc(2026, 10, 23, 6, 0))
        );
        assert_eq!(
            decide(&row, utc(2026, 10, 23, 6, 0), zone),
            Decision::Fire(utc(2026, 10, 23, 6, 0))
        );
        // A day ahead or less is no jump.
        assert!(!clock_jumped(
            &repeat("0 9 * * *", now, Some(now + 3600)),
            now
        ));
    }

    #[test]
    fn resuming_banks_nothing() {
        let zone = plus3();
        let now = utc(2026, 10, 22, 8, 0);
        let mut row = repeat("0 9 * * *", now - 86_400 * 5, Some(now - 86_400 * 4));
        row.paused = true;
        assert_eq!(decide(&row, now, zone), Decision::Idle);
        // What `resume_schedule` writes.
        row.paused = false;
        row.last_slot = Some(now);
        assert_eq!(
            decide(&row, now, zone),
            Decision::Wait(utc(2026, 10, 22, 6, 0) + 86_400)
        );
    }

    #[test]
    fn a_one_off_fires_once_at_its_moment() {
        let zone = plus3();
        let at = utc(2026, 10, 22, 12, 0);
        // Added at the very moment it is for: it fires.
        let row = once(at, at);
        assert_eq!(decide(&row, at, zone), Decision::Fire(at));
        let early = once(at, at - 600);
        assert_eq!(decide(&early, at - 1, zone), Decision::Wait(at));
        assert_eq!(decide(&early, at + 90, zone), Decision::Fire(at));
        // Caught up a few hours late, skipped a day late, never twice.
        assert_eq!(decide(&early, at + 5 * 3600, zone), Decision::Fire(at));
        assert!(
            matches!(decide(&early, at + 86_400 + 1, zone), Decision::Skip(slot, _) if slot == at)
        );
        let mut done = early.clone();
        done.last_slot = Some(at);
        assert_eq!(decide(&done, at + 10, zone), Decision::Idle);
        // Resumed after it passed: done, nothing banked.
        let mut paused = early.clone();
        paused.paused = true;
        assert_eq!(decide(&paused, at + 10, zone), Decision::Idle);
        paused.paused = false;
        paused.last_slot = Some(at + 600);
        assert_eq!(decide(&paused, at + 700, zone), Decision::Idle);
        assert_eq!(next_at(&done, at, zone), None);
        assert_eq!(next_at(&early, at - 5, zone), Some(at));
    }

    #[test]
    fn the_next_run_of_a_list_is_none_when_paused() {
        let zone = plus3();
        let now = utc(2026, 10, 22, 5, 0);
        let mut row = repeat("0 9 * * *", now - 86_400, None);
        assert_eq!(next_at(&row, now, zone), Some(utc(2026, 10, 22, 6, 0)));
        row.paused = true;
        assert_eq!(next_at(&row, now, zone), None);
    }

    #[test]
    fn the_words_for_a_timing_are_the_same_everywhere() {
        let zone = plus3();
        let words = |text: &str| when_words(&repeat(text, 0, None), zone);
        assert_eq!(words("0 9 * * *"), "every day 09:00");
        assert_eq!(words("30 8 * * 1-5"), "weekdays 08:30");
        assert_eq!(words("30 8 * * 1"), "mondays 08:30");
        assert_eq!(words("0 18 * * 0"), "sundays 18:00");
        assert_eq!(words("0 18 * * 7"), "sundays 18:00");
        assert_eq!(words("0 */2 * * *"), "every 2h");
        assert_eq!(words("*/15 * * * *"), "every 15m");
        assert_eq!(words("5 */2 * * *"), "5 */2 * * *");
        assert_eq!(words("0 9 1 * *"), "0 9 1 * *");
        let one = once(utc(2026, 10, 24, 14, 0), 0);
        assert_eq!(when_words(&one, zone), "once oct 24 17:00");
    }

    #[test]
    fn a_typed_moment_becomes_an_instant() {
        let zone = plus3();
        let now = utc(2026, 10, 22, 9, 0) + 7; // 12:00:07 local
        assert_eq!(parse_at("+3s", now, zone), Ok(now + 3));
        assert_eq!(parse_at("+10m", now, zone), Ok(now + 600));
        assert_eq!(parse_at("+2h", now, zone), Ok(now + 7200));
        assert_eq!(parse_at("17:30", now, zone), Ok(utc(2026, 10, 22, 14, 30)));
        assert_eq!(
            parse_at("9:05", now, zone),
            Ok(utc(2026, 10, 23, 6, 5)),
            "tomorrow"
        );
        assert_eq!(
            parse_at("2026-10-24 08:00", now, zone),
            Ok(utc(2026, 10, 24, 5, 0))
        );
        for bad in [
            "",
            "+0s",
            "+5",
            "+5d",
            "25:00",
            "12:60",
            "2026-10-21 08:00",
            "2026-02-30 08:00",
            "tomorrow",
            "9",
        ] {
            assert!(parse_at(bad, now, zone).is_err(), "{bad:?} was accepted");
        }
    }

    /// The machine's own zone maps a mid-summer instant to a wall minute and back.
    #[test]
    fn the_system_zone_round_trips() {
        let zone = system_zone();
        let at = utc(2026, 6, 15, 12, 0);
        assert_eq!(instant_of(zone, (zone.local)(at)), at);
        let daily = cron("0 9 * * *");
        let slot = next_slot(&daily, at, zone).unwrap();
        assert!(slot > at && slot - at <= 86_400 + 3600);
        assert_eq!((zone.local)(slot).clock(), "09:00");
    }

    #[test]
    fn a_watch_is_checked_at_once_then_every_five_minutes() {
        let zone = plus3();
        let made = utc(2026, 10, 25, 9, 0);
        let row = ScheduleRow {
            cron: None,
            watch: Some("gh issue list".into()),
            ..repeat("", made, None)
        };
        assert_eq!(
            decide(&row, made, zone),
            Decision::Fire(made),
            "the baseline at once"
        );
        let checked = ScheduleRow {
            last_slot: Some(made),
            ..row.clone()
        };
        assert_eq!(
            decide(&checked, made + 60, zone),
            Decision::Wait(made + WATCH_EVERY)
        );
        assert_eq!(next_at(&checked, made + 60, zone), Some(made + WATCH_EVERY));
        // A Mac asleep for a day checks once when it wakes, not once per missed slot.
        assert_eq!(
            decide(&checked, made + 86_400, zone),
            Decision::Fire(made + 86_400)
        );
        // A clock set back an hour: the slot ahead of it is due now.
        assert_eq!(
            decide(&checked, made - 3600, zone),
            Decision::Fire(made - 3600)
        );
        let paused = ScheduleRow {
            paused: true,
            ..checked.clone()
        };
        assert_eq!(decide(&paused, made + 86_400, zone), Decision::Idle);
        assert_eq!(next_at(&paused, made, zone), None);
        assert_eq!(when_words(&checked, zone), "on change, every 5m");
    }
}
