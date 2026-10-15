//! Date formatting without a timezone database: absolute dates are shown in
//! the author's own offset, relative dates against the current wall clock.

/// Days since 1970-01-01 to (year, month, day). Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `2026-10-14 21:07 -0400`
pub fn format_absolute(secs: i64, offset: i32) -> String {
    let local = secs + offset as i64;
    let days = local.div_euclid(86_400);
    let rem = local.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let (hh, mm) = (rem / 3600, (rem % 3600) / 60);
    let sign = if offset < 0 { '-' } else { '+' };
    let off = offset.abs();
    format!(
        "{y:04}-{m:02}-{d:02} {hh:02}:{mm:02} {sign}{:02}{:02}",
        off / 3600,
        (off % 3600) / 60
    )
}

/// Compact relative age: `now`, `5m`, `3h`, `2d`, `3w`, `4mo`, `2y`.
pub fn format_relative(secs: i64, now: i64) -> String {
    let delta = now - secs;
    if delta < 60 {
        return "now".into();
    }
    let (n, unit) = if delta < 3600 {
        (delta / 60, "m")
    } else if delta < 86_400 {
        (delta / 3600, "h")
    } else if delta < 7 * 86_400 {
        (delta / 86_400, "d")
    } else if delta < 30 * 86_400 {
        (delta / (7 * 86_400), "w")
    } else if delta < 365 * 86_400 {
        (delta / (30 * 86_400), "mo")
    } else {
        (delta / (365 * 86_400), "y")
    };
    format!("{n}{unit}")
}

/// Longer relative age for the details panel: `3 days ago`.
pub fn format_relative_long(secs: i64, now: i64) -> String {
    let delta = now - secs;
    if delta < 60 {
        return "just now".into();
    }
    let (n, unit) = if delta < 3600 {
        (delta / 60, "minute")
    } else if delta < 86_400 {
        (delta / 3600, "hour")
    } else if delta < 7 * 86_400 {
        (delta / 86_400, "day")
    } else if delta < 30 * 86_400 {
        (delta / (7 * 86_400), "week")
    } else if delta < 365 * 86_400 {
        (delta / (30 * 86_400), "month")
    } else {
        (delta / (365 * 86_400), "year")
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_000), (2022, 1, 8));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn absolute_uses_offset() {
        // 2026-10-15 01:07 UTC == 2026-10-14 21:07 -0400
        let secs = 1_792_026_420;
        assert_eq!(format_absolute(secs, -4 * 3600), "2026-10-14 21:07 -0400");
        assert_eq!(format_absolute(secs, 0), "2026-10-15 01:07 +0000");
    }

    #[test]
    fn relative() {
        assert_eq!(format_relative(1000, 1000 + 30), "now");
        assert_eq!(format_relative(1000, 1000 + 3 * 3600), "3h");
        assert_eq!(format_relative(1000, 1000 + 40 * 86_400), "1mo");
        assert_eq!(format_relative_long(1000, 1000 + 86_400), "1 day ago");
    }
}
