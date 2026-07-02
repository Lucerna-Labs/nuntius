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

/// Fixed-precision variant: always one decimal place for KB/MB and two
/// for GB, and no TB/PB tiers (everything ≥ 1 GB renders as GB). This
/// preserves the exact historical output of the podcast/video plugins'
/// admin size columns ("1.0 KB", not "1 KB") — use [`human_bytes`] for
/// new code.
pub fn human_bytes_fixed(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
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

    #[test]
    fn fixed_variant_keeps_trailing_decimals() {
        // Byte-exact with the old local helpers in plugin-podcast and
        // plugin-video-podcast: fixed decimals, GB is the top tier.
        assert_eq!(human_bytes_fixed(0), "0 B");
        assert_eq!(human_bytes_fixed(1023), "1023 B");
        assert_eq!(human_bytes_fixed(1024), "1.0 KB");
        assert_eq!(human_bytes_fixed(1536), "1.5 KB");
        assert_eq!(human_bytes_fixed(1024 * 1024), "1.0 MB");
        assert_eq!(human_bytes_fixed(1024u64.pow(3)), "1.00 GB");
        assert_eq!(human_bytes_fixed(1024u64.pow(4)), "1024.00 GB");
    }
}
