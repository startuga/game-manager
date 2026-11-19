//! Dates on this PC's clock, for what the window shows (when a game was last played) and for file
//! names (a save backup's). Seconds since 1970 (UTC) become a civil date with Howard Hinnant's
//! days-to-civil (`utc`); Windows' own time zone rules then make it local, daylight saving
//! included (`SystemTimeToTzSpecificLocalTime`). No date crate.

use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::{SYSTEMTIME, SystemTimeToTzSpecificLocalTime};

/// A date and a time of day.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LocalTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl LocalTime {
    /// The date's days since 1970-01-01 (the time of day left out): two dates' difference in days.
    pub fn day_number(&self) -> i64 {
        days_from_civil(
            i64::from(self.year),
            i64::from(self.month),
            i64::from(self.day),
        )
    }
}

/// Seconds since 1970 (UTC), now.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// `unix` seconds since 1970 as a UTC date and time.
pub fn utc(unix: u64) -> LocalTime {
    let seconds = i64::try_from(unix).unwrap_or(i64::MAX);
    let (days, of_day) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let (year, month, day) = civil_from_days(days);
    LocalTime {
        year: u16::try_from(year).unwrap_or(u16::MAX),
        month: month as u8,
        day: day as u8,
        hour: (of_day / 3600) as u8,
        minute: (of_day / 60 % 60) as u8,
        second: (of_day % 60) as u8,
    }
}

/// `unix` seconds since 1970 on this PC's clock; `None` when Windows can't convert it.
pub fn local(unix: u64) -> Option<LocalTime> {
    let at = utc(unix);
    let universal = SYSTEMTIME {
        wYear: at.year,
        wMonth: u16::from(at.month),
        wDayOfWeek: 0,
        wDay: u16::from(at.day),
        wHour: u16::from(at.hour),
        wMinute: u16::from(at.minute),
        wSecond: u16::from(at.second),
        wMilliseconds: 0,
    };
    let mut converted = SYSTEMTIME::default();
    // SAFETY: two valid SYSTEMTIMEs for the call; no time zone given means this PC's own.
    let ok = unsafe { SystemTimeToTzSpecificLocalTime(None, &universal, &mut converted) };
    ok.as_bool().then_some(LocalTime {
        year: converted.wYear,
        month: converted.wMonth as u8,
        day: converted.wDay as u8,
        hour: converted.wHour as u8,
        minute: converted.wMinute as u8,
        second: converted.wSecond as u8,
    })
}

/// Howard Hinnant's days-to-civil: (year, month, day) of `days` since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// The other way: days since 1970-01-01 of a civil date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> LocalTime {
        LocalTime {
            year,
            month,
            day,
            hour,
            minute,
            second,
        }
    }

    #[test]
    fn civil_dates_from_seconds() {
        assert_eq!(utc(0), at(1970, 1, 1, 0, 0, 0));
        // A leap day, and the last second of a year.
        assert_eq!(utc(951_782_400), at(2000, 2, 29, 0, 0, 0));
        assert_eq!(utc(1_735_689_599), at(2024, 12, 31, 23, 59, 59));
        assert_eq!(utc(1_735_689_600), at(2025, 1, 1, 0, 0, 0));
        assert_eq!(utc(1_795_000_000), at(2026, 11, 18, 11, 6, 40));
    }

    #[test]
    fn day_numbers_count_days() {
        assert_eq!(at(1970, 1, 1, 12, 0, 0).day_number(), 0);
        assert_eq!(
            at(2000, 3, 1, 0, 0, 0).day_number() - at(2000, 2, 28, 0, 0, 0).day_number(),
            2
        );
        for seconds in [0, 951_782_400, 1_735_689_599, 1_795_000_000] {
            assert_eq!(utc(seconds).day_number(), (seconds / 86_400) as i64);
        }
    }

    #[test]
    fn local_time_is_within_a_day_of_utc() {
        let now = now();
        let local = local(now).unwrap();
        assert!((local.day_number() - utc(now).day_number()).abs() <= 1);
    }
}
