//! A hold on a fake's answers, so a test controls when they go out: the
//! fake daemon's ([`crate::fake_daemon::Answers`]) and the fake sshd's
//! session channels ([`crate::fake_sshd::FakeSshd::hold_sessions`]).
//!
//! An answer ready while held waits for the next wake: a release, or the
//! end of the server the fake plays. It waits for that wake itself, not
//! for the hold to clear, so a hold right after a release doesn't catch
//! the answers that release freed, however late they run again.

use tokio::sync::watch;

/// Whether answers wait, and the wakes they wait for. A clone shares its
/// state: it holds and releases the same answers.
#[derive(Clone, Debug, Default)]
pub(crate) struct Hold(watch::Sender<Holding>);

#[derive(Clone, Copy, Debug, Default)]
struct Holding {
    held: bool,
    /// Counts wakes; an answer waiting waits for it to change.
    wakes: u64,
    /// The latest wake.
    wake: Wake,
}

/// What woke the answers waiting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Wake {
    /// [`Hold::release`], or nothing held them.
    #[default]
    Released,
    /// [`Hold::end`]: the server the fake plays stopped.
    Ended,
}

impl Hold {
    /// Makes answers ready from now on wait.
    pub(crate) fn hold(&self) {
        self.0.send_modify(|holding| holding.held = true);
    }

    /// Sends the answers waiting, and those after at once.
    pub(crate) fn release(&self) {
        self.wake(Wake::Released);
    }

    /// Ends the answers waiting, as the server the fake plays stops, and
    /// sends those after at once.
    pub(crate) fn end(&self) {
        self.wake(Wake::Ended);
    }

    fn wake(&self, wake: Wake) {
        self.0.send_modify(|holding| {
            holding.held = false;
            holding.wakes += 1;
            holding.wake = wake;
        });
    }

    /// How many answers wait for a wake, or were woken and have yet to go
    /// out: only an answer held subscribes.
    pub(crate) fn waiting(&self) -> usize {
        self.0.receiver_count()
    }

    /// What an answer ready now waits for.
    pub(crate) fn arrive(&self) -> Wait {
        // Only an answer held subscribes, so [`Self::waiting`] counts no
        // other.
        if !self.0.borrow().held {
            return Wait(None);
        }
        let wakes = self.0.subscribe();
        // Read again through the subscription, so a wake since the check
        // above is either seen here or waited for.
        let Holding {
            held, wakes: seen, ..
        } = *wakes.borrow();
        Wait(held.then_some(Held { wakes, seen }))
    }
}

/// What a held answer waits for: nothing unless it was held.
#[must_use = "an answer held goes out only once its wait ends"]
pub(crate) struct Wait(Option<Held>);

/// An answer held, and the wakes it had seen when it was.
struct Held {
    wakes: watch::Receiver<Holding>,
    seen: u64,
}

impl Wait {
    /// Ends at the next wake, saying which it was.
    pub(crate) async fn woken(self) -> Wake {
        let Some(Held { mut wakes, seen }) = self.0 else {
            return Wake::Released;
        };
        let woken = wakes.wait_for(|holding| holding.wakes != seen).await;
        // Without a sender the fake is gone, and with it the server it
        // plays.
        woken.map_or(Wake::Ended, |holding| holding.wake)
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
        let hold = Hold::default();
        hold.hold();
        let wait = hold.arrive();
        assert_eq!(hold.waiting(), 1);
        hold.release();
        hold.hold();
        assert_eq!(wait.woken().now_or_never(), Some(Wake::Released));
        assert_eq!(hold.waiting(), 0);
    }
}
