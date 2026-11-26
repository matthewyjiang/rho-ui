//! What tells the user that a session wants them while rho-ui is in the
//! background: a desktop notification when a session starts to need them or
//! its turn ends (its sidebar row turns Done), and the count of the sessions
//! that do in the window's title, `(2) Rho`, and on macOS on the Dock icon.
//!
//! [`Watch`] tells which sessions newly want the user from the sidebar's
//! list as it changes, so a session on a remote machine counts as one here
//! does: its daemon's connection keeps its row current. The app
//! (`app/attention.rs`) gathers what a few seconds bring into one
//! notification ([`notification`]) and posts it through a [`Notifier`].
//!
//! What GPUI offers, as of the version in `Cargo.lock`:
//!
//! - `App::show_system_notification`, and `on_system_notification_response`
//!   for a click on one. On Linux, X11 and Wayland alike, it posts over the
//!   XDG notifications D-Bus interface with `notify-rust`, and a click on
//!   its body answers. On macOS it uses `UNUserNotificationCenter`, which
//!   only works in the app bundle: run as a bare binary, nothing shows.
//! - `Window::request_attention`: the urgency hint on X11, which the window
//!   manager clears on focus, and one bounce of the Dock icon on macOS.
//!   On Wayland it does nothing.
//! - No badge. The Dock's is set here through `AppKit` ([`set_dock_badge`]),
//!   which GPUI's Mac platform builds already; Linux has no common one.

use std::collections::{HashMap, HashSet};
#[cfg(test)]
use std::{cell::RefCell, rc::Rc};

use gpui_kit::{App, SharedString, SystemNotification};

use crate::{
    machines::{MachineId, SessionKey},
    session_list::Status,
};

/// What a session wants of the user, as its sidebar row says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Call {
    /// It asks a question or waits for an approval.
    NeedsYou,
    /// Its turn ended since the user last saw it.
    Done,
}

impl Call {
    /// What a row whose status is `status` wants, if anything.
    pub(crate) fn of(status: Option<Status>) -> Option<Self> {
        match status {
            Some(Status::NeedsYou(_)) => Some(Call::NeedsYou),
            Some(Status::Done) => Some(Call::Done),
            Some(Status::Working(_) | Status::Background | Status::Watching) | None => None,
        }
    }

    /// The word the sidebar's row says it with.
    fn word(self) -> &'static str {
        match self {
            Call::NeedsYou => "Needs you",
            Call::Done => "Done",
        }
    }
}

/// What each listed session wanted at the last look, to tell which start
/// to want the user.
///
/// A machine's sessions only call once it is primed ([`Self::prime`]):
/// what its daemon says when rho-ui connects, such as a question that
/// waited before, is where it starts, not news.
#[derive(Debug, Default)]
pub(crate) struct Watch {
    /// Every listed session, with what it wants.
    last: HashMap<SessionKey, Option<Call>>,
    primed: HashSet<MachineId>,
}

impl Watch {
    /// Takes what every listed session wants now; returns the ones that
    /// started to. A session that needs the user calls once until it stops;
    /// one Done calls only when it was listed before as something else, as
    /// a session Done before rho-ui started lists Done from the first.
    pub(crate) fn update<'a>(
        &mut self,
        now: impl IntoIterator<Item = (&'a SessionKey, Option<Call>)>,
    ) -> Vec<(SessionKey, Call)> {
        let mut calls = Vec::new();
        let mut next = HashMap::with_capacity(self.last.len());
        for (id, call) in now {
            let before = self.last.get(id).copied();
            let started = call.filter(|call| match call {
                Call::NeedsYou => before != Some(Some(Call::NeedsYou)),
                Call::Done => before.is_some_and(|before| before != Some(Call::Done)),
            });
            if let Some(call) = started.filter(|_| self.primed.contains(&id.machine)) {
                calls.push((id.clone(), call));
            }
            next.insert(id.clone(), call);
        }
        self.last = next;
        calls
    }

    /// Whether `machine`'s sessions call.
    pub(crate) fn is_primed(&self, machine: &MachineId) -> bool {
        self.primed.contains(machine)
    }

    /// Takes what `machine`'s sessions want now, `now`, as where they
    /// start, and from here on lets them call: once its daemon's first jobs
    /// listing on a connection is in the list.
    pub(crate) fn prime<'a>(
        &mut self,
        machine: &MachineId,
        now: impl IntoIterator<Item = (&'a SessionKey, Option<Call>)>,
    ) {
        for (id, call) in now {
            if id.machine == *machine {
                self.last.insert(id.clone(), call);
            }
        }
        self.primed.insert(machine.clone());
    }

    /// Stops `machine`'s sessions calling until it is primed again: its
    /// connection is new, or gone.
    pub(crate) fn unprime(&mut self, machine: &MachineId) {
        self.primed.remove(machine);
    }
}

/// A session in a notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Called {
    /// Its sidebar row's element ID, which the notification's tag carries
    /// back when it is clicked.
    pub(crate) row: SharedString,
    pub(crate) title: SharedString,
    pub(crate) call: Call,
}

/// One notification for `called`, the sessions that started to want the
/// user within a few seconds; `None` for none. It names the session when
/// there is one, else counts them and lists their titles. A click opens
/// the first that needs the user, else the first Done.
pub(crate) fn notification(mut called: Vec<Called>) -> Option<SystemNotification> {
    called.sort_by_key(|called| called.call == Call::Done);
    let first = called.first()?;
    let (title, body) = match called.as_slice() {
        [one] => (one.title.clone(), one.call.word().into()),
        many => {
            let needs = many.iter().filter(|c| c.call == Call::NeedsYou).count();
            let done = many.len() - needs;
            // Two or more: one alone is named above.
            let title = match (needs, done) {
                (needs, 0) => format!("{needs} sessions need you"),
                (0, done) => format!("{done} sessions are done"),
                (1, done) => format!("1 session needs you, {done} done"),
                (needs, done) => format!("{needs} sessions need you, {done} done"),
            };
            let titles: Vec<&str> = many.iter().map(|called| called.title.as_ref()).collect();
            (title.into(), titles.join(", ").into())
        }
    };
    Some(SystemNotification {
        tag: first.row.clone(),
        title,
        body,
        actions: Vec::new(),
    })
}

/// The window's title while `count` sessions want the user.
pub(crate) fn window_title(count: usize) -> String {
    let name = crate::window_chrome::TITLE;
    if count == 0 {
        name.to_owned()
    } else {
        format!("({count}) {name}")
    }
}

/// Shows `count` on the Dock icon, or no badge for 0. Off the main thread,
/// as in the UI tests, it does nothing: `AppKit` is the main thread's.
#[cfg(target_os = "macos")]
pub(crate) fn set_dock_badge(count: usize) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    use objc2_foundation::NSString;

    let Some(main_thread) = MainThreadMarker::new() else {
        return;
    };
    let label = (count > 0).then(|| NSString::from_str(&count.to_string()));
    NSApplication::sharedApplication(main_thread)
        .dockTile()
        .setBadgeLabel(label.as_deref());
}

/// Only macOS has a badge GPUI's windows can reach.
#[cfg(not(target_os = "macos"))]
pub(crate) fn set_dock_badge(_count: usize) {}

/// Where notifications go: the desktop's, except in UI tests, which keep
/// them to check.
#[derive(Clone, Debug, Default)]
pub(crate) enum Notifier {
    #[default]
    System,
    #[cfg(test)]
    Kept(Rc<RefCell<Vec<SystemNotification>>>),
}

impl Notifier {
    pub(crate) fn post(&self, notification: SystemNotification, cx: &App) {
        match self {
            Notifier::System => cx.show_system_notification(notification),
            #[cfg(test)]
            Notifier::Kept(kept) => kept.borrow_mut().push(notification),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(machine: &str, id: &str) -> SessionKey {
        SessionKey::new(MachineId::from(machine), id.into())
    }

    /// One list of the sidebar's: each session's machine, ID and call.
    type Step<'a> = &'a [(&'a str, &'a str, Option<Call>)];

    /// A case: its name, its lists, the list from which `box` is primed,
    /// and the calls each list makes.
    type Case<'a> = (&'a str, &'a [Step<'a>], usize, &'a [&'a [&'a str]]);

    /// Feeds each step's list to one watch, which `local` is primed for
    /// from the start and `box` from step `prime_box`, and returns what
    /// each step called.
    fn calls(steps: &[Step<'_>], prime_box: usize) -> Vec<Vec<String>> {
        let mut watch = Watch::default();
        watch.prime(&MachineId::from("local"), []);
        let mut out = Vec::new();
        for (ix, step) in steps.iter().enumerate() {
            let keys: Vec<(SessionKey, Option<Call>)> = step
                .iter()
                .map(|&(machine, id, call)| (key(machine, id), call))
                .collect();
            let listed = || keys.iter().map(|(id, call)| (id, *call));
            if ix == prime_box {
                watch.prime(&MachineId::from("box"), listed());
            }
            let called = watch.update(listed());
            out.push(
                called
                    .into_iter()
                    .map(|(id, call)| format!("{} {call:?}", id.id))
                    .collect(),
            );
        }
        out
    }

    /// Which list changes call: a session that starts to need the user,
    /// once; one whose turn ends while listed; never a session Done when
    /// it is first listed, nor anything a machine says before it is
    /// primed, as a question that waited across a reconnect.
    #[test]
    fn a_session_calls_when_it_starts_to_want_the_user() {
        use Call::{Done, NeedsYou};
        let cases: &[Case<'_>] = &[
            (
                "asks, then is answered and ends",
                &[
                    &[("local", "a", None)],
                    &[("local", "a", Some(NeedsYou))],
                    &[("local", "a", Some(NeedsYou))],
                    &[("local", "a", None)],
                    &[("local", "a", Some(Done))],
                    &[("local", "a", Some(Done))],
                ],
                0,
                &[&[], &["a NeedsYou"], &[], &[], &["a Done"], &[]],
            ),
            (
                "listed already asking, and already Done",
                &[&[("local", "a", Some(NeedsYou)), ("local", "b", Some(Done))]],
                0,
                &[&["a NeedsYou"]],
            ),
            (
                "a machine says nothing until primed",
                &[
                    &[("box", "a", None)],
                    &[("box", "a", Some(NeedsYou))],
                    &[("box", "a", None)],
                    &[("box", "a", Some(NeedsYou))],
                    &[("box", "a", Some(NeedsYou)), ("box", "b", Some(NeedsYou))],
                ],
                3,
                &[&[], &[], &[], &[], &["b NeedsYou"]],
            ),
        ];
        for &(case, steps, prime_box, want) in cases {
            let want: Vec<Vec<String>> = want
                .iter()
                .map(|step| step.iter().map(|&call| call.to_owned()).collect())
                .collect();
            assert_eq!(calls(steps, prime_box), want, "{case}");
        }
    }

    /// A burst is one notification: it opens the first session that needs
    /// the user, whatever came first.
    #[test]
    fn a_burst_opens_the_first_that_needs_the_user() {
        let called = |row: &str, call| Called {
            row: row.to_owned().into(),
            title: row.to_owned().into(),
            call,
        };
        let one = notification(vec![called("session-local-a", Call::Done)]).expect("one");
        assert_eq!(one.tag, "session-local-a");
        let burst = notification(vec![
            called("session-local-a", Call::Done),
            called("session-box-b", Call::NeedsYou),
            called("session-local-c", Call::NeedsYou),
        ])
        .expect("one for the burst");
        assert_eq!(burst.tag, "session-box-b");
        assert_eq!(notification(Vec::new()), None);
    }
}
