//! Telling the user, from the app's side, of the sessions that want them
//! (see [`crate::attention`]): the count in the window's title and on the
//! Dock, kept as the sidebar's list changes, and a desktop notification for
//! the sessions that start to want the user while the window doesn't have
//! the focus, one for what [`COALESCE`] brings. A click on it brings the
//! window forward, where the platform lets an app do that, and opens the
//! session.

use std::{mem, time::Duration};

use gpui_kit::{App, Context, Task, Window};

use super::{RhoApp, JOB_POLL};
use crate::{
    attention::{self, Call, Called, Notifier, Watch},
    machines::{MachineId, SessionKey},
    pull_requests::Focus,
    settings::Settings,
    shelf::Shelf,
};

/// How long the first session to want the user waits for others, to go in
/// one notification with them. A session that isn't open here changes on
/// the daemon's jobs, polled every [`JOB_POLL`], so sessions that settle
/// together, as a lead's workers or a turn and the session it started,
/// land at most one poll apart: two polls catch them, and a notification
/// two seconds late costs nothing to someone away from the window.
const COALESCE: Duration = JOB_POLL.saturating_mul(2);

/// What [`RhoApp`] keeps to tell the user of sessions that want them.
pub(super) struct Attention {
    watch: Watch,
    /// As the window's last change said; it starts with it.
    focus: Focus,
    /// The sessions that started to want the user since the first of them,
    /// waiting out [`COALESCE`].
    batch: Vec<(SessionKey, Call)>,
    /// Posts `batch` once [`COALESCE`] has passed; `None` while it is
    /// empty.
    post: Option<Task<()>>,
    /// The sessions the title counts.
    count: usize,
    notifier: Notifier,
}

impl Attention {
    pub(super) fn new(notifier: Notifier) -> Self {
        Self {
            watch: Watch::default(),
            focus: Focus::Focused,
            batch: Vec::new(),
            post: None,
            count: 0,
            notifier,
        }
    }
}

impl RhoApp {
    /// Answers a click on one of rho-ui's notifications, from now on, in
    /// this window.
    pub(super) fn answer_notifications(window: &Window, cx: &mut Context<'_, Self>) {
        let (app, handle) = (cx.entity().downgrade(), window.window_handle());
        cx.on_system_notification_response(move |response, cx: &mut App| {
            let _ = handle.update(cx, |_, window, cx| {
                let _ = app.update(cx, |app, cx| {
                    app.notification_clicked(&response.tag, window, cx);
                });
            });
        });
    }

    /// The sidebar's list changed: counts the sessions that want the user
    /// in the title and on the Dock, and queues a notification for each
    /// that started to, unless the window has the focus.
    pub(super) fn attention_changed(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let rows = self.sidebar.read(cx).rows();
        let count = rows
            .iter()
            .filter(|row| {
                row.shelf == Shelf::Active && !row.away() && Call::of(row.status()).is_some()
            })
            .count();
        let mut calls = self
            .attention
            .watch
            .update(rows.iter().map(|row| (&row.id, Call::of(row.status()))));
        if count != self.attention.count {
            self.attention.count = count;
            window.set_window_title(&attention::window_title(count));
            attention::set_dock_badge(count);
        }
        if self.attention.focus == Focus::Focused {
            return;
        }
        let sends = cx.global::<Settings>().notifications;
        calls.retain(|&(_, call)| sends.sends(call));
        if calls.is_empty() {
            return;
        }
        self.attention.batch.extend(calls);
        if self.attention.post.is_none() {
            self.attention.post = Some(cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(COALESCE).await;
                let _ = this.update_in(cx, Self::post_attention);
            }));
        }
    }

    /// Posts the batch as one notification, of the sessions that still want
    /// what they started to and the setting still sends: a question
    /// answered elsewhere meanwhile is left out.
    fn post_attention(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.attention.post = None;
        let batch = mem::take(&mut self.attention.batch);
        // The setting may have changed meanwhile.
        let sends = cx.global::<Settings>().notifications;
        let sidebar = self.sidebar.read(cx);
        let mut called: Vec<Called> = Vec::new();
        for (id, call) in batch {
            let Some(row) = sidebar.rows().iter().find(|row| row.id == id) else {
                continue;
            };
            let listed = called.iter().any(|called| called.row == row.element_id);
            if Call::of(row.status()) == Some(call) && sends.sends(call) && !listed {
                called.push(Called {
                    row: row.element_id.clone(),
                    title: row.title(),
                    call,
                });
            }
        }
        if let Some(notification) = attention::notification(called) {
            self.attention.notifier.post(notification, cx);
            window.request_attention();
        }
    }

    /// The user clicked notification `tag`: brings the window forward and
    /// opens the session whose row it names, if it is still listed.
    pub(crate) fn notification_clicked(
        &mut self,
        tag: &str,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        window.activate_window();
        let id = self
            .sidebar
            .read(cx)
            .rows()
            .iter()
            .find(|row| row.element_id == tag)
            .map(|row| row.id.clone());
        if let Some(id) = id {
            self.sidebar.update(cx, |sidebar, cx| sidebar.open(&id, cx));
        }
    }

    /// The window gained the focus or lost it. Back in it, the user sees
    /// what a notification would say, so a waiting one is dropped.
    pub(super) fn focus_changed(&mut self, focus: Focus, cx: &mut Context<'_, Self>) {
        self.attention.focus = focus;
        if focus == Focus::Focused {
            self.attention.batch.clear();
            self.attention.post = None;
        }
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_focus(focus, cx));
    }

    /// Lets `machine`'s sessions call once its first jobs listing on a
    /// connection is in the sidebar, which then shows what it ran and
    /// waited on before rho-ui connected: that is no news.
    pub(super) fn prime_attention(&mut self, machine: &MachineId, cx: &App) {
        if self.attention.watch.is_primed(machine) {
            return;
        }
        let rows = self.sidebar.read(cx).rows();
        self.attention.watch.prime(
            machine,
            rows.iter().map(|row| (&row.id, Call::of(row.status()))),
        );
    }

    /// How many sessions the window's title counts, for UI tests: headless
    /// GPUI keeps no title to read back.
    #[cfg(test)]
    pub(crate) fn attention_count(&self) -> usize {
        self.attention.count
    }

    /// Stops `machine`'s sessions calling until its next connection lists
    /// its jobs: what its rows do meanwhile is the connection coming and
    /// going, not the sessions.
    pub(super) fn unprime_attention(&mut self, machine: &MachineId) {
        self.attention.watch.unprime(machine);
    }
}
