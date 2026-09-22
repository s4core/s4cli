//! UTC date conversions for SigV4 timestamps and S3 `LastModified` values.

use std::ops::Range;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// `(year, month, day)` for a count of days since 1970-01-01 (Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let mp = i64::from((month + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `YYYYMMDDTHHMMSSZ`, the SigV4 `x-amz-date` format.
pub fn amz_date(secs: u64) -> String {
    let (year, month, day) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Parses a UTC timestamp like `2030-01-01T00:00:00Z` or `2024-05-01T10:20:30.123Z`.
pub fn parse_iso8601(s: &str) -> Option<u64> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || !s.ends_with('Z')
    {
        return None;
    }
    let num = |r: Range<usize>| -> Option<u32> {
        let part = s.get(r)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let fraction = &s[19..s.len() - 1];
    let fraction_ok = fraction.is_empty()
        || (fraction.len() > 1
            && fraction.starts_with('.')
            && fraction[1..].bytes().all(|c| c.is_ascii_digit()));
    if !fraction_ok
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let days = days_from_civil(i64::from(year), month, day);
    let secs = days * 86_400 + i64::from(hour * 3600 + minute * 60 + second);
    u64::try_from(secs).ok()
}

#[cfg(test)]
mod tests {
    use super::{amz_date, parse_iso8601};

    #[test]
    fn amz_date_formats_known_instants() {
        assert_eq!(amz_date(0), "19700101T000000Z");
        assert_eq!(amz_date(1_369_353_600), "20130524T000000Z");
        assert_eq!(amz_date(1_709_208_000), "20240229T120000Z");
    }

    #[test]
    fn parse_iso8601_accepts_s3_timestamps() {
        assert_eq!(parse_iso8601("2013-05-24T00:00:00Z"), Some(1_369_353_600));
        assert_eq!(
            parse_iso8601("2013-05-24T00:00:00.000Z"),
            Some(1_369_353_600)
        );
        assert_eq!(parse_iso8601("2024-02-29T12:00:00Z"), Some(1_709_208_000));
    }

    #[test]
    fn parse_iso8601_rejects_malformed_values() {
        assert_eq!(parse_iso8601("2030-01-01"), None);
        assert_eq!(parse_iso8601("2030-13-01T00:00:00Z"), None);
        assert_eq!(parse_iso8601("2030-01-01T00:00:00+03:00"), None);
        assert_eq!(parse_iso8601("2030-01-01T00:00:00.Z"), None);
    }

    #[test]
    fn roundtrip_many_days() {
        for day in (0..40_000u64).step_by(37) {
            let secs = day * 86_400 + 3_723;
            let d = amz_date(secs);
            let iso = format!(
                "{}-{}-{}T{}:{}:{}Z",
                &d[0..4],
                &d[4..6],
                &d[6..8],
                &d[9..11],
                &d[11..13],
                &d[13..15]
            );
            assert_eq!(parse_iso8601(&iso), Some(secs), "{iso}");
        }
    }
}
