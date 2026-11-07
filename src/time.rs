//! The little UTC arithmetic the client needs (SigV4 timestamps, credential expiry),
//! without a date/time dependency.
use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch.
pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// `(year, month, day, hour, minute, second)` of a Unix time, UTC.
pub fn civil(epoch: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = epoch.div_euclid(86_400);
    let secs = epoch.rem_euclid(86_400);
    // Howard Hinnant's days_from_civil, inverted.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day, (secs / 3600) as u32, (secs % 3600 / 60) as u32, (secs % 60) as u32)
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `20130524T000000Z`, the SigV4 `x-amz-date` form.
pub fn amz_date(epoch: i64) -> String {
    let (y, mo, d, h, mi, s) = civil(epoch);
    format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z")
}

/// `2013-05-24T00:00:00Z`.
pub fn rfc3339(epoch: i64) -> String {
    let (y, mo, d, h, mi, s) = civil(epoch);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Parses an RFC 3339 timestamp (`2026-11-02T12:00:00Z`, fractional seconds and
/// `+09:00`-style offsets accepted) into Unix seconds.
pub fn parse_rfc3339(text: &str) -> Option<i64> {
    let t = text.trim();
    let num = |s: &str| s.parse::<i64>().ok();
    if !t.is_ascii()
        || t.len() < 20
        || t.as_bytes()[4] != b'-'
        || t.as_bytes()[7] != b'-'
        || !matches!(t.as_bytes()[10], b'T' | b't' | b' ')
    {
        return None;
    }
    let (year, month, day) = (num(&t[0..4])?, num(&t[5..7])?, num(&t[8..10])?);
    let (hour, minute, second) = (num(&t[11..13])?, num(&t[14..16])?, num(&t[17..19])?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let mut rest = &t[19..];
    if let Some(frac) = rest.strip_prefix('.') {
        let digits = frac.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        rest = &frac[digits..];
    }
    let offset = match rest {
        "Z" | "z" => 0,
        _ if rest.len() == 6 && (rest.starts_with('+') || rest.starts_with('-')) && &rest[3..4] == ":" => {
            let o = num(&rest[1..3])? * 3600 + num(&rest[4..6])? * 60;
            if rest.starts_with('-') { -o } else { o }
        }
        _ => return None,
    };
    let days = days_from_civil(year, month as u32, day as u32);
    Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        assert_eq!(amz_date(1_369_353_600), "20130524T000000Z");
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        for t in [0, 951_782_400, 1_369_353_600, 1_793_000_123, 4_102_444_800] {
            assert_eq!(parse_rfc3339(&rfc3339(t)), Some(t));
        }
        assert_eq!(parse_rfc3339("2013-05-24T09:00:00.123+09:00"), Some(1_369_353_600));
        assert_eq!(parse_rfc3339("2013-05-23T23:00:00-01:00"), Some(1_369_353_600));
        for bad in [
            "",
            "2013-05-24",
            "2013-05-24T00:00:00日",
            "2013-13-01T00:00:00Z",
            "2013-05-24T00:00:00",
            "2013-05-24T00:00:00.Z",
        ] {
            assert_eq!(parse_rfc3339(bad), None, "{bad}");
        }
    }
}
