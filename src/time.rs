//! Timestamps as the views show them.

use std::borrow::Cow;

use crate::types::Str;

/// How long ago a millisecond Unix timestamp was, as jj phrases it
/// ("5 hours ago").
pub fn relative(millis: i64) -> Str {
    let secs = millis / 1000;
    let nanos = ((millis % 1000) * 1_000_000) as u32;
    match chrono::DateTime::from_timestamp(secs, nanos) {
        Some(dt) => format_relative_time(dt).into(),
        None => "unknown".into(),
    }
}

fn format_relative_time(dt: chrono::DateTime<chrono::Utc>) -> Cow<'static, str> {
    let now = chrono::Utc::now();
    let duration = now.signed_duration_since(dt);

    if duration.num_seconds() < 0 {
        return "just now".into();
    }

    let secs = duration.num_seconds();
    if secs < 60 {
        return if secs == 1 {
            "1 second ago".into()
        } else {
            format!("{secs} seconds ago").into()
        };
    }
    let mins = duration.num_minutes();
    if mins < 60 {
        return if mins == 1 {
            "1 minute ago".into()
        } else {
            format!("{mins} minutes ago").into()
        };
    }
    let hours = duration.num_hours();
    if hours < 24 {
        return if hours == 1 {
            "1 hour ago".into()
        } else {
            format!("{hours} hours ago").into()
        };
    }
    let days = duration.num_days();
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
