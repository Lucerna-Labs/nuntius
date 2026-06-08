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
}
