//! Timestamps as the views show them.

use std::borrow::Cow;

use crate::types::Str;

/// `timestamp` in its author's zone, spelled by a strftime `format`. An
/// invalid format is an error rather than a panic, so a bad
/// `date_format` is reported when read and can never fail a draw.
pub fn format_at(
    timestamp: jiff::Timestamp,
    tz_offset_seconds: i32,
    format: &str,
) -> Result<String, jiff::Error> {
    let tz = jiff::tz::Offset::from_seconds(tz_offset_seconds).unwrap_or(jiff::tz::Offset::UTC);
    let zoned = timestamp.to_zoned(jiff::tz::TimeZone::fixed(tz));
    jiff::fmt::strtime::BrokenDownTime::from(&zoned).to_string(format)
}

/// How long ago a millisecond Unix timestamp was, as jj phrases it
/// ("5 hours ago").
pub fn relative(millis: i64) -> Str {
    match jiff::Timestamp::from_millisecond(millis) {
        Ok(then) => {
            format_relative_time(jiff::Timestamp::now().duration_since(then).as_secs()).into()
        }
        Err(_) => "unknown".into(),
    }
}

/// `secs` elapsed, in the largest whole unit that fits.
fn format_relative_time(secs: i64) -> Cow<'static, str> {
    if secs < 0 {
        return "just now".into();
    }
    if secs < 60 {
        return if secs == 1 {
            "1 second ago".into()
        } else {
            format!("{secs} seconds ago").into()
        };
    }
    let mins = secs / 60;
    if mins < 60 {
        return if mins == 1 {
            "1 minute ago".into()
        } else {
            format!("{mins} minutes ago").into()
        };
    }
    let hours = secs / 3600;
    if hours < 24 {
        return if hours == 1 {
            "1 hour ago".into()
        } else {
            format!("{hours} hours ago").into()
        };
    }
    let days = secs / 86_400;
    if days < 30 {
        return if days == 1 {
            "1 day ago".into()
        } else {
            format!("{days} days ago").into()
        };
    }
    let months = days / 30;
    if months < 12 {
        return if months == 1 {
            "1 month ago".into()
        } else {
            format!("{months} months ago").into()
        };
    }
    let years = days / 365;
    if years == 1 {
        "1 year ago".into()
    } else {
        format!("{years} years ago").into()
    }
}

#[cfg(test)]
mod format_tests {
    use super::{format_at, format_relative_time};

    #[test]
    fn a_format_is_applied_in_the_authors_zone_or_rejected() {
        let noon_utc = jiff::Timestamp::from_second(12 * 3600).unwrap();
        assert_eq!(format_at(noon_utc, 3600, "%H:%M").unwrap(), "13:00");
        assert_eq!(format_at(noon_utc, 0, "%Y-%m-%d").unwrap(), "1970-01-01");
        assert!(format_at(noon_utc, 0, "%").is_err());
    }

    #[test]
    fn elapsed_time_is_spelled_in_the_largest_whole_unit() {
        assert_eq!(format_relative_time(-5), "just now");
        assert_eq!(format_relative_time(1), "1 second ago");
        assert_eq!(format_relative_time(59), "59 seconds ago");
        assert_eq!(format_relative_time(60), "1 minute ago");
        assert_eq!(format_relative_time(2 * 3600 + 59), "2 hours ago");
        assert_eq!(format_relative_time(86_400), "1 day ago");
        assert_eq!(format_relative_time(45 * 86_400), "1 month ago");
        assert_eq!(format_relative_time(800 * 86_400), "2 years ago");
    }
}
