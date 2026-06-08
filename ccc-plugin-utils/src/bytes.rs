//! Human-readable byte sizes. "1.4 KB", "523 MB", "2.1 GB" — at the
//! precision the operator's eye can read at a glance.

const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB", "PB"];

/// Formats a byte count as a short human string using 1024-based units.
/// Trailing `.0` decimals are dropped.
pub fn human_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{} B", bytes);
    }
    let mut value = bytes as f64;
    let mut unit_idx = 0;
    while value >= 1024.0 && unit_idx < UNITS.len() - 1 {
        value /= 1024.0;
        unit_idx += 1;
    }
    let formatted = if (value - value.round()).abs() < 0.05 {
        format!("{:.0}", value)
    } else {
        format!("{:.1}", value)
    };
    format!("{} {}", formatted, UNITS[unit_idx])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_under_kb() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1023), "1023 B");
    }

    #[test]
    fn kilo_mega_giga() {
        assert_eq!(human_bytes(1024), "1 KB");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(1024 * 1024), "1 MB");
        assert_eq!(human_bytes(1024u64.pow(3)), "1 GB");
    }
}
