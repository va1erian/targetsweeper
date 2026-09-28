//! Human-readable sizes and local timestamps for the list rows.

use std::time::SystemTime;

/// Formats `bytes` with binary units: `17 B`, `1.2 KB`, `3.4 GB`.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Formats `time` as a local `YYYY-MM-DD HH:MM` stamp.
pub fn local_stamp(time: SystemTime) -> String {
    chrono::DateTime::<chrono::Local>::from(time)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn bytes_keep_their_unit() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
    }

    #[test]
    fn larger_sizes_drop_to_the_biggest_unit() {
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GB");
        assert_eq!(human_size(2 * 1024_u64.pow(4)), "2.0 TB");
    }

    #[test]
    fn a_timestamp_is_formatted_to_the_minute() {
        let stamp = local_stamp(SystemTime::UNIX_EPOCH + Duration::from_secs(0));
        assert_eq!(stamp.len(), 16, "YYYY-MM-DD HH:MM: {stamp}");
        assert_eq!(&stamp[4..5], "-");
        assert_eq!(&stamp[13..14], ":");
    }
}
