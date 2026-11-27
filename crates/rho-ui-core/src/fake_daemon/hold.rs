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

use super::FakeDaemon;
use crate::hold::{Hold, Wait, Wake};

/// The answers a test can hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answers {
    /// `rho/session/mcp/catalog`, listed once released, as a slow
    /// server's listing is.
    McpCatalog,
    /// `rho/session/mcp/resource`, read once released, as a slow server's
    /// reads are.
    McpReads,
    /// `rho/plugins/install`, installed once released, as a slow git
    /// fetch is.
    PluginInstalls,
    /// `rho/session/report`, read as it is asked but answered once
    /// released, as a busy daemon's answer comes late: it says what was so
    /// when it was asked.
    Reports,
}

/// One [`Hold`] for each kind of [`Answers`].
#[derive(Debug, Default)]
pub(super) struct Holds([Hold; 4]);

impl Holds {
    pub(super) fn of(&self, answers: Answers) -> &Hold {
        let index = match answers {
            Answers::McpCatalog => 0,
            Answers::McpReads => 1,
            Answers::Reports => 2,
            Answers::PluginInstalls => 3,
        };
        &self.0[index]
    }

    /// Lifts every hold and ends what waits, as the daemon restarts or
    /// stops.
    pub(super) fn end(&self) {
        self.0.iter().for_each(Hold::end);
    }
}

/// Ends once the answer may go out, or with `DaemonStopping` if the daemon
/// restarts or stops first.
pub(super) async fn released(wait: Wait) -> Result<(), RpcError> {
    match wait.woken().await {
        Wake::Released => Ok(()),
        Wake::Ended => Err(RpcError::new(
            ErrorCode::DaemonStopping,
            "the daemon stopped before it answered",
        )),
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
        self.service.lock().answer_holds.of(answers).waiting()
    }
}

#[cfg(test)]
mod tests {
    use futures::FutureExt as _;

    use super::*;

    /// A restart or stop ends the answers waiting, and those that arrive
    /// after it go at once, rather than leaving their connection open
    /// until a release.
    #[test]
    fn a_restart_ends_what_waits_and_what_comes_after() {
        let holds = Holds::default();
        holds.of(Answers::Reports).hold();
        let waiting = holds.of(Answers::Reports).arrive();
        holds.end();
        let ended = released(waiting).now_or_never();
        let code = ended
            .and_then(Result::err)
            .and_then(|error| error.error_code());
        assert_eq!(code, Some(ErrorCode::DaemonStopping));
        let late = released(holds.of(Answers::Reports).arrive());
        assert_eq!(late.now_or_never(), Some(Ok(())));
    }
}
