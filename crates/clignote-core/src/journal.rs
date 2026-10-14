//! Journal date and file-name logic (org-journal inspired).
//!
//! Pure functions only: scanning directories and opening files is the
//! caller's job, so this stays headless and I/O-free.

const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    Year,
    Month,
    Day,
    Lit(String),
}

/// Daily file-name pattern (without the `.org` extension), built from
/// `%Y`, `%m`, `%d` and literal text; `%%` is a literal percent sign.
/// It must contain each of year, month and day exactly once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFormat(Vec<Piece>);

impl FileFormat {
    pub fn parse(pattern: &str) -> Result<Self, String> {
        let mut pieces = Vec::new();
        let mut lit = String::new();
        let mut chars = pattern.chars();
        while let Some(c) = chars.next() {
            if c != '%' {
                lit.push(c);
                continue;
            }
            let piece = match chars.next() {
                Some('Y') => Piece::Year,
                Some('m') => Piece::Month,
                Some('d') => Piece::Day,
                Some('%') => {
                    lit.push('%');
                    continue;
                }
                other => {
                    return Err(format!(
                        "unsupported specifier %{} in file_format (use %Y %m %d)",
                        other.map(String::from).unwrap_or_default()
                    ))
                }
            };
            if !lit.is_empty() {
                pieces.push(Piece::Lit(std::mem::take(&mut lit)));
            }
            pieces.push(piece);
        }
        if !lit.is_empty() {
            pieces.push(Piece::Lit(lit));
        }
        for want in [Piece::Year, Piece::Month, Piece::Day] {
            if pieces.iter().filter(|p| **p == want).count() != 1 {
                return Err("file_format must contain %Y, %m and %d exactly once".into());
            }
        }
        Ok(Self(pieces))
    }
}

impl Default for FileFormat {
    fn default() -> Self {
        Self(vec![Piece::Year, Piece::Month, Piece::Day])
    }
}

/// A calendar day. Ordering is chronological.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        _ => 28,
    }
}

impl Date {
    /// `None` if the combination is not a real calendar day.
    pub fn new(year: i32, month: u32, day: u32) -> Option<Self> {
        if !(1..=9999).contains(&year) || !(1..=12).contains(&month) {
            return None;
        }
        if day == 0 || day > days_in_month(year, month) {
            return None;
        }
        Some(Self { year, month, day })
    }

    /// Day name, computed with Sakamoto's algorithm.
    pub fn weekday(self) -> &'static str {
        const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
        let y = if self.month < 3 {
            self.year - 1
        } else {
            self.year
        };
        let idx = (y + y / 4 - y / 100 + y / 400 + T[(self.month - 1) as usize] + self.day as i32)
            .rem_euclid(7);
        WEEKDAYS[idx as usize]
    }

    /// Daily journal file name, e.g. `20260305.org` for the default format.
    pub fn file_name(self, fmt: &FileFormat) -> String {
        let mut out = String::new();
        for piece in &fmt.0 {
            match piece {
                Piece::Year => out.push_str(&format!("{:04}", self.year)),
                Piece::Month => out.push_str(&format!("{:02}", self.month)),
                Piece::Day => out.push_str(&format!("{:02}", self.day)),
                Piece::Lit(s) => out.push_str(s),
            }
        }
        out.push_str(".org");
        out
    }

    /// Inverse of [`Date::file_name`]; `None` for anything else.
    pub fn from_file_name(name: &str, fmt: &FileFormat) -> Option<Self> {
        let mut rest = name.strip_suffix(".org")?;
        let (mut year, mut month, mut day) = (None, None, None);
        for piece in &fmt.0 {
            match piece {
                Piece::Lit(s) => rest = rest.strip_prefix(s.as_str())?,
                _ => {
                    let width = if *piece == Piece::Year { 4 } else { 2 };
                    let digits = rest.get(..width)?;
                    if !digits.bytes().all(|b| b.is_ascii_digit()) {
                        return None;
                    }
                    let n: u32 = digits.parse().ok()?;
                    rest = &rest[width..];
                    match piece {
                        Piece::Year => year = Some(n as i32),
                        Piece::Month => month = Some(n),
                        _ => day = Some(n),
                    }
                }
            }
        }
        if !rest.is_empty() {
            return None;
        }
        Self::new(year?, month?, day?)
    }

    /// Top-level day headline, e.g. `* Wednesday, 14 October 2026`.
    pub fn headline(self) -> String {
        format!(
            "* {}, {} {} {}",
            self.weekday(),
            self.day,
            MONTHS[(self.month - 1) as usize],
            self.year
        )
    }
}

/// Timed entry headline, e.g. `** 14:05 `, ready for the title to be typed.
pub fn timed_headline(hour: u32, minute: u32) -> String {
    format!("** {hour:02}:{minute:02} ")
}

/// The date strictly after (`forward`) or before `current` in `dates`.
/// `dates` must be sorted ascending; gaps between days are skipped.
pub fn neighbor(dates: &[Date], current: Date, forward: bool) -> Option<Date> {
    if forward {
        dates.iter().copied().find(|d| *d > current)
    } else {
        dates.iter().copied().rev().find(|d| *d < current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> Date {
        Date::new(y, m, day).expect("valid date")
    }

    #[test]
    fn rejects_invalid_dates() {
        assert!(Date::new(2026, 2, 29).is_none());
        assert!(Date::new(2024, 2, 29).is_some());
        assert!(Date::new(2026, 13, 1).is_none());
        assert!(Date::new(2026, 4, 31).is_none());
        assert!(Date::new(2026, 1, 0).is_none());
    }

    #[test]
    fn weekday_known_dates() {
        assert_eq!(d(2026, 10, 14).weekday(), "Wednesday");
        assert_eq!(d(2000, 1, 1).weekday(), "Saturday");
        assert_eq!(d(2024, 2, 29).weekday(), "Thursday");
    }

    #[test]
    fn file_name_round_trip() {
        let date = d(2026, 3, 5);
        let f = FileFormat::default();
        assert_eq!(date.file_name(&f), "20260305.org");
        assert_eq!(Date::from_file_name("20260305.org", &f), Some(date));
        assert_eq!(Date::from_file_name("20260305.txt", &f), None);
        assert_eq!(Date::from_file_name("notes.org", &f), None);
        assert_eq!(Date::from_file_name("20261340.org", &f), None);
    }

    #[test]
    fn custom_file_format() {
        let f = FileFormat::parse("%Y-%m-%d").unwrap();
        let date = d(2026, 3, 5);
        assert_eq!(date.file_name(&f), "2026-03-05.org");
        assert_eq!(Date::from_file_name("2026-03-05.org", &f), Some(date));
        assert_eq!(Date::from_file_name("20260305.org", &f), None);
        assert_eq!(Date::from_file_name("2026-03-05-x.org", &f), None);
        assert!(FileFormat::parse("%Y-%m").is_err());
        assert!(FileFormat::parse("%Y-%m-%d-%d").is_err());
        assert!(FileFormat::parse("%Y-%m-%H").is_err());
    }

    #[test]
    fn headlines() {
        assert_eq!(d(2026, 10, 14).headline(), "* Wednesday, 14 October 2026");
        assert_eq!(timed_headline(9, 5), "** 09:05 ");
    }

    #[test]
    fn neighbor_skips_gaps() {
        let dates = [d(2026, 1, 1), d(2026, 1, 5), d(2026, 2, 1)];
        assert_eq!(neighbor(&dates, d(2026, 1, 1), true), Some(d(2026, 1, 5)));
        assert_eq!(neighbor(&dates, d(2026, 1, 3), true), Some(d(2026, 1, 5)));
        assert_eq!(neighbor(&dates, d(2026, 1, 5), false), Some(d(2026, 1, 1)));
        assert_eq!(neighbor(&dates, d(2026, 2, 1), true), None);
        assert_eq!(neighbor(&dates, d(2026, 1, 1), false), None);
    }
}
