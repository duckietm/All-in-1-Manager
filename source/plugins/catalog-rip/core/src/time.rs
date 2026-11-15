//! A point in time passed in by the caller (the core never reads a clock), formatted in UTC.

/// Seconds since 1970-01-01T00:00:00Z.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp {
    pub unix_secs: i64,
}

impl Timestamp {
    pub fn new(unix_secs: i64) -> Self {
        Self { unix_secs }
    }

    /// (year, month 1-12, day 1-31, hour, minute, second) in UTC.
    pub fn civil(self) -> (i64, u32, u32, u32, u32, u32) {
        let days = self.unix_secs.div_euclid(86_400);
        let secs = self.unix_secs.rem_euclid(86_400) as u32;
        let (y, m, d) = civil_from_days(days);
        (y, m, d, secs / 3600, secs % 3600 / 60, secs % 60)
    }

    /// `YYYY-mm-dd_HH-MM-SS` in UTC, the timestamp in a rip's folder name.
    pub fn folder_stamp(self) -> String {
        let (y, mo, d, h, mi, s) = self.civil();
        format!("{y:04}-{mo:02}-{d:02}_{h:02}-{mi:02}-{s:02}")
    }
}

/// Days since 1970-01-01 -> (year, month, day), proleptic Gregorian (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_instants() {
        let f = |s| Timestamp::new(s).folder_stamp();
        assert_eq!(f(0), "1970-01-01_00-00-00");
        assert_eq!(f(86_399), "1970-01-01_23-59-59");
        assert_eq!(f(951_782_400), "2000-02-29_00-00-00", "leap day of a 400-year leap year");
        assert_eq!(f(951_868_800), "2000-03-01_00-00-00");
        assert_eq!(f(1_709_210_096), "2024-02-29_12-34-56", "leap day");
        assert_eq!(f(1_700_000_000), "2023-11-14_22-13-20");
        assert_eq!(f(4_102_444_799), "2099-12-31_23-59-59");
        assert_eq!(f(-1), "1969-12-31_23-59-59", "before the epoch");
        assert_eq!(f(-86_400 * 365), "1969-01-01_00-00-00");
    }
}
