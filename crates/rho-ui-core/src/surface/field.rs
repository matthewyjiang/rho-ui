//! What a surface's text and date fields take, and how a date field's
//! value is written to the data model.

use chrono::{NaiveDate, NaiveDateTime, NaiveTime, Timelike as _};

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
    /// An ISO 8601 date.
    Date,
    /// An ISO 8601 time of day.
    Time,
    /// An ISO 8601 date and time of day.
    DateTime,
}

/// Why typed text cannot be a field's value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    /// A number field holds text that is not a finite number.
    NotANumber,
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
    /// Why typed `text` cannot be the field's value, if it cannot. An
    /// empty field is always allowed: whether it may stay empty is for the
    /// surface's own checks to say. A date field's picker only ever gives
    /// a moment, so only a number field has a problem.
    pub fn problem(self, text: &str) -> Option<Problem> {
        match self {
            Self::Number if !text.is_empty() => text
                .trim()
                .parse::<f64>()
                .map_or(true, |number| !number.is_finite())
                .then_some(Problem::NotANumber),
            Self::Number
            | Self::Short
            | Self::Long
            | Self::Obscured
            | Self::Date
            | Self::Time
            | Self::DateTime => None,
        }
    }

    /// The moment `text` names, for a date field: `2026-01-31`, `09:30`,
    /// or `2026-01-31T09:30`, each time with or without seconds.
    pub fn moment(self, text: &str) -> Option<Moment> {
        let day = |text: &str| {
            (text.len() == 10)
                .then(|| NaiveDate::parse_from_str(text, "%Y-%m-%d").ok())
                .flatten()
        };
        let time = |text: &str| {
            // chrono takes `9:30`; ISO 8601 pads.
            let format = match text.len() {
                5 => "%H:%M",
                8 => "%H:%M:%S",
                _ => return None,
            };
            NaiveTime::parse_from_str(text, format).ok()
        };
        match self {
            Self::Date => day(text).map(Moment::Date),
            Self::Time => time(text).map(Moment::Time),
            Self::DateTime => {
                let (date, clock) = text.split_once('T')?;
                Some(Moment::DateTime(day(date)?.and_time(time(clock)?)))
            }
            Self::Short | Self::Long | Self::Number | Self::Obscured => None,
        }
    }
}

impl Moment {
    /// The moment as the data model holds it: ISO 8601, to the minute
    /// unless it has seconds.
    pub fn to_iso(self) -> String {
        let clock = |time: NaiveTime| {
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
