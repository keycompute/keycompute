//! 时间格式化工具
//!
//! API timestamps are UTC. Display helpers stay intentionally compact, while
//! editable values use `chrono` for precise RFC3339/`datetime-local` conversion.

use chrono::{DateTime, NaiveDateTime, SecondsFormat, TimeZone, Timelike, Utc};

/// 将 ISO 8601 时间字符串统一转换为 `YYYY-MM-DD HH:mm UTC` 供 UI 展示。
///
/// - 输入如 `"2024-03-01T12:34:56Z"` → `"2024-03-01 12:34 UTC"`
/// - 带偏移量的输入会先转换为 UTC，避免将 UTC 时间误认为本地时间。
/// - 输入如 `"2024-03-01"` → `"2024-03-01"`
/// - 无法解析时原样返回
pub fn format_time(iso: &str) -> String {
    DateTime::parse_from_rfc3339(iso.trim())
        .map(|value| {
            value
                .with_timezone(&Utc)
                .format("%Y-%m-%d %H:%M UTC")
                .to_string()
        })
        .unwrap_or_else(|_| iso.to_string())
}

/// 同 `format_time`，但接受 `Option<&str>`，None 时返回 "—"。
#[allow(dead_code)]
pub fn format_time_opt(iso: Option<&str>) -> String {
    match iso {
        Some(s) if !s.is_empty() => format_time(s),
        _ => "—".to_string(),
    }
}

/// Convert an API RFC3339 value into the value expected by a
/// `datetime-local` control. Tenant administration treats these controls as
/// UTC and says so in the interface, avoiding hand-edited RFC3339 strings.
pub fn rfc3339_to_datetime_local(value: &str) -> String {
    DateTime::parse_from_rfc3339(value.trim())
        .map(|value| value.with_timezone(&Utc))
        .map(|value| {
            if value.nanosecond() == 0 {
                value.format("%Y-%m-%dT%H:%M:%S").to_string()
            } else {
                value.format("%Y-%m-%dT%H:%M:%S%.f").to_string()
            }
        })
        .unwrap_or_else(|_| value.to_owned())
}

/// Accept either a browser `datetime-local` value (interpreted as UTC) or an
/// existing RFC3339 value and return the canonical API representation.
pub fn datetime_input_to_rfc3339(value: &str) -> Option<String> {
    let value = value.trim();
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Some(
            parsed
                .with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::AutoSi, true),
        );
    }
    [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
    ]
    .iter()
    .find_map(|format| NaiveDateTime::parse_from_str(value, format).ok())
    .map(|value| {
        Utc.from_utc_datetime(&value)
            .to_rfc3339_opts(SecondsFormat::AutoSi, true)
    })
}

#[cfg(test)]
mod datetime_input_tests {
    use super::{datetime_input_to_rfc3339, format_time, rfc3339_to_datetime_local};

    #[test]
    fn display_timestamps_are_normalized_and_labelled_as_utc() {
        assert_eq!(
            format_time("2026-10-19T10:15:00+08:00"),
            "2026-10-19 02:15 UTC"
        );
        assert_eq!(format_time("2026-10-19"), "2026-10-19");
    }

    #[test]
    fn datetime_controls_round_trip_utc_without_exposing_rfc3339() {
        assert_eq!(
            rfc3339_to_datetime_local("2026-10-07T13:39:48Z"),
            "2026-10-07T13:39:48"
        );
        assert_eq!(
            datetime_input_to_rfc3339("2026-10-07T13:39").as_deref(),
            Some("2026-10-07T13:39:00Z")
        );
        assert_eq!(
            datetime_input_to_rfc3339("2026-10-07T21:39:00+08:00").as_deref(),
            Some("2026-10-07T13:39:00Z")
        );
        let precise = "2026-10-07T13:39:48.123456789Z";
        assert_eq!(
            datetime_input_to_rfc3339(&rfc3339_to_datetime_local(precise)).as_deref(),
            Some(precise)
        );
    }
}
