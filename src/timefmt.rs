use std::time::{Duration, SystemTime};

use jiff::fmt::friendly::{Designator, SpanPrinter};
use jiff::{Span, SpanRelativeTo, Timestamp};

/// A duration like `90s`, `5m`, `1h 30m` or `1w 2d`, where a day is 24 hours.
pub fn parse_duration(raw: &str) -> Option<Duration> {
    let span = raw.parse::<Span>().ok()?;
    let signed = span.to_duration(SpanRelativeTo::days_are_24_hours()).ok()?;
    Duration::try_from(signed).ok()
}

pub fn format_duration(duration: Duration) -> String {
    SpanPrinter::new()
        .designator(Designator::Compact)
        .unsigned_duration_to_string(&duration)
}

/// An RFC 3339 time; a zone-less time like `2026-10-16 21:04:11` is UTC.
pub fn parse_timestamp(raw: &str) -> Option<SystemTime> {
    let utc = format!("{raw}Z");
    let timestamp = raw
        .parse::<Timestamp>()
        .or_else(|_| utc.parse::<Timestamp>());
    Some(timestamp.ok()?.into())
}

/// RFC 3339 in UTC with milliseconds, like `2026-10-16T21:04:11.392Z`.
pub fn format_timestamp(time: SystemTime) -> String {
    Timestamp::try_from(time)
        .expect("a SystemTime within jiff's range")
        .strftime("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_round_trip_in_the_documented_forms() {
        for (raw, seconds, back) in [
            ("5s", 5, "5s"),
            ("90s", 90, "1m 30s"),
            ("1m", 60, "1m"),
            ("1h30m", 5400, "1h 30m"),
            ("1h", 3600, "1h"),
            ("2d", 172_800, "48h"),
            ("1w", 604_800, "168h"),
            ("1d 12h", 129_600, "36h"),
        ] {
            let duration = parse_duration(raw).unwrap();
            assert_eq!(duration, Duration::from_secs(seconds), "{raw}");
            assert_eq!(format_duration(duration), back, "{raw}");
        }
        for bad in ["", "soon", "-5s", "5"] {
            assert_eq!(parse_duration(bad), None, "{bad}");
        }
    }

    #[test]
    fn timestamps_parse_with_or_without_a_zone_and_format_with_milliseconds() {
        let time = parse_timestamp("2026-10-16T21:04:11.392Z").unwrap();
        assert_eq!(format_timestamp(time), "2026-10-16T21:04:11.392Z");
        let utc = parse_timestamp("2026-10-16T21:04:11Z");
        for same in [
            "2026-10-16T21:04:11Z",
            "2026-10-16 21:04:11",
            "2026-10-16T23:04:11+02:00",
        ] {
            assert_eq!(parse_timestamp(same), utc, "{same}");
        }
        assert_eq!(parse_timestamp("yesterday"), None);
    }
}
