//! Dates without a clock or time zone database (the core does no I/O).
//!
//! A date is a millisecond timestamp plus a flag: *naive* dates (no zone in the input, as in Excel)
//! are kept in their own wall-clock time; zoned dates are normalized to UTC (spec §5).

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DateTime {
    /// Milliseconds since 1970-01-01T00:00:00 (UTC, or wall clock for naive dates).
    pub ms: i64,
    pub naive: bool,
    /// False for date-only inputs (`2024-05-17`): they print without a time part.
    pub has_time: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fields {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub millis: u32,
}

const DAY_MS: i64 = 86_400_000;

/// Days since 1970-01-01 for a proleptic Gregorian date (H. Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
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

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                29
            } else {
                28
            }
        }
    }
}

impl DateTime {
    pub fn from_ms(ms: i64, naive: bool) -> Self {
        DateTime { ms, naive, has_time: true }
    }

    pub fn fields(&self) -> Fields {
        let days = self.ms.div_euclid(DAY_MS);
        let rem = self.ms.rem_euclid(DAY_MS);
        let (year, month, day) = civil_from_days(days);
        Fields {
            year,
            month,
            day,
            hour: (rem / 3_600_000) as u32,
            minute: (rem / 60_000 % 60) as u32,
            second: (rem / 1000 % 60) as u32,
            millis: (rem % 1000) as u32,
        }
    }

    /// Parses ISO 8601: `YYYY`, `YYYY-MM`, `YYYY-MM-DD`, optionally `THH:MM[:SS[.fff]]` (or a space
    /// instead of `T`) and a zone `Z` / `±HH:MM` / `±HHMM` / `±HH`.
    pub fn parse(s: &str) -> Option<DateTime> {
        let s = s.trim();
        let b = s.as_bytes();
        let mut i = 0usize;
        let num = |i: &mut usize, n: usize| -> Option<i64> {
            if *i + n > b.len() || !b[*i..*i + n].iter().all(u8::is_ascii_digit) {
                return None;
            }
            let v = s[*i..*i + n].parse().ok()?;
            *i += n;
            Some(v)
        };
        let year = num(&mut i, 4)?;
        let (mut month, mut day) = (1u32, 1u32);
        if i < b.len() && b[i] == b'-' {
            i += 1;
            month = num(&mut i, 2)? as u32;
            if i < b.len() && b[i] == b'-' {
                i += 1;
                day = num(&mut i, 2)? as u32;
            }
        }
        if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
            return None;
        }
        let (mut h, mut mi, mut sec, mut ms) = (0i64, 0i64, 0i64, 0i64);
        let mut has_time = false;
        if i < b.len() && (b[i] == b'T' || b[i] == b't' || b[i] == b' ') {
            i += 1;
            has_time = true;
            h = num(&mut i, 2)?;
            if i >= b.len() || b[i] != b':' {
                return None;
            }
            i += 1;
            mi = num(&mut i, 2)?;
            if i < b.len() && b[i] == b':' {
                i += 1;
                sec = num(&mut i, 2)?;
                if i < b.len() && (b[i] == b'.' || b[i] == b',') {
                    i += 1;
                    let st = i;
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                    if i == st {
                        return None;
                    }
                    let frac = &s[st..i.min(st + 3)];
                    ms = format!("{frac:0<3}").parse().ok()?;
                }
            }
            if h > 24 || mi > 59 || sec > 59 || (h == 24 && (mi | sec | ms) != 0) {
                return None;
            }
        }
        let mut naive = true;
        let mut offset_min = 0i64;
        if i < b.len() {
            match b[i] {
                b'Z' | b'z' => {
                    naive = false;
                    i += 1;
                }
                b'+' | b'-' => {
                    let sign = if b[i] == b'-' { -1 } else { 1 };
                    i += 1;
                    let oh = num(&mut i, 2)?;
                    let mut om = 0;
                    if i < b.len() && b[i] == b':' {
                        i += 1;
                        om = num(&mut i, 2)?;
                    } else if i + 2 <= b.len() {
                        om = num(&mut i, 2)?;
                    }
                    if oh > 23 || om > 59 {
                        return None;
                    }
                    naive = false;
                    offset_min = sign * (oh * 60 + om);
                }
                _ => return None,
            }
        }
        if i != b.len() {
            return None;
        }
        let total = days_from_civil(year, month, day) * DAY_MS + h * 3_600_000 + mi * 60_000 + sec * 1000 + ms - offset_min * 60_000;
        Some(DateTime { ms: total, naive, has_time: has_time || !naive })
    }

    /// ISO text: `2024-05-17T10:00:00.000Z` (UTC), `2024-05-17T10:00:00.000` (naive) or
    /// `2024-05-17` (naive, date only).
    pub fn to_iso(&self) -> String {
        let f = self.fields();
        let date = if (0..=9999).contains(&f.year) {
            format!("{:04}-{:02}-{:02}", f.year, f.month, f.day)
        } else {
            let sign = if f.year < 0 { '-' } else { '+' };
            format!("{sign}{:06}-{:02}-{:02}", f.year.abs(), f.month, f.day)
        };
        if self.naive && !self.has_time {
            return date;
        }
        let z = if self.naive { "" } else { "Z" };
        format!("{date}T{:02}:{:02}:{:02}.{:03}{z}", f.hour, f.minute, f.second, f.millis)
    }

    /// Formats with tokens `YYYY YY MM M DD D HH H mm m ss s`; `[text]` is literal.
    pub fn format(&self, pattern: &str) -> String {
        let f = self.fields();
        let mut out = String::new();
        let mut rest = pattern;
        while let Some(c) = rest.chars().next() {
            if c == '[' {
                if let Some(end) = rest.find(']') {
                    out.push_str(&rest[1..end]);
                    rest = &rest[end + 1..];
                    continue;
                }
            }
            let tokens: [(&str, String); 12] = [
                ("YYYY", f.year.to_string()),
                ("YY", format!("{:02}", f.year.rem_euclid(100))),
                ("MM", format!("{:02}", f.month)),
                ("M", f.month.to_string()),
                ("DD", format!("{:02}", f.day)),
                ("D", f.day.to_string()),
                ("HH", format!("{:02}", f.hour)),
                ("H", f.hour.to_string()),
                ("mm", format!("{:02}", f.minute)),
                ("m", f.minute.to_string()),
                ("ss", format!("{:02}", f.second)),
                ("s", f.second.to_string()),
            ];
            if let Some((tok, val)) = tokens.iter().find(|(t, _)| rest.starts_with(t)) {
                out.push_str(val);
                rest = &rest[tok.len()..];
            } else {
                out.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_print() {
        let d = DateTime::parse("2024-01-02T03:04:05Z").unwrap();
        assert_eq!(d.to_iso(), "2024-01-02T03:04:05.000Z");
        assert!(!d.naive);
        let d = DateTime::parse("2024-05-17").unwrap();
        assert_eq!(d.to_iso(), "2024-05-17");
        assert_eq!(d.format("DD.MM.YYYY"), "17.05.2024");
        let d = DateTime::parse("2024-05-17T10:00:00+03:00").unwrap();
        assert_eq!(d.to_iso(), "2024-05-17T07:00:00.000Z");
        let d = DateTime::parse("2024-05-17 10:30").unwrap();
        assert_eq!(d.to_iso(), "2024-05-17T10:30:00.000");
        assert_eq!(d.format("[at] H:mm, D/M/YY"), "at 10:30, 17/5/24");
        assert!(DateTime::parse("2024-02-30").is_none());
        assert!(DateTime::parse("not a date").is_none());
        assert_eq!(DateTime::parse("1969-12-31T23:59:59.5Z").unwrap().ms, -500);
    }

    #[test]
    fn civil_roundtrip() {
        for d in [-800_000, -1, 0, 1, 19_000, 2_000_000] {
            let (y, m, dd) = civil_from_days(d);
            assert_eq!(days_from_civil(y, m, dd), d);
        }
    }
}
