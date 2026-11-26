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
//! A read that fails is not tried again until the list changes, another
//! process is followed, or the session's machine connects again: a daemon
//! from before process reads refuses every one.

use std::{collections::HashMap, time::Duration};

use crate::right_panel::Visibility;
use gpui_kit::component::message_scroller::MessageScrollerState;
use gpui_kit::{AppContext as _, Context, Entity, SharedString, Task};
use rho_ui_core::{
    BackgroundKind, BackgroundStatus, BackgroundWork, Daemon, DaemonError, ProcessLog,
    ProcessOutput, ProcessOutputParams, ProcessState, SessionId, SessionProcess,
    SessionProcessesResponse,
};

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
    /// The session no longer knows the process: no more output comes.
    pub(crate) gone: bool,
}

/// One session's processes, as its daemon last read them. Its
/// [`crate::session_view::SessionView`] keeps it in step with the
/// session's background list ([`Self::sync`]).
pub(crate) struct Processes {
    daemon: Daemon,
    session: SessionId,
    /// Each process the daemon listed, by ID, as it last said: kept after
    /// it leaves the list, so an ended row still says how long it ran.
    known: HashMap<String, SessionProcess>,
    /// The session's processes and their status, as its background list
    /// last had them.
    listed: Vec<(String, BackgroundStatus)>,
    /// The list changed since the last answer: one more read is owed.
    owed: bool,
    followed: Option<Followed>,
    /// Whether the followed output is on screen, as the right panel says.
    shown: Visibility,
    /// Why the last read failed; reads wait until something changes.
    error: Option<SharedString>,
    /// The loop reading on while anything is wanted, which ends itself
    /// when nothing is.
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
            owed: false,
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
            self.owed = true;
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
    /// retains, in place of any other.
    pub(crate) fn follow(&mut self, id: &str, cx: &mut Context<'_, Self>) {
        if self
            .followed()
            .is_some_and(|followed| followed.log.id() == id)
        {
            return;
        }
        self.followed = Some(Followed {
            log: ProcessLog::new(id.to_owned()),
            scroller: cx.new(|cx| MessageScrollerState::new(0, cx)),
            gone: false,
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
        let followed = self
            .followed
            .as_ref()
            .filter(|followed| self.shown == Visibility::Shown && !followed.gone)?;
        let ended = followed
            .log
            .process()
            .is_some_and(|process| !is_live(process.state));
        // Once it ended, one read takes what it wrote last; after that
        // nothing more comes.
        (!ended || self.owed).then(|| followed.log.request())
    }

    /// Whether a read would tell anything new.
    fn wanted(&self) -> bool {
        let running = self
            .listed
            .iter()
            .any(|(_, status)| *status == BackgroundStatus::Running);
        self.error.is_none() && (self.owed || running || self.output_wanted().is_some())
    }

    /// Starts reading if anything is wanted and no read loop runs.
    fn kick(&mut self, cx: &mut Context<'_, Self>) {
        if self.polling || !self.wanted() {
            return;
        }
        self.polling = true;
        self.poll = Some(cx.spawn(async move |this, cx| loop {
            let read = this.update(cx, |this, _| this.next_read());
            let Ok(Some((daemon, session, output))) = read else {
                break;
            };
            let answer = daemon.processes(&session, output.clone()).await;
            let cadence = this.update(cx, |this, cx| this.answered(output, answer, cx));
            let Ok(cadence) = cadence else {
                break;
            };
            cx.background_executor().timer(cadence).await;
        }));
    }

    /// Reads at once, in place of a read loop that may be waiting. A read
    /// the loop had under way may have been the one owed, which is owed
    /// again.
    fn restart(&mut self, cx: &mut Context<'_, Self>) {
        self.owed |= self.polling;
        self.poll = None;
        self.polling = false;
        self.kick(cx);
    }

    /// What the next read asks, or `None`, ending the loop, when nothing is
    /// wanted. The read is the one owed, if one is: an answer to a read
    /// sent before the list changed would say what was before.
    fn next_read(&mut self) -> Option<(Daemon, SessionId, Option<ProcessOutputParams>)> {
        if !self.wanted() {
            self.polling = false;
            return None;
        }
        let output = self.output_wanted();
        self.owed = false;
        Some((self.daemon.clone(), self.session.clone(), output))
    }

    /// Takes the answer to a read that asked for `asked`, and returns how
    /// long to wait before the next.
    fn answered(
        &mut self,
        asked: Option<ProcessOutputParams>,
        answer: Result<SessionProcessesResponse, DaemonError>,
        cx: &mut Context<'_, Self>,
    ) -> Duration {
        let mut changed = false;
        match answer {
            Ok(answer) => {
                for process in answer.processes {
                    changed |= self.known.get(&process.id) != Some(&process);
                    self.known.insert(process.id.clone(), process);
                }
                // An answer for a process followed before this one changes
                // nothing.
                let followed = self.followed.as_mut().filter(|followed| {
                    asked.is_some_and(|asked| asked.process == followed.log.id())
                });
                if let Some(followed) = followed {
                    changed = true;
                    match answer.output {
                        Some(output) => {
                            self.known
                                .insert(output.process.id.clone(), output.process.clone());
                            show(followed, output, cx);
                        }
                        None => followed.gone = true,
                    }
                }
            }
            Err(error) => {
                changed = true;
                self.error = Some(error.to_string().into());
            }
        }
        // Each redraw reaches the session's view and the panel: only
        // what changed is drawn again.
        if changed {
            cx.notify();
        }
        if self.output_wanted().is_some() {
            OUTPUT_POLL
        } else {
            LIST_POLL
        }
    }
}

/// Adds `output` to what `followed` holds, and has its list show the lines
/// that changed.
fn show(followed: &mut Followed, output: ProcessOutput, cx: &mut Context<'_, Processes>) {
    let change = followed.log.apply(output);
    followed.scroller.update(cx, |scroller, cx| {
        if change.dropped > 0 {
            scroller.splice(0..change.dropped, 0, cx);
        }
        if let Some(ix) = change.changed {
            scroller.remeasure_items(ix..ix + 1, cx);
        }
        if change.added > 0 {
            scroller.append(change.added, cx);
        }
    });
}

/// Whether a process in `state` may still write.
pub(crate) fn is_live(state: ProcessState) -> bool {
    matches!(state, ProcessState::Starting | ProcessState::Running)
}
