//! Local wall-clock times for scheduling (YouTube publish times, calendar events), without a
//! date-time crate. On Windows the conversion uses the system time zone, including the
//! daylight-saving rule in force on the date itself; elsewhere times are taken as UTC.

use anyhow::{bail, Result};

/// A local date and time to the minute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LocalTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
}

/// Days since 1970-01-01 for a civil date (proleptic Gregorian).
pub fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

pub fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    ((if m <= 2 { y + 1 } else { y }) as i32, m, d)
}

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        4 | 6 | 9 | 11 => 30,
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        _ => 31,
    }
}

impl LocalTime {
    /// Parses "2026-11-25 18:00", "2026-11-25 6:00 pm" or "2026-11-25T18:00".
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim().to_ascii_lowercase().replace('t', " ");
        let (date, time) = s.split_once(' ').unwrap_or((s.as_str(), ""));
        let d: Vec<&str> = date.split('-').collect();
        if d.len() != 3 {
            bail!("write the date as YYYY-MM-DD, e.g. 2026-11-25 18:00");
        }
        let (year, month, day) = (d[0].parse::<i32>()?, d[1].parse::<u32>()?, d[2].parse::<u32>()?);
        if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
            bail!("{date} isn't a real date");
        }
        let time = time.trim();
        let (clock, pm, am) = if let Some(t) = time.strip_suffix("pm") {
            (t.trim(), true, false)
        } else if let Some(t) = time.strip_suffix("am") {
            (t.trim(), false, true)
        } else {
            (time, false, false)
        };
        let (h, m) = if clock.is_empty() {
            (0, 0)
        } else {
            let (h, m) = clock.split_once(':').unwrap_or((clock, "0"));
            (h.trim().parse::<u32>()?, m.trim().parse::<u32>()?)
        };
        if (pm || am) && !(1..=12).contains(&h) {
            bail!("with am/pm the hour is 1–12");
        }
        let hour = match (pm, am) {
            (true, _) if h == 12 => 12,
            (true, _) => h + 12,
            (_, true) if h == 12 => 0,
            _ => h,
        };
        if hour > 23 || m > 59 {
            bail!("{time} isn't a real time");
        }
        Ok(Self { year, month, day, hour, minute: m })
    }

    /// "2026-11-25 18:00", the form `parse` reads.
    pub fn to_input(&self) -> String {
        format!("{:04}-{:02}-{:02} {:02}:{:02}", self.year, self.month, self.day, self.hour, self.minute)
    }

    /// "Wed 25 Nov 2026, 6:00 PM".
    pub fn label(&self) -> String {
        const WD: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
        const MO: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
        let wd = WD[days_from_civil(self.year, self.month, self.day).rem_euclid(7) as usize];
        let (h12, ap) = match self.hour {
            0 => (12, "AM"),
            1..=11 => (self.hour, "AM"),
            12 => (12, "PM"),
            h => (h - 12, "PM"),
        };
        format!("{wd} {} {} {}, {}:{:02} {ap}", self.day, MO[self.month as usize - 1], self.year, h12, self.minute)
    }

    /// Seconds since 1970 if this wall-clock time were UTC.
    fn as_if_utc(&self) -> i64 {
        days_from_civil(self.year, self.month, self.day) * 86400 + self.hour as i64 * 3600 + self.minute as i64 * 60
    }

    fn from_utc_secs(secs: i64) -> Self {
        let (year, month, day) = civil_from_days(secs.div_euclid(86400));
        let rem = secs.rem_euclid(86400);
        Self { year, month, day, hour: (rem / 3600) as u32, minute: (rem % 3600 / 60) as u32 }
    }

    /// The moment this local time names, as Unix seconds.
    pub fn to_unix(&self) -> i64 {
        sys::local_to_utc(self).map(|u| u.as_if_utc()).unwrap_or_else(|| self.as_if_utc())
    }

    /// The local time at Unix second `secs`.
    pub fn from_unix(secs: i64) -> Self {
        let utc = Self::from_utc_secs(secs);
        sys::utc_to_local(&utc).unwrap_or(utc)
    }

    /// The same time `days` later.
    pub fn plus_days(&self, days: i64) -> Self {
        let (year, month, day) = civil_from_days(days_from_civil(self.year, self.month, self.day) + days);
        Self { year, month, day, ..*self }
    }
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// "2026-11-25T23:00:00Z" (what YouTube's `publishAt` wants).
pub fn rfc3339_utc(secs: i64) -> String {
    let t = LocalTime::from_utc_secs(secs);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", t.year, t.month, t.day, t.hour, t.minute, secs.rem_euclid(60))
}

/// "2026-11-25T18:00:00-05:00": the local time with its offset (for Google Calendar).
pub fn rfc3339_local(secs: i64) -> String {
    let local = LocalTime::from_unix(secs);
    let offset_min = (local.as_if_utc() - (secs - secs.rem_euclid(60))) / 60;
    let sign = if offset_min < 0 { '-' } else { '+' };
    let o = offset_min.abs();
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:00{sign}{:02}:{:02}", local.year, local.month, local.day, local.hour, local.minute, o / 60, o % 60)
}

/// Unix seconds from an RFC 3339 time ("…Z", "…+01:00" or a date "2026-11-25").
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let s = s.trim();
    let date = s.get(0..10)?;
    let d: Vec<i64> = date.split('-').map(|p| p.parse().ok()).collect::<Option<_>>()?;
    if d.len() != 3 {
        return None;
    }
    let days = days_from_civil(d[0] as i32, d[1] as u32, d[2] as u32);
    if s.len() == 10 {
        // An all-day event: midnight local time.
        let (y, m, dd) = civil_from_days(days);
        return Some(LocalTime { year: y, month: m, day: dd, hour: 0, minute: 0 }.to_unix());
    }
    let h: i64 = s.get(11..13)?.parse().ok()?;
    let mi: i64 = s.get(14..16)?.parse().ok()?;
    let sec: i64 = s.get(17..19).and_then(|x| x.parse().ok()).unwrap_or(0);
    let rest = s.get(19..).unwrap_or("");
    let rest = rest.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    let offset = if rest.is_empty() || rest.eq_ignore_ascii_case("z") {
        0
    } else {
        let sign = if rest.starts_with('-') { -1 } else { 1 };
        let oh: i64 = rest.get(1..3)?.parse().ok()?;
        let om: i64 = rest.get(4..6).and_then(|x| x.parse().ok()).unwrap_or(0);
        sign * (oh * 3600 + om * 60)
    };
    Some(days * 86400 + h * 3600 + mi * 60 + sec - offset)
}

#[cfg(windows)]
mod sys {
    use super::LocalTime;

    #[repr(C)]
    #[derive(Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn TzSpecificLocalTimeToSystemTime(tz: *const std::ffi::c_void, local: *const SystemTime, utc: *mut SystemTime) -> i32;
        fn SystemTimeToTzSpecificLocalTime(tz: *const std::ffi::c_void, utc: *const SystemTime, local: *mut SystemTime) -> i32;
    }

    fn to_sys(t: &LocalTime) -> SystemTime {
        SystemTime { year: t.year as u16, month: t.month as u16, day: t.day as u16, hour: t.hour as u16, minute: t.minute as u16, ..Default::default() }
    }

    fn from_sys(s: &SystemTime) -> LocalTime {
        LocalTime { year: s.year as i32, month: s.month as u32, day: s.day as u32, hour: s.hour as u32, minute: s.minute as u32 }
    }

    pub fn local_to_utc(t: &LocalTime) -> Option<LocalTime> {
        let mut out = SystemTime::default();
        // SAFETY: both pointers are valid SYSTEMTIME structs; a null zone means the current one.
        let ok = unsafe { TzSpecificLocalTimeToSystemTime(std::ptr::null(), &to_sys(t), &mut out) };
        (ok != 0).then(|| from_sys(&out))
    }

    pub fn utc_to_local(t: &LocalTime) -> Option<LocalTime> {
        let mut out = SystemTime::default();
        // SAFETY: as above.
        let ok = unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &to_sys(t), &mut out) };
        (ok != 0).then(|| from_sys(&out))
    }
}

#[cfg(not(windows))]
mod sys {
    use super::LocalTime;
    pub fn local_to_utc(_: &LocalTime) -> Option<LocalTime> {
        None
    }
    pub fn utc_to_local(_: &LocalTime) -> Option<LocalTime> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2026, 11, 25), 20782);
        for z in [-1000, 0, 59, 60, 20782, 30000] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
        assert_eq!(rfc3339_utc(1_795_647_600), "2026-11-25T23:00:00Z");
    }

    #[test]
    fn parsing_and_labels() {
        let t = LocalTime::parse("2026-11-25 6:30 pm").unwrap();
        assert_eq!(t, LocalTime { year: 2026, month: 11, day: 25, hour: 18, minute: 30 });
        assert_eq!(t.to_input(), "2026-11-25 18:30");
        assert_eq!(t.label(), "Wed 25 Nov 2026, 6:30 PM");
        assert_eq!(LocalTime::parse("2026-11-25T18:30").unwrap(), t);
        assert_eq!(LocalTime::parse("2026-11-25 12:05 am").unwrap().hour, 0);
        assert_eq!(LocalTime::parse("2026-11-25 12:05 pm").unwrap().hour, 12);
        assert_eq!(LocalTime::parse("2026-11-25").unwrap().hour, 0);
        assert!(LocalTime::parse("2026-02-30 10:00").is_err());
        assert!(LocalTime::parse("25/11/2026").is_err());
        assert!(LocalTime::parse("2026-11-25 25:00").is_err());
        assert!(LocalTime::parse("2026-11-25 13:00 pm").is_err());
        assert_eq!(t.plus_days(7).to_input(), "2026-12-02 18:30");
    }

    #[test]
    fn local_round_trip_and_offsets() {
        let t = LocalTime::parse("2026-07-04 09:15").unwrap();
        let secs = t.to_unix();
        assert_eq!(LocalTime::from_unix(secs), t);
        let s = rfc3339_local(secs);
        assert!(s.starts_with("2026-07-04T09:15:00"), "{s}");
        assert_eq!(parse_rfc3339(&s), Some(secs));
        assert_eq!(parse_rfc3339(&rfc3339_utc(secs)), Some(secs));
        assert_eq!(parse_rfc3339("2026-11-25T23:00:00.000Z"), Some(1_795_647_600));
        assert_eq!(parse_rfc3339("2026-11-25T18:00:00-05:00"), Some(1_795_647_600));
    }
}
