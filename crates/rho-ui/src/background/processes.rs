//! A session's background processes as its daemon reads them now
//! ([`Daemon::processes`]): how long each has run, how long it has been
//! quiet, how it ended, and the output of the one the user follows.
//!
//! The read is a snapshot, so [`Processes`] asks again while there is
//! something to learn, and stops when there isn't:
//!
//! - every [`LIST_POLL`] while the session's background list has a process
//!   running, and once more after that list changes, so a row that ended
//!   keeps how long it ran;
//! - every [`OUTPUT_POLL`] while the followed process's output is on screen
//!   and the process runs, and until its last output is read.
//!
//! A read that fails is not tried again until the list changes, a process
//! is followed, or the session's next background list after its machine
//! connected again, so a daemon that keeps failing isn't asked every
//! second.

use std::{collections::HashMap, time::Duration};

use gpui_kit::component::message_scroller::MessageScrollerState;
use gpui_kit::{AppContext as _, Context, Entity, SharedString, Task};
use rho_ui_core::{
    BackgroundKind, BackgroundStatus, BackgroundWork, Daemon, DaemonError, LogChange, ProcessLog,
    ProcessOutput, ProcessOutputParams, SessionId, SessionProcess, SessionProcessesResponse,
};

use super::is_live;
use crate::right_panel::Visibility;

/// How often the rows are read while a process runs: the daemon counts a
/// process's time in whole seconds, as the rows show it, so reading more
/// often shows nothing new, and less often makes the count jump.
const LIST_POLL: Duration = Duration::from_secs(1);

/// How often followed output is read while it is on screen: rho's own
/// terminal UI reads its process view every 500 ms (`RAIL_POLL` in rho's
/// `tui/daemon_jobs.rs`), so output reads as live here as there.
const OUTPUT_POLL: Duration = Duration::from_millis(500);

/// The process whose output the user follows, and the scroll state of the
/// list that shows its lines.
pub(crate) struct Followed {
    pub(crate) log: ProcessLog,
    pub(crate) scroller: Entity<MessageScrollerState>,
    /// Why no more output comes, once none will.
    done: Option<Done>,
}

/// Why a followed process's output is all read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Done {
    /// An output read found it ended: that read held its last output.
    Ended,
    /// The session no longer knows the process.
    Gone,
}

impl Followed {
    /// Why no more output comes, once none will; `None` while the process
    /// may still write.
    pub(crate) fn done(&self) -> Option<Done> {
        self.done
    }
}

/// One session's processes, as its daemon last read them. Its
/// [`crate::session_view::SessionView`] keeps it in step with the
/// session's background list ([`Self::sync`]).
pub(crate) struct Processes {
    daemon: Daemon,
    session: SessionId,
    /// Each process the daemon listed, by ID, as it last said. Kept after
    /// it leaves the list, so an ended row still says how long it ran; it
    /// holds no more than the processes the session ran.
    known: HashMap<String, SessionProcess>,
    /// The session's processes and their status, as its background list
    /// last had them.
    listed: Vec<(String, BackgroundStatus)>,
    /// How many times `listed` changed, and how many of those changes the
    /// newest answered read was sent after: a read is owed while they
    /// differ. An answer to a read sent before a change doesn't pay it.
    changes: u64,
    seen: u64,
    followed: Option<Followed>,
    /// Whether the followed output is on screen, as the right panel says.
    shown: Visibility,
    /// Why the last read failed; reads wait until something changes.
    error: Option<SharedString>,
    /// The loop reading on while anything is wanted. It ends itself when
    /// nothing is, setting `polling` back; the task stays until replaced.
    poll: Option<Task<()>>,
    polling: bool,
}

impl Processes {
    pub(crate) fn new(daemon: Daemon, session: SessionId) -> Self {
        Self {
            daemon,
            session,
            known: HashMap::new(),
            listed: Vec::new(),
            changes: 0,
            seen: 0,
            followed: None,
            shown: Visibility::Hidden,
            error: None,
            poll: None,
            polling: false,
        }
    }

    /// Reads through `daemon` from now on, as after the session's machine
    /// connected again. Reading waits for the session's next background
    /// list ([`Self::sync`]): until its attach answers, the daemon may not
    /// have the session loaded.
    pub(crate) fn set_daemon(&mut self, daemon: Daemon) {
        self.daemon = daemon;
        self.error = None;
        self.listed.clear();
        self.poll = None;
        self.polling = false;
    }

    /// Takes the session's background list, `work`: when its processes
    /// changed, they are read again. Reads start if anything is wanted.
    pub(crate) fn sync(&mut self, work: &[BackgroundWork], cx: &mut Context<'_, Self>) {
        let listed: Vec<(String, BackgroundStatus)> = work
            .iter()
            .filter_map(|work| match work.id().key() {
                Some((BackgroundKind::Process, id)) => Some((id.to_owned(), work.status())),
                _ => None,
            })
            .collect();
        if listed != self.listed {
            self.listed = listed;
            self.changes += 1;
            self.error = None;
        }
        self.kick(cx);
    }

    /// Process `id` as the daemon last read it, if it did.
    pub(crate) fn process(&self, id: &str) -> Option<&SessionProcess> {
        self.known.get(id)
    }

    /// The process whose output the user follows.
    pub(crate) fn followed(&self) -> Option<&Followed> {
        self.followed.as_ref()
    }

    /// Why the last read failed, if it did.
    pub(crate) fn error(&self) -> Option<&SharedString> {
        self.error.as_ref()
    }

    /// Follows process `id`'s output, from the start of what the session
    /// retains, in place of any other. Following the one followed again
    /// reads it afresh if its last read failed or the session forgot it.
    pub(crate) fn follow(&mut self, id: &str, cx: &mut Context<'_, Self>) {
        let reading = self
            .followed()
            .is_some_and(|followed| followed.log.id() == id && followed.done != Some(Done::Gone));
        if reading && self.error.is_none() {
            return;
        }
        self.followed = Some(Followed {
            log: ProcessLog::new(id.to_owned()),
            scroller: cx.new(|cx| MessageScrollerState::new(0, cx)),
            done: None,
        });
        self.error = None;
        self.restart(cx);
        cx.notify();
    }

    /// Stops following output.
    pub(crate) fn unfollow(&mut self, cx: &mut Context<'_, Self>) {
        if self.followed.take().is_some() {
            cx.notify();
        }
    }

    /// Notes whether the followed output is on screen: it is read only
    /// then, and at once when it comes back.
    pub(crate) fn set_shown(&mut self, shown: Visibility, cx: &mut Context<'_, Self>) {
        if self.shown != shown {
            self.shown = shown;
            if shown == Visibility::Shown {
                self.restart(cx);
            }
        }
    }

    /// The output to read with the next read: the followed process's,
    /// while it is on screen and has more to come.
    fn output_wanted(&self) -> Option<ProcessOutputParams> {
        self.followed
            .as_ref()
            .filter(|followed| self.shown == Visibility::Shown && followed.done.is_none())
            .map(|followed| followed.log.request())
    }

    /// Whether a read would tell anything new.
    fn wanted(&self) -> bool {
        let running = self
            .listed
            .iter()
            .any(|(_, status)| *status == BackgroundStatus::Running);
        let owed = self.seen < self.changes;
        self.error.is_none() && (owed || running || self.output_wanted().is_some())
    }

    /// Starts reading if anything is wanted and no read loop runs.
    fn kick(&mut self, cx: &mut Context<'_, Self>) {
        if self.polling || !self.wanted() {
            return;
        }
        self.polling = true;
        self.poll = Some(cx.spawn(async move |this, cx| loop {
            let read = this.update(cx, |this, _| this.next_read());
            let Ok(Some(read)) = read else {
                break;
            };
            let answer = read
                .daemon
                .processes(&read.session, read.output.clone())
                .await;
            let cadence = this.update(cx, |this, cx| this.answered(read, answer, cx));
            let Ok(cadence) = cadence else {
                break;
            };
            cx.background_executor().timer(cadence).await;
        }));
    }

    /// Reads at once, in place of a read loop that may be waiting. A read
    /// it had under way is dropped unanswered, so what it owed stays owed.
    fn restart(&mut self, cx: &mut Context<'_, Self>) {
        self.poll = None;
        self.polling = false;
        self.kick(cx);
    }

    /// What the next read asks, or `None`, ending the loop, when nothing is
    /// wanted.
    fn next_read(&mut self) -> Option<Read> {
        if !self.wanted() {
            self.polling = false;
            return None;
        }
        Some(Read {
            daemon: self.daemon.clone(),
            session: self.session.clone(),
            output: self.output_wanted(),
            changes: self.changes,
        })
    }

    /// Takes the answer to `read`, and returns how long to wait before the
    /// next. Redraws only if it changed something.
    fn answered(
        &mut self,
        read: Read,
        answer: Result<SessionProcessesResponse, DaemonError>,
        cx: &mut Context<'_, Self>,
    ) -> Duration {
        self.seen = self.seen.max(read.changes);
        let mut changed = false;
        match answer {
            Ok(answer) => {
                changed |= self.learn(answer.processes);
                // An answer for a process followed before this one changes
                // nothing.
                let followed = self.followed.as_mut().filter(|followed| {
                    read.output
                        .is_some_and(|asked| asked.process == followed.log.id())
                });
                if let Some(followed) = followed {
                    if let Some(output) = answer.output {
                        let process = output.process.clone();
                        changed |= show(followed, output, cx);
                        changed |= self.learn([process]);
                    } else {
                        followed.done = Some(Done::Gone);
                        changed = true;
                    }
                }
            }
            Err(error) => {
                self.error = Some(error.to_string().into());
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
        if self.output_wanted().is_some() {
            OUTPUT_POLL
        } else {
            LIST_POLL
        }
    }

    /// Keeps what the daemon said of `processes`; returns whether it said
    /// anything new.
    fn learn(&mut self, processes: impl IntoIterator<Item = SessionProcess>) -> bool {
        let mut changed = false;
        for process in processes {
            changed |= self.known.get(&process.id) != Some(&process);
            self.known.insert(process.id.clone(), process);
        }
        changed
    }
}

/// One read as it was sent.
struct Read {
    daemon: Daemon,
    session: SessionId,
    /// The output it asked for too, if any.
    output: Option<ProcessOutputParams>,
    /// How many times the list had changed when it was sent.
    changes: u64,
}

/// Adds `output` to what `followed` holds, and has its list show the lines
/// that changed. Returns whether anything did.
fn show(followed: &mut Followed, output: ProcessOutput, cx: &mut Context<'_, Processes>) -> bool {
    let was = followed.done;
    if !is_live(output.process.state) {
        // The daemon marks a process ended only once its output is all
        // read, so this answer holds the last of it.
        followed.done = Some(Done::Ended);
    }
    let change = followed.log.apply(output);
    if change == LogChange::default() {
        return followed.done != was;
    }
    followed.scroller.update(cx, |scroller, cx| {
        if change.dropped > 0 {
            scroller.splice(0..change.dropped, 0, cx);
        }
        if change.trimmed {
            scroller.remeasure_items(0..1, cx);
        }
        if let Some(ix) = change.changed {
            scroller.remeasure_items(ix..ix + 1, cx);
        }
        if change.added > 0 {
            scroller.append(change.added, cx);
        }
    });
    true
}
