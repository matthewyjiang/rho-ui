//! A session's side chats, from the view's side: who may start one, the
//! `/side` question that starts one from the composer, and how a side
//! chat whose tab closed winds down before the app deletes it.

use gpui_kit::{Context, Window};
use rho_ui_core::Prompt;

use super::{Link, Loading, Outgoing, Role, SessionEvent, SessionView};

/// How far a side chat whose tab closed is from being deleted
/// ([`SessionView::discard`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Discarding {
    No,
    /// It waits for nothing to run in it, as rho deletes no session with
    /// a turn under way or work running. `cancelling` while a cancel of
    /// the turn under way is on its way.
    WhenIdle {
        cancelling: bool,
    },
    /// The app was asked to delete it, once.
    Asked,
}

impl SessionView {
    /// Why the session can't start a side chat now, if it can't: rho forks
    /// only what a session has saved, and never a fork or a subagent's
    /// session.
    pub(crate) fn side_chat_refusal(&self) -> Option<&'static str> {
        match self.role {
            Role::Side(_) => Some("A side chat can't start another side chat."),
            Role::Subagent(_) => Some("A subagent's session can't start a side chat."),
            Role::Main if !self.prompted => {
                Some("A side chat starts after the session's first message.")
            }
            Role::Main => None,
        }
    }

    /// Asks the app for an empty side chat of this session, from the right
    /// panel, or says why it can't have one.
    pub(crate) fn start_side_chat(&mut self, cx: &mut Context<'_, Self>) {
        match self.side_chat_refusal() {
            Some(why) => {
                self.error = Some(why.to_owned());
                cx.notify();
            }
            None => cx.emit(SessionEvent::SideChat(None)),
        }
    }

    /// Asks the app for a side chat that asks `question`, with what the
    /// composer has attached, from a `/side` the composer holds. One the
    /// session can't have leaves the composer as it was, and says why.
    pub(super) fn send_side_question(
        &mut self,
        question: String,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some(why) = self.side_chat_refusal() {
            self.error = Some(why.to_owned());
            cx.notify();
            return;
        }
        let (_, attachments) = self
            .composer
            .update(cx, |composer, cx| composer.take(window, cx))
            .into_parts();
        let prompt = Prompt::new(question, attachments);
        cx.emit(SessionEvent::SideChat(
            (!prompt.is_empty()).then_some(prompt),
        ));
    }

    /// A side chat asked for with `prompt` didn't open, as the app says
    /// in a notification: the prompt goes back in the composer.
    pub(crate) fn side_chat_failed(
        &self,
        prompt: Option<Prompt>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some(prompt) = prompt {
            self.give_back(prompt, window, cx);
        }
    }

    /// A side chat whose attach failed shows only why: its first prompt
    /// goes back to the composer of the session it forks.
    pub(super) fn give_back_opening(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let (Some(prompt), Some(parent)) = (self.opening.take(), self.parent()) else {
            return;
        };
        parent.update(cx, |parent, cx| parent.give_back(prompt, window, cx));
    }

    /// The side chat's tab closed: winds it down, then asks the app to
    /// delete it ([`Self::discard_when_idle`]). What was still to send
    /// goes nowhere, and its running work is stopped.
    pub(crate) fn discard(&mut self, cx: &mut Context<'_, Self>) {
        if !self.is_side() || self.discarding != Discarding::No {
            return;
        }
        self.discarding = Discarding::WhenIdle { cancelling: false };
        self.opening = None;
        self.session.take_held();
        if matches!(self.outgoing, Some(Outgoing::Waiting { .. })) {
            self.outgoing = None;
        }
        let stoppable: Vec<_> = self
            .session
            .background()
            .iter()
            .filter(|work| work.can_stop())
            .map(|work| work.id().clone())
            .collect();
        let daemon = self.daemon.clone();
        let id = self.session.id().clone();
        cx.spawn(async move |_, _| {
            for work in stoppable {
                let _ = daemon.stop_background(&id, &work).await;
            }
        })
        .detach();
        self.discard_when_idle(cx);
    }

    /// Winds a discarded side chat down as it changes: cancels each turn
    /// that runs in it, as one a prompt already on its way or the daemon's
    /// own wake starts, and once its attach has answered and nothing runs,
    /// asks the app to delete it. One no connection will follow again is
    /// asked for at once: nothing here can change it.
    pub(super) fn discard_when_idle(&mut self, cx: &mut Context<'_, Self>) {
        let Discarding::WhenIdle { cancelling } = self.discarding else {
            return;
        };
        let unreachable = matches!(self.link, Link::Lost | Link::Refused(_));
        let busy = self.session.state().is_busy();
        if busy && !cancelling && !unreachable {
            let daemon = self.daemon.clone();
            let id = self.session.id().clone();
            // Should the cancel fail, the turn runs to its end first.
            cx.spawn(async move |_, _| {
                let _ = daemon.cancel(&id).await;
            })
            .detach();
        }
        self.discarding = Discarding::WhenIdle { cancelling: busy };
        let attached = matches!(self.loading, Loading::Ready | Loading::Failed(_));
        let idle = !self.is_busy() && self.session.background().is_empty();
        if unreachable || (attached && idle) {
            self.discarding = Discarding::Asked;
            cx.emit(SessionEvent::Discard);
        }
    }
}

/// The question of a prompt that asks for a side chat, `/side` or `/btw`
/// and what follows, as rho's terminal UI reads them: the rest of the
/// text, trimmed, which may be empty.
pub(super) fn side_question(text: &str) -> Option<&str> {
    let rest = text.strip_prefix('/')?;
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let (command, question) = rest.split_at(end);
    ["side", "btw"]
        .iter()
        .any(|name| command.eq_ignore_ascii_case(name))
        .then(|| question.trim())
}

#[cfg(test)]
mod tests {
    use super::side_question;

    /// `/side` and its alias `/btw` ask for a side chat, in any case, with
    /// or without a question; a command that only starts so does not.
    #[test]
    fn a_side_question_is_the_text_after_side_or_btw() {
        let cases = [
            ("/side what is this lock", Some("what is this lock")),
            ("/btw  why\nnot ", Some("why\nnot")),
            ("/SIDE x", Some("x")),
            ("/side", Some("")),
            ("/side\nmulti line", Some("multi line")),
            ("/sidebar open", None),
            ("side note", None),
            (" /side x", None),
            ("//side x", None),
        ];
        for (text, question) in cases {
            assert_eq!(side_question(text), question, "text {text:?}");
        }
    }
}
