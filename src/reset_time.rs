use chrono::{DateTime, Local, Utc};

/// Format a UTC reset time as a human-readable string.
pub fn format_reset_time(resets_at: DateTime<Utc>) -> String {
    format_reset_time_at(resets_at, Utc::now())
}

pub fn format_reset_time_at(resets_at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    if resets_at <= now {
        return "resets soon".to_string();
    }
    let delta = resets_at - now;
    let total_secs = delta.num_seconds().max(0);
    if total_secs < 24 * 3600 {
        let hours = total_secs / 3600;
        let mins = (total_secs % 3600) / 60;
        if hours > 0 {
            format!("resets in {} h {} m", hours, mins)
        } else {
            format!("resets in {} m", mins)
        }
    } else {
        let local: DateTime<Local> = resets_at.into();
        if total_secs < 7 * 24 * 3600 {
            format!("resets {} {}", local.format("%a"), local.format("%H:%M"))
        } else {
            format!("resets {} {}", local.format("%b %d"), local.format("%H:%M"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn format_reset_time_within_day_uses_countdown() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 10, 0, 0).unwrap();
        let resets_at = now + chrono::Duration::hours(3) + chrono::Duration::minutes(15);
        assert_eq!(format_reset_time_at(resets_at, now), "resets in 3 h 15 m");
    }

    #[test]
    fn format_reset_time_within_week_uses_weekday() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 10, 0, 0).unwrap();
        let resets_at = Utc.with_ymd_and_hms(2026, 7, 19, 13, 23, 0).unwrap();
        let formatted = format_reset_time_at(resets_at, now);
        assert!(formatted.starts_with("resets "));
        assert!(!formatted.contains("Jul"));
    }

    #[test]
    fn format_reset_time_beyond_week_uses_date() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 10, 0, 0).unwrap();
        let resets_at = Utc.with_ymd_and_hms(2026, 7, 28, 13, 23, 0).unwrap();
        let formatted = format_reset_time_at(resets_at, now);
        assert!(
            formatted.starts_with("resets Jul 28 "),
            "far reset should use date, got: {formatted}"
        );
    }
}
