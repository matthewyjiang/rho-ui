//! What a surface's text and date fields take, and how their values are
//! written to the data model.

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use serde_json::Value;

/// What a field takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    /// One line of text.
    Short,
    /// Several lines of text.
    Long,
    /// A number, kept as the text the user typed.
    Number,
    /// One line that shows as dots, such as a password.
    Obscured,
    /// An ISO 8601 date, time, or both. A field that enables neither
    /// takes both.
    DateTime {
        /// Whether it takes a date.
        date: bool,
        /// Whether it takes a time of day.
        time: bool,
    },
}

/// A date field's value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Moment {
    /// A day.
    Date(NaiveDate),
    /// A time of day.
    Time(NaiveTime),
    /// A day and a time of day.
    DateTime(NaiveDateTime),
}

impl FieldKind {
    /// Why `text` cannot be the field's value, if it cannot. An empty
    /// field is always allowed: whether it may stay empty is for the
    /// surface's own checks to say. Text that cannot be the value is kept
    /// out of the data model.
    pub fn problem(self, text: &str) -> Option<&'static str> {
        if text.is_empty() {
            return None;
        }
        match self {
            Self::Short | Self::Long | Self::Obscured => None,
            Self::Number => text
                .trim()
                .parse::<f64>()
                .map_or(true, |number| !number.is_finite())
                .then_some("Enter a number"),
            Self::DateTime { date, time } => match (self.moment(text), date, time) {
                (Some(_), _, _) => None,
                (None, true, false) => Some("Enter a date as YYYY-MM-DD"),
                (None, false, true) => Some("Enter a time as HH:MM"),
                (None, _, _) => Some("Enter a date and time as YYYY-MM-DDTHH:MM"),
            },
        }
    }

    /// The moment `text` names, for a date field: `2026-01-31`, `09:30`,
    /// or `2026-01-31T09:30`, each time with or without seconds.
    pub fn moment(self, text: &str) -> Option<Moment> {
        let Self::DateTime { date, time } = self else {
            return None;
        };
        let time_of = |text: &str| {
            let format = if text.len() == 8 { "%H:%M:%S" } else { "%H:%M" };
            // chrono takes `9:30`; ISO 8601 pads.
            matches!(text.len(), 5 | 8)
                .then(|| NaiveTime::parse_from_str(text, format).ok())
                .flatten()
        };
        match (date, time) {
            (true, false) => (text.len() == 10)
                .then(|| NaiveDate::parse_from_str(text, "%Y-%m-%d").ok())
                .flatten()
                .map(Moment::Date),
            (false, true) => time_of(text).map(Moment::Time),
            (true, true) | (false, false) => {
                let (day, clock) = text.split_once('T')?;
                let day = (day.len() == 10)
                    .then(|| NaiveDate::parse_from_str(day, "%Y-%m-%d").ok())
                    .flatten()?;
                Some(Moment::DateTime(day.and_time(time_of(clock)?)))
            }
        }
    }
}

impl Moment {
    /// The moment as the data model holds it: ISO 8601, to the minute
    /// unless it has seconds.
    pub fn to_iso(self) -> String {
        let clock = |time: NaiveTime| {
            use chrono::Timelike as _;
            if time.second() == 0 {
                time.format("%H:%M")
            } else {
                time.format("%H:%M:%S")
            }
        };
        match self {
            Self::Date(date) => date.format("%Y-%m-%d").to_string(),
            Self::Time(time) => clock(time).to_string(),
            Self::DateTime(moment) => {
                format!(
                    "{}T{}",
                    moment.date().format("%Y-%m-%d"),
                    clock(moment.time())
                )
            }
        }
    }
}

/// A slider's value as JSON: an integer when it is whole, so a model that
/// asked for 1 to 10 reads `7`, not `7.0`.
pub fn number_value(value: f64) -> Value {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::as_conversions,
            reason = "whole and under 1e15, which i64 holds exactly"
        )]
        let whole = value as i64;
        Value::from(whole)
    } else {
        Value::from(value)
    }
}
