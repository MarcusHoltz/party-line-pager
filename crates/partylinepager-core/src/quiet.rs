//! Quiet hours ("blacklist hours").
//!
//! One window per subscriber, expressed in that subscriber's timezone. A window
//! may wrap past midnight (`23:00-07:00`). The start is inclusive and the end is
//! exclusive, so `23:00-07:00` silences 23:00:00 through 06:59:59.
//!
//! An invite that lands inside the window is dropped for that subscriber with no
//! record and no catch-up, which is the whole point: the daemon never
//! accumulates a list of who was asleep.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// A recurring daily window of silence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuietWindow {
    pub start: NaiveTime,
    pub end: NaiveTime,
}

impl QuietWindow {
    pub fn new(start: NaiveTime, end: NaiveTime) -> Self {
        Self { start, end }
    }

    /// True when `t` falls inside the window, handling windows that wrap past
    /// midnight. A zero-length window (`start == end`) silences nothing.
    pub fn contains(&self, t: NaiveTime) -> bool {
        if self.start == self.end {
            false
        } else if self.start < self.end {
            t >= self.start && t < self.end
        } else {
            t >= self.start || t < self.end
        }
    }

    /// True when `now`, converted into `tz`, falls inside the window.
    pub fn is_quiet_at(&self, tz: Tz, now: DateTime<Utc>) -> bool {
        let local = tz.from_utc_datetime(&now.naive_utc());
        self.contains(local.time())
    }
}

impl FromStr for QuietWindow {
    type Err = Error;

    /// Parses `HH:MM-HH:MM`, tolerating whitespace around the dash.
    fn from_str(s: &str) -> Result<Self> {
        let (start, end) = s
            .split_once('-')
            .ok_or_else(|| Error::QuietWindow(format!("{s:?} is not HH:MM-HH:MM")))?;
        Ok(Self {
            start: parse_time(start)?,
            end: parse_time(end)?,
        })
    }
}

fn parse_time(s: &str) -> Result<NaiveTime> {
    let s = s.trim();
    NaiveTime::parse_from_str(s, "%H:%M")
        .or_else(|_| NaiveTime::parse_from_str(s, "%H:%M:%S"))
        .map_err(|_| Error::QuietWindow(format!("{s:?} is not a HH:MM time")))
}

impl fmt::Display for QuietWindow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}-{}",
            self.start.format("%H:%M"),
            self.end.format("%H:%M")
        )
    }
}

impl Serialize for QuietWindow {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for QuietWindow {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

/// Resolves an IANA timezone name such as `America/Denver`.
pub fn parse_timezone(name: &str) -> Result<Tz> {
    name.trim()
        .parse::<Tz>()
        .map_err(|_| Error::Timezone(name.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    #[test]
    fn parses_and_renders() {
        let w: QuietWindow = "23:00-07:00".parse().unwrap();
        assert_eq!(w.start, t(23, 0));
        assert_eq!(w.end, t(7, 0));
        assert_eq!(w.to_string(), "23:00-07:00");
    }

    #[test]
    fn tolerates_spacing_and_seconds() {
        assert_eq!(
            " 09:00 - 17:30 ".parse::<QuietWindow>().unwrap(),
            QuietWindow::new(t(9, 0), t(17, 30))
        );
        assert!("09:00:00-17:00:00".parse::<QuietWindow>().is_ok());
    }

    #[test]
    fn rejects_garbage() {
        assert!("".parse::<QuietWindow>().is_err());
        assert!("23:00".parse::<QuietWindow>().is_err());
        assert!("25:00-07:00".parse::<QuietWindow>().is_err());
        assert!("bedtime-morning".parse::<QuietWindow>().is_err());
    }

    #[test]
    fn same_day_window_is_half_open() {
        let w = QuietWindow::new(t(9, 0), t(17, 0));
        assert!(!w.contains(t(8, 59)));
        assert!(w.contains(t(9, 0)));
        assert!(w.contains(t(16, 59)));
        assert!(!w.contains(t(17, 0)));
    }

    #[test]
    fn wrapping_window_covers_midnight() {
        let w = QuietWindow::new(t(23, 0), t(7, 0));
        assert!(w.contains(t(23, 0)));
        assert!(w.contains(t(0, 0)));
        assert!(w.contains(t(3, 30)));
        assert!(w.contains(t(6, 59)));
        assert!(!w.contains(t(7, 0)));
        assert!(!w.contains(t(12, 0)));
    }

    #[test]
    fn zero_length_window_silences_nothing() {
        let w = QuietWindow::new(t(9, 0), t(9, 0));
        assert!(!w.contains(t(9, 0)));
        assert!(!w.contains(t(0, 0)));
    }

    #[test]
    fn respects_the_subscribers_timezone() {
        let w = QuietWindow::new(t(23, 0), t(7, 0));
        let denver = parse_timezone("America/Denver").unwrap();
        let tokyo = parse_timezone("Asia/Tokyo").unwrap();

        // 2026-08-11 06:00Z is 00:00 in Denver (asleep) and 15:00 in Tokyo (awake).
        let now = Utc.with_ymd_and_hms(2026, 8, 11, 6, 0, 0).unwrap();
        assert!(w.is_quiet_at(denver, now));
        assert!(!w.is_quiet_at(tokyo, now));
    }

    #[test]
    fn timezone_names_are_validated() {
        assert!(parse_timezone("America/Denver").is_ok());
        assert!(parse_timezone(" UTC ").is_ok());
        assert!(parse_timezone("Mars/Olympus").is_err());
    }
}
