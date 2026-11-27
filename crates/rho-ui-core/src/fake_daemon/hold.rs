//! Answers a test holds, to control when they go out: [`FakeDaemon::hold`]
//! makes the requests of one kind ([`Answers`]) wait,
//! [`FakeDaemon::release`] sends every answer waiting and lets later ones
//! go at once, and [`FakeDaemon::waiting`] counts the answers waiting.
//!
//! A release frees every answer waiting when it comes, even one a hold
//! right after catches before its call runs again. A restart or stop of
//! the daemon lifts every hold, as a daemon started afresh holds nothing,
//! and ends the answers waiting with `DaemonStopping`, as rho's daemon
//! ends what waits when it stops, so their connections can close.

use rho_protocol::{ErrorCode, RpcError};
use tokio::sync::watch;

use super::FakeDaemon;

/// The answers a test can hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answers {
    /// `rho/session/mcp/catalog`, listed once released, as a slow
    /// server's listing is.
    McpCatalog,
    /// `rho/session/mcp/resource`, read once released, as a slow server's
    /// reads are.
    McpReads,
    /// `rho/session/report`, read as it is asked but answered once
    /// released, as a busy daemon's answer comes late: it says what was so
    /// when it was asked.
    Reports,
}

/// One [`Hold`] for each kind of [`Answers`]. It lives in the daemon's
/// state, so each change and each [`Self::arrive`] happens under its lock,
/// before or after any other as a whole.
#[derive(Debug, Default)]
pub(super) struct Holds([Hold; 3]);

impl Holds {
    fn index(answers: Answers) -> usize {
        match answers {
            Answers::McpCatalog => 0,
            Answers::McpReads => 1,
            Answers::Reports => 2,
        }
    }

    fn of(&mut self, answers: Answers) -> &mut Hold {
        &mut self.0[Self::index(answers)]
    }

    fn waiting(&self, answers: Answers) -> usize {
        self.0[Self::index(answers)].wakes.receiver_count()
    }

    /// What an `answers` that is ready now waits for.
    pub(super) fn arrive(&self, answers: Answers) -> Wait {
        let hold = &self.0[Self::index(answers)];
        Wait(hold.held.then(|| hold.wakes.subscribe()))
    }

    /// Lifts every hold and ends what waits, as the daemon restarts or
    /// stops.
    pub(super) fn end(&mut self) {
        for hold in &mut self.0 {
            hold.held = false;
            hold.wakes.send_replace(Wake::Stopped);
        }
    }
}

/// Whether answers of one kind wait, and what woke those that did.
#[derive(Debug, Default)]
struct Hold {
    held: bool,
    /// Only an answer waiting subscribes, so its receivers count those
    /// waiting. Each waits for the next send, not for `held` to clear, so
    /// no later hold can catch it again.
    wakes: watch::Sender<Wake>,
}

/// Why the answers waiting were woken.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Wake {
    #[default]
    Released,
    /// The daemon restarted or stopped.
    Stopped,
}

impl Hold {
    fn hold(&mut self) {
        self.held = true;
    }

    fn release(&mut self) {
        self.held = false;
        self.wakes.send_replace(Wake::Released);
    }
}

/// What a held answer waits for: nothing unless it was held.
#[must_use = "an answer held goes out only once its wait ends"]
pub(super) struct Wait(Option<watch::Receiver<Wake>>);

impl Wait {
    /// Ends at the next release, or with `DaemonStopping` if the daemon
    /// restarts or stops first.
    pub(super) async fn until_released(self) -> Result<(), RpcError> {
        let Some(mut wakes) = self.0 else {
            return Ok(());
        };
        // A subscriber has seen the value it subscribed at, so this waits
        // for the next send. The sender outlives every call, so it doesn't
        // fail.
        let _ = wakes.changed().await;
        let wake = *wakes.borrow();
        match wake {
            Wake::Released => Ok(()),
            Wake::Stopped => Err(RpcError::new(
                ErrorCode::DaemonStopping,
                "the daemon stopped before it answered",
            )),
        }
    }
}

impl FakeDaemon {
    /// Makes `answers` wait until [`Self::release`], or until the daemon
    /// restarts or stops.
    pub fn hold(&self, answers: Answers) {
        self.service.lock().answer_holds.of(answers).hold();
    }

    /// Sends the `answers` [`Self::hold`] held, and those after at once.
    pub fn release(&self, answers: Answers) {
        self.service.lock().answer_holds.of(answers).release();
    }

    /// How many `answers` wait for a release, or were released and have
    /// yet to go out.
    pub fn waiting(&self, answers: Answers) -> usize {
        self.service.lock().answer_holds.waiting(answers)
    }
}

#[cfg(test)]
mod tests {
    use futures::FutureExt as _;

    use super::*;

    /// A hold right after a release doesn't catch the answers that
    /// release freed, however late they run again.
    #[test]
    fn a_release_frees_what_waits_through_a_hold_after_it() {
        let mut holds = Holds::default();
        holds.of(Answers::Reports).hold();
        let wait = holds.arrive(Answers::Reports);
        assert_eq!(holds.waiting(Answers::Reports), 1);
        holds.of(Answers::Reports).release();
        holds.of(Answers::Reports).hold();
        assert_eq!(wait.until_released().now_or_never(), Some(Ok(())));
        assert_eq!(holds.waiting(Answers::Reports), 0);
    }

    /// A restart or stop ends the answers waiting, and those that arrive
    /// after it go at once, rather than leaving their connection open
    /// until a release.
    #[test]
    fn a_restart_ends_what_waits_and_what_comes_after() {
        let mut holds = Holds::default();
        holds.of(Answers::Reports).hold();
        let waiting = holds.arrive(Answers::Reports);
        holds.end();
        let ended = waiting.until_released().now_or_never();
        let code = ended
            .and_then(Result::err)
            .and_then(|error| error.error_code());
        assert_eq!(code, Some(ErrorCode::DaemonStopping));
        let late = holds.arrive(Answers::Reports).until_released();
        assert_eq!(late.now_or_never(), Some(Ok(())));
    }
}
