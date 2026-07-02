//! Unix-timestamp formatting and date math.
//!
//! All functions are pure: no `SystemTime` reads, no I/O. The single
//! exception is [`unix_now`], which is the only function that touches
//! the clock and is the one most plugins want for "now" anyway.

/// Returns the current Unix epoch time in seconds.
pub fn unix_now() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Formats a Unix timestamp as `YYYY-MM-DD HH:MM` in UTC.
pub fn format_unix(ts: u64) -> String {
    let (y, m, d, h, mi) = unix_to_ymdhm(ts);
    format!("{:04}-{:02}-{:02} {:02}:{:02}", y, m, d, h, mi)
}

/// Formats a duration in seconds as `Hh Mm Ss` (omitting zero hours).
pub fn format_seconds(s: u64) -> String {
    let h = s / 3600;
    let m = (s % 3600) / 60;
    let sec = s % 60;
    if h > 0 {
        format!("{}h {}m {}s", h, m, sec)
    } else if m > 0 {
        format!("{}m {}s", m, sec)
    } else {
        format!("{}s", sec)
    }
}

/// Converts Unix seconds to a (year, month, day, hour, minute) tuple in
/// UTC. Uses the proleptic Gregorian calendar with proper leap-year rules
/// (so 2000 is a leap year, 2100 is not).
pub fn unix_to_ymdhm(secs: u64) -> (u32, u32, u32, u32, u32) {
    let m = ((secs / 60) % 60) as u32;
    let h = ((secs / 3600) % 24) as u32;
    let days = (secs / 86_400) as i64;

    // Howard Hinnant's `days_from_civil` algorithm, ported to Rust.
    // Returns the Gregorian (y, m, d) for a day count since 1970-01-01.
    let z = days + 719_468;
    let era = if z >= 0 {
        z / 146_097
    } else {
        (z - 146_096) / 146_097
    };
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y_signed = era * 400 + yoe as i64;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m_raw = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = (y_signed + if m_raw <= 2 { 1 } else { 0 }) as u32;

    (year, m_raw, d, h, m)
}

/// Gregorian leap year: divisible by 4, except centuries not divisible by 400.
/// (So 2000 is a leap year, 2100 is not.)
pub fn is_leap(y: u32) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

/// Like [`format_unix`] but with an explicit ` UTC` suffix:
/// `YYYY-MM-DD HH:MM UTC`. Preserves the exact output of the
/// newsletter plugin's historical local helper.
pub fn format_unix_utc(ts: u64) -> String {
    format!("{} UTC", format_unix(ts))
}

/// Formats a duration in seconds as a zero-padded clock string:
/// `MM:SS`, or `HH:MM:SS` once there is at least an hour. This is the
/// style the podcast/video public pages render (`02:05`, `01:02:05`),
/// as opposed to [`format_seconds`]'s prose style (`2m 5s`).
pub fn format_clock(s: u64) -> String {
    let h = s / 3600;
    let m = (s % 3600) / 60;
    let sec = s % 60;
    if h > 0 {
        format!("{:02}:{:02}:{:02}", h, m, sec)
    } else {
        format!("{:02}:{:02}", m, sec)
    }
}

/// Formats a Unix timestamp as an RFC 2822 date-time in UTC, e.g.
/// `Mon, 15 Jan 2024 12:34:56 +0000`. This is the exact `pubDate`
/// format RSS 2.0 readers expect; the output is byte-identical to the
/// plugin-podcast helper it was moved from — do not change it without
/// updating the regression tests below.
pub fn rfc2822_utc(ts: u64) -> String {
    let (y, mo, d, h, mi) = unix_to_ymdhm(ts);
    let s = (ts % 60) as u32;
    let weekday = weekday_short(y, mo, d);
    let month = month_short(mo);
    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} +0000",
        weekday, d, month, y, h, mi, s
    )
}

/// Zeller's congruence, mapped to RFC 2822 three-letter weekday names.
fn weekday_short(y: u32, m: u32, d: u32) -> &'static str {
    let (y2, m2) = if m < 3 { (y - 1, m + 12) } else { (y, m) };
    let k = y2 % 100;
    let j = y2 / 100;
    let h = (d as i32
        + ((13 * (m2 + 1) as i32) / 5)
        + k as i32
        + (k as i32) / 4
        + (j as i32) / 4
        + 5 * j as i32)
        .rem_euclid(7);
    match h {
        0 => "Sat",
        1 => "Sun",
        2 => "Mon",
        3 => "Tue",
        4 => "Wed",
        5 => "Thu",
        _ => "Fri",
    }
}

fn month_short(m: u32) -> &'static str {
    match m {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        12 => "Dec",
        // Months outside 1..=12 are a programmer error — the calling
        // site should be passing a value from `unix_to_ymdhm`, which
        // always returns 1..=12. Fall through to a clear sentinel
        // rather than silently mis-rendering as "Dec".
        _ => "???",
    }
}

/// Human-friendly relative time like "5m ago", "3h ago", "2d ago",
/// "Jan 5" (if older than 60 days), or the year if older than a year.
pub fn relative_time(ts: u64) -> String {
    let now = unix_now();
    if ts > now {
        return format_unix(ts);
    }
    let diff = now - ts;
    if diff < 60 {
        "just now".into()
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86_400 {
        format!("{}h ago", diff / 3600)
    } else if diff < 60 * 86_400 {
        format!("{}d ago", diff / 86_400)
    } else {
        let (y, m, d, _, _) = unix_to_ymdhm(ts);
        let month = match m {
            1 => "Jan",
            2 => "Feb",
            3 => "Mar",
            4 => "Apr",
            5 => "May",
            6 => "Jun",
            7 => "Jul",
            8 => "Aug",
            9 => "Sep",
            10 => "Oct",
            11 => "Nov",
            12 => "Dec",
            _ => "???",
        };
        if y != ((now / 31_536_000) + 1970) as u32 {
            format!("{} {} {}", month, d, y)
        } else {
            format!("{} {}", month, d)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_1970_01_01_00_00() {
        assert_eq!(unix_to_ymdhm(0), (1970, 1, 1, 0, 0));
    }

    #[test]
    fn leap_year_rules() {
        assert!(is_leap(2000));
        assert!(!is_leap(2100));
        assert!(is_leap(2024));
        assert!(!is_leap(2025));
    }

    #[test]
    fn format_unix_known_value() {
        // 2024-01-15 12:34:00 UTC == 1705322040
        assert_eq!(format_unix(1705322040), "2024-01-15 12:34");
    }

    #[test]
    fn format_seconds_hms() {
        assert_eq!(format_seconds(0), "0s");
        assert_eq!(format_seconds(45), "45s");
        assert_eq!(format_seconds(125), "2m 5s");
        assert_eq!(format_seconds(3725), "1h 2m 5s");
    }

    #[test]
    fn format_unix_utc_has_suffix() {
        // 2024-01-15 12:34:00 UTC == 1705322040. Newsletter admin pages
        // have always shown the explicit UTC suffix; keep it byte-exact.
        assert_eq!(format_unix_utc(1705322040), "2024-01-15 12:34 UTC");
    }

    #[test]
    fn format_clock_padding() {
        assert_eq!(format_clock(0), "00:00");
        assert_eq!(format_clock(125), "02:05");
        assert_eq!(format_clock(3725), "01:02:05");
    }

    #[test]
    fn weekday_short_known_dates() {
        // Reference dates (weekday is deterministic).
        assert_eq!(weekday_short(1970, 1, 1), "Thu"); // Unix epoch
        assert_eq!(weekday_short(2000, 1, 1), "Sat");
        assert_eq!(weekday_short(2024, 2, 29), "Thu"); // leap day
        assert_eq!(weekday_short(2025, 1, 1), "Wed");
    }

    #[test]
    fn weekday_short_zellers_century_boundary() {
        // 2100-01-01 is a Friday, NOT a leap year (the bug the old
        // year-loop unix_to_ymdhm implementations would have hit
        // without proper century rules). Pin the weekday computation
        // to a known value so this can never silently regress.
        assert_eq!(weekday_short(2100, 1, 1), "Fri");
        assert_eq!(weekday_short(2000, 1, 1), "Sat"); // 2000 IS a leap year
    }

    #[test]
    fn month_short_all_twelve() {
        let months = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        for (i, expected) in months.iter().enumerate() {
            assert_eq!(month_short(i as u32 + 1), *expected);
        }
        // Out-of-range months produce a sentinel rather than silently
        // rendering as "Dec" — caller is a programmer error.
        assert_eq!(month_short(13), "???");
        assert_eq!(month_short(0), "???");
    }

    #[test]
    fn rfc2822_utc_format() {
        // RSS pubDate regression guard: output must stay byte-identical
        // to the plugin-podcast helper this was moved from.
        // 2024-01-15 12:34:56 UTC == 1705322096
        assert_eq!(
            rfc2822_utc(1_705_322_096),
            "Mon, 15 Jan 2024 12:34:56 +0000"
        );
        // Epoch
        assert_eq!(rfc2822_utc(0), "Thu, 01 Jan 1970 00:00:00 +0000");
    }
}
