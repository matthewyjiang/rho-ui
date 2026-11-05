//! The thread list: a "New thread" command, then every saved session.

use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    sidebar::{Sidebar, SidebarFooter, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem},
    spinner::Spinner,
    tag::Tag,
    ActiveTheme as _, IconName, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, rems, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StyleRefinement, Styled as _,
};
use rho_ui_core::{SessionId, SessionSummary};

use crate::app::RhoApp;

/// Whether a thread needs a look, shown at the end of its row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Attention {
    #[default]
    None,
    Busy,
    /// Approvals waiting, oldest first.
    Approval(usize),
}

/// One sidebar row, prepared for rendering.
pub(crate) struct SessionRow {
    pub(crate) id: SessionId,
    pub(crate) cwd: PathBuf,
    pub(crate) title: SharedString,
    /// Muted secondary text: workspace folder and age.
    pub(crate) detail: SharedString,
    pub(crate) active: bool,
    pub(crate) attention: Attention,
}

/// `creating` counts new threads still starting.
pub(crate) fn render(
    rows: Vec<SessionRow>,
    creating: usize,
    error: Option<String>,
    cx: &mut Context<'_, RhoApp>,
) -> impl IntoElement {
    let empty = rows.is_empty();
    let items = rows.into_iter().map(|row| {
        let SessionRow {
            id,
            cwd,
            title,
            detail,
            active,
            attention,
        } = row;
        // The menu numbers its rows; the suffix carries the session's own ID
        // so UI tests can find a row by session.
        let suffix_id = SharedString::from(format!("session-{id}"));
        SidebarMenuItem::new(title)
            .active(active)
            .label_style(StyleRefinement::default().truncate())
            .suffix(move |_, cx| {
                h_flex()
                    .id(suffix_id.clone())
                    .test_support()
                    .flex_shrink_0()
                    .gap_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .map(|this| match attention {
                        Attention::Approval(count) => {
                            this.child(Tag::warning().xsmall().child(if count > 1 {
                                format!("Approve {count}")
                            } else {
                                "Approve".to_owned()
                            }))
                        }
                        Attention::Busy => {
                            this.child(detail.clone()).child(Spinner::new().xsmall())
                        }
                        Attention::None => this.child(detail.clone()),
                    })
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_session(id.clone(), cwd.clone(), window, cx);
            }))
    });

    let menu = if empty {
        SidebarMenu::new().child(SidebarMenuItem::new("No saved threads").disable(true))
    } else {
        SidebarMenu::new().children(items)
    };

    Sidebar::new("threads-sidebar")
        .collapsible(false)
        .w(rems(18.))
        .header(
            SidebarHeader::new().child(
                Button::new("new-thread")
                    .ghost()
                    .w_full()
                    .icon(IconName::Plus)
                    .label("New thread")
                    .loading(creating > 0)
                    .tooltip("Ctrl+N")
                    .on_click(cx.listener(|this, _, window, cx| this.new_thread(window, cx))),
            ),
        )
        .child(SidebarGroup::new("Threads").child(menu))
        .when_some(error, |this, error| {
            this.footer(
                SidebarFooter::new().child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(format!("Couldn't load threads: {error}")),
                ),
            )
        })
}

/// The last component of `path`, or the whole path when it has none.
pub(crate) fn dir_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// "folder · 3h" for a saved session.
pub(crate) fn session_detail(session: &SessionSummary, now: SystemTime) -> String {
    let dir = dir_name(session.cwd());
    let age = session
        .updated_at()
        .and_then(unix_seconds)
        .zip(
            now.duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|now| i64::try_from(now.as_secs()).ok()),
        )
        .map(|(then, now)| relative_age(now - then));
    match age {
        Some(age) => format!("{dir} · {age}"),
        None => dir,
    }
}

/// Compact age: "now", "5m", "3h", "2d", "6w", "1y".
fn relative_age(seconds: i64) -> String {
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    const WEEK: i64 = 7 * DAY;
    const YEAR: i64 = 365 * DAY;
    match seconds.max(0) {
        s if s < MINUTE => "now".to_owned(),
        s if s < HOUR => format!("{}m", s / MINUTE),
        s if s < DAY => format!("{}h", s / HOUR),
        s if s < WEEK => format!("{}d", s / DAY),
        s if s < YEAR => format!("{}w", s / WEEK),
        s => format!("{}y", s / YEAR),
    }
}

/// Seconds since the Unix epoch for an RFC 3339 timestamp such as
/// `2026-11-04T12:34:56.789Z` or `2026-11-04T07:34:56-05:00`.
fn unix_seconds(stamp: &str) -> Option<i64> {
    let number = |range: std::ops::Range<usize>| stamp.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    // Skip fractional seconds, then read the zone.
    let zone = stamp
        .get(19..)?
        .trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    let offset = match zone.as_bytes().first()? {
        b'Z' | b'z' => 0,
        sign @ (b'+' | b'-') => {
            let hours = zone.get(1..3)?.parse::<i64>().ok()?;
            let minutes = zone.get(4..6)?.parse::<i64>().ok()?;
            let offset = hours * 3600 + minutes * 60;
            if *sign == b'+' {
                offset
            } else {
                -offset
            }
        }
        _ => return None,
    };
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rfc3339_with_zones_and_fractions() {
        assert_eq!(unix_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            unix_seconds("2026-11-04T12:00:00.123456Z"),
            Some(1_793_793_600)
        );
        assert_eq!(
            unix_seconds("2026-11-04T07:00:00-05:00"),
            Some(1_793_793_600)
        );
        assert_eq!(unix_seconds("yesterday"), None);
    }

    #[test]
    fn ages_are_compact() {
        assert_eq!(relative_age(5), "now");
        assert_eq!(relative_age(5 * 60), "5m");
        assert_eq!(relative_age(3 * 3600), "3h");
        assert_eq!(relative_age(15 * 86_400), "2w");
    }
}
