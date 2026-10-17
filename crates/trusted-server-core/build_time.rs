//! The build time written as RFC 3339 in UTC.
//!
//! A module of the build script, which bakes the time into the binary, and of
//! the tests in `src/build_info.rs`, which check the arithmetic against known
//! dates.

/// Formats seconds since the Unix epoch as RFC 3339 in UTC.
///
/// Days are civil days from the epoch, using the standard days-to-civil
/// algorithm, which is correct for any date this will see.
pub(crate) fn rfc3339_utc(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let rem = seconds.rem_euclid(86_400);
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // Days-to-civil: shift the era so March is month 0 and leap days land at
    // the end of a 400 year era.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}Z")
}
