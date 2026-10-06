//! RFC3339 timestamps without a date crate.
//!
//! The workspace carries no chrono/jiff, and a transcript reader must not
//! drag one in for a single conversion. This parses the shapes the four
//! products actually write (`Z` and numeric offsets, optional fraction) with
//! Howard Hinnant's days-from-civil algorithm. Anything else is `None`.

/// `2026-06-10T14:32:00.000Z` → epoch millis. `None` when unparseable.
pub fn rfc3339_to_millis(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    // Minimum: 20 chars for `YYYY-MM-DDTHH:MM:SSZ`.
    if b.len() < 20 {
        return None;
    }
    let num =
        |from: usize, len: usize| -> Option<i64> { s.get(from..from + len)?.parse::<i64>().ok() };
    if b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let (y, mo, d) = (num(0, 4)?, num(5, 2)?, num(8, 2)?);
    let (h, mi, sec) = (num(11, 2)?, num(14, 2)?, num(17, 2)?);
    if !(1..=12).contains(&mo) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    if d < 1 || d > days_in_month(y, mo) {
        return None;
    }
    // Fraction: `.000`, truncated or padded to millis.
    let mut rest = &s[19..];
    let mut millis: i64 = 0;
    if let Some(frac) = rest.strip_prefix('.') {
        let end = frac
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(frac.len());
        if end == 0 {
            return None;
        }
        let mut padded = frac[..end.min(3)].to_string();
        while padded.len() < 3 {
            padded.push('0');
        }
        millis = padded.parse().ok()?;
        rest = &frac[end..];
    }
    // Offset: `Z` or `±HH:MM` / `±HHMM` / `±HH`.
    let offset_secs: i64 = if rest == "Z" || rest == "z" {
        0
    } else {
        let (sign, digits) = match rest.strip_prefix('+') {
            Some(d) => (1i64, d),
            None => (-1i64, rest.strip_prefix('-')?),
        };
        let digits: String = digits.chars().filter(|c| *c != ':').collect();
        if digits.len() != 2 && digits.len() != 4 {
            return None;
        }
        let oh: i64 = digits.get(..2)?.parse().ok()?;
        let om: i64 = if digits.len() == 4 {
            digits.get(2..)?.parse().ok()?
        } else {
            0
        };
        if oh > 23 || om > 59 {
            return None;
        }
        sign * (oh * 3600 + om * 60)
    };
    let days = days_from_civil(y, mo, d)?;
    let secs = days
        .checked_mul(86_400)?
        .checked_add(h * 3600 + mi * 60 + sec)?
        .checked_sub(offset_secs)?;
    secs.checked_mul(1000)?.checked_add(millis)
}

/// Epoch millis as a UTC day, `YYYY-MM-DD` (the inverse of `days_from_civil`).
pub fn date_utc(ms: i64) -> String {
    let z = ms.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}
/// Days since 1970-01-01 for a civil date. `None` outside a sane range.
fn days_from_civil(y: i64, m: i64, d: i64) -> Option<i64> {
    if !(1970..=9999).contains(&y) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9).rem_euclid(12);
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146097 + doe - 719468)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_zero() {
        assert_eq!(rfc3339_to_millis("1970-01-01T00:00:00Z"), Some(0));
    }

    #[test]
    fn fraction_shapes() {
        assert_eq!(
            rfc3339_to_millis("2026-06-10T14:32:00.000Z"),
            rfc3339_to_millis("2026-06-10T14:32:00Z")
        );
        assert_eq!(
            rfc3339_to_millis("2026-06-10T14:32:00.5Z"),
            rfc3339_to_millis("2026-06-10T14:32:00.500Z")
        );
        // Extra precision truncates to millis.
        assert_eq!(
            rfc3339_to_millis("2026-06-10T14:32:00.123456Z"),
            rfc3339_to_millis("2026-06-10T14:32:00.123Z")
        );
    }

    #[test]
    fn numeric_offset_shifts() {
        assert_eq!(
            rfc3339_to_millis("2026-06-10T16:32:00+02:00"),
            rfc3339_to_millis("2026-06-10T14:32:00Z")
        );
        assert_eq!(
            rfc3339_to_millis("2026-06-10T14:32:00+00:00"),
            rfc3339_to_millis("2026-06-10T14:32:00Z")
        );
    }

    #[test]
    fn garbage_is_none() {
        for bad in [
            "",
            "yesterday",
            "2026-13-01T00:00:00Z",
            "2026-06-10 14:32:00",
            "nightly",
        ] {
            assert_eq!(rfc3339_to_millis(bad), None, "{bad}");
        }
    }

    #[test]
    fn impossible_calendar_dates_are_none() {
        assert_eq!(rfc3339_to_millis("2026-02-30T00:00:00Z"), None);
        assert_eq!(rfc3339_to_millis("2026-02-29T00:00:00Z"), None);
        assert!(rfc3339_to_millis("2024-02-29T00:00:00Z").is_some());
    }
}
