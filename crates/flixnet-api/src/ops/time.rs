//! RFC 3339 timestamps as used by Trakt, Jellyfin and Discord, converted to and from
//! unix milliseconds without a date library.

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn number(s: &str) -> Option<i64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Parses `2024-01-15T20:31:00Z`, `2024-01-15T20:31:00.0000000Z` or
/// `2024-01-15T22:31:00+02:00` into unix milliseconds.
pub fn parse_rfc3339(value: &str) -> Option<i64> {
    let value = value.trim();
    let (date, time) = value.split_once(['T', 't', ' '])?;
    let mut date_parts = date.splitn(3, '-');
    let year = number(date_parts.next()?)?;
    let month = number(date_parts.next()?)?;
    let day = number(date_parts.next()?)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    // Split the offset off the clock time.
    let (clock, offset_ms) = if let Some(clock) = time.strip_suffix(['Z', 'z']) {
        (clock, 0)
    } else if let Some(pos) = time.rfind(['+', '-']) {
        let (clock, offset) = time.split_at(pos);
        let sign = if offset.starts_with('-') { -1 } else { 1 };
        let (h, m) = offset[1..].split_once(':').unwrap_or((&offset[1..], "0"));
        (clock, sign * (number(h)? * 60 + number(m)?) * 60_000)
    } else {
        (time, 0)
    };
    let (clock, fraction) = clock.split_once('.').unwrap_or((clock, ""));
    let mut clock_parts = clock.splitn(3, ':');
    let hour = number(clock_parts.next()?)?;
    let minute = number(clock_parts.next()?)?;
    let second = clock_parts.next().map_or(Some(0), number)?;
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let millis = if fraction.is_empty() {
        0
    } else {
        let digits: String = fraction.chars().take(3).collect();
        number(&format!("{digits:0<3}"))?
    };
    let days = days_from_civil(year, month, day);
    Some(((days * 24 + hour) * 60 + minute) * 60_000 + second * 1000 + millis - offset_ms)
}

/// Formats unix milliseconds as `2024-01-15T20:31:00.000Z`.
pub fn format_rfc3339(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let rest = ms.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3_600_000,
        rest / 60_000 % 60,
        rest / 1000 % 60,
        rest % 1000
    )
}

/// `20261030-142501` (UTC), for file names.
pub fn file_stamp(ms: i64) -> String {
    let full = format_rfc3339(ms);
    // "2026-10-30T14:25:01.000Z"
    format!(
        "{}{}{}-{}{}{}",
        &full[0..4],
        &full[5..7],
        &full[8..10],
        &full[11..13],
        &full[14..16],
        &full[17..19]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_and_offsets() {
        let ms = parse_rfc3339("2024-01-15T20:31:00.000Z").unwrap();
        assert_eq!(ms, 1_705_350_660_000);
        assert_eq!(format_rfc3339(ms), "2024-01-15T20:31:00.000Z");
        assert_eq!(parse_rfc3339("2024-01-15T22:31:00+02:00"), Some(ms));
        assert_eq!(parse_rfc3339("2024-01-15T20:31:00.0000000Z"), Some(ms));
        assert_eq!(parse_rfc3339("2024-01-15T20:31:00.5Z"), Some(ms + 500));
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(file_stamp(ms), "20240115-203100");
        assert_eq!(parse_rfc3339("gestern"), None);
        assert_eq!(parse_rfc3339("2024-13-01T00:00:00Z"), None);
    }
}
