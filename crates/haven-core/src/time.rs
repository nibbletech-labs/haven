//! Hand-rolled UTC date/time helpers (no `chrono` dependency).
//!
//! Howard Hinnant's civil/day algorithms, shared by the content layer
//! (date-stamped notes) and the backup layer (snapshot timestamps + rotation).
//! Keeping one implementation here means the two callers can't drift.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch, UTC. Saturates to 0 before 1970 (never panics).
pub(crate) fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64
}

/// Today's date as `(year, month, day)` in UTC.
pub(crate) fn today_ymd() -> (i64, u32, u32) {
    civil_from_days(now_secs().div_euclid(86_400))
}

/// `(year, month, day)` -> `"YYYY-MM-DD"` (the `last_backup` debounce marker).
pub(crate) fn ymd_string((y, m, d): (i64, u32, u32)) -> String {
    format!("{y:04}-{m:02}-{d:02}")
}

/// Unix seconds -> a sortable UTC stamp `"YYYYMMDDTHHMMSSZ"`. Lexical order ==
/// chronological order (so a string sort over snapshot dir names is a time
/// sort), and filesystem-safe — no `:`.
pub(crate) fn utc_stamp(secs: i64) -> String {
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let sod = secs.rem_euclid(86_400);
    let (h, mi, s) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    format!("{y:04}{m:02}{d:02}T{h:02}{mi:02}{s:02}Z")
}

/// Days since 1970-01-01 -> `(year, month, day)`, UTC. Howard Hinnant
/// `civil_from_days`.
pub(crate) fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `(year, month, day)` -> days since 1970-01-01, UTC. Inverse of
/// [`civil_from_days`] (Howard Hinnant `days_from_civil`).
pub(crate) fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// ISO-8601 week date for a `(year, month, day)`: returns
/// `(iso_week_year, week_number)` with `week_number` in `1..=53`. The ISO week
/// year can differ from the calendar year near January/December boundaries.
pub(crate) fn iso_week(y: i64, m: u32, d: u32) -> (i64, u32) {
    let z = days_from_civil(y, m, d);
    // 1970-01-01 (z = 0) is a Thursday; ISO weekday Mon=1 .. Sun=7.
    let iso_dow = (z + 3).rem_euclid(7) + 1;
    // The Thursday of this ISO week determines the week's owning year.
    let thursday = z + (4 - iso_dow);
    let (ty, _, _) = civil_from_days(thursday);
    let jan1 = days_from_civil(ty, 1, 1);
    let week = ((thursday - jan1) / 7 + 1) as u32;
    (ty, week)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_roundtrip_anchors() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(31), (1970, 2, 1));
        assert_eq!(civil_from_days(20_574), (2026, 5, 1));
        // round-trips both ways
        for &(y, m, d) in &[(1970, 1, 1), (2000, 2, 29), (2026, 6, 19), (2027, 1, 1)] {
            assert_eq!(civil_from_days(days_from_civil(y, m, d)), (y, m, d));
        }
    }

    #[test]
    fn utc_stamp_is_sortable_and_safe() {
        // 2026-06-19T00:00:00Z = day 20623 * 86400.
        let z = days_from_civil(2026, 6, 19);
        let s = utc_stamp(z * 86_400 + 13 * 3600 + 5 * 60 + 9);
        assert_eq!(s, "20260619T130509Z");
        assert!(!s.contains(':'));
    }

    #[test]
    fn iso_week_known_values() {
        // 2026-01-01 is a Thursday -> ISO week 1 of 2026.
        assert_eq!(iso_week(2026, 1, 1), (2026, 1));
        // 2027-01-01 is a Friday -> belongs to ISO week 53 of 2026.
        assert_eq!(iso_week(2027, 1, 1), (2026, 53));
        // 2026-12-31 (Thursday) -> ISO week 53 of 2026.
        assert_eq!(iso_week(2026, 12, 31), (2026, 53));
        // mid-year sanity: 2026-06-19 is in week 25.
        assert_eq!(iso_week(2026, 6, 19), (2026, 25));
    }
}
