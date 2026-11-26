//! The notification cards the prompt "cards" sends between its two
//! replies, as rho's daemon builds them: the card a terminal draws, and
//! beside it what the card is about, typed. "older cards" sends the same
//! cards as a rho from before typed notifications does: no time, nothing
//! beside them.

use rho_protocol::{
    display::{
        NotificationCard, NotificationDelivery, NotificationId, NotificationInfo, NotificationItem,
        NotificationOutcome, NotificationPreview, NotificationSubject, NotificationTone,
        NotificationUrgency, NotificationVisibility,
    },
    methods::SessionId,
    CardTone, PullRequestId, SessionEvent,
};

use super::FakeSession;

/// What a [`FakeCard`] is about.
#[derive(Clone, Copy, Debug)]
pub enum FakeSubject {
    /// A pull request on github.com.
    PullRequest {
        /// `owner/name`.
        repo: &'static str,
        /// Its number in `repo`.
        number: u32,
    },
    /// Another of the fake's sessions.
    Session(&'static str),
    /// A subagent run that keeps no session of its own.
    Subagent(&'static str),
    /// A background process, by its id.
    Process(&'static str),
}

impl FakeSubject {
    /// As rho-protocol says it.
    ///
    /// # Panics
    ///
    /// If a pull request's repo isn't `owner/name`: [`CARDS`] has a bug.
    pub fn wire(self) -> NotificationSubject {
        match self {
            Self::PullRequest { repo, number } => {
                let (owner, name) = repo.split_once('/').expect("a repo is owner/name");
                NotificationSubject::PullRequest {
                    id: PullRequestId::new("github.com", owner, name, number),
                    url: format!("https://github.com/{repo}/pull/{number}"),
                }
            }
            Self::Session(id) => NotificationSubject::Session {
                id: SessionId::new(id),
            },
            Self::Subagent(run) => NotificationSubject::Subagent {
                run: run.to_owned(),
                session: None,
            },
            Self::Process(id) => NotificationSubject::Process { id: id.to_owned() },
        }
    }
}

/// A notification card, as the fake sends it.
#[derive(Clone, Copy, Debug)]
pub struct FakeCard {
    /// Its notification id, the same on every replay.
    pub id: &'static str,
    /// The title rho's terminal shows, which a rho that sends no items
    /// shows alone.
    pub title: &'static str,
    /// The tone rho gives the card, which, as rho's own does in places,
    /// may say otherwise than its urgency.
    pub tone: CardTone,
    /// What it asks of the reader.
    pub urgency: NotificationUrgency,
    /// What the card is about.
    pub subject: FakeSubject,
    /// One per event the card tells of, each about [`Self::subject`]: what
    /// happened, and the line that says it.
    pub items: &'static [(NotificationOutcome, &'static str)],
    /// Its body in Markdown, which opens under its row.
    pub body: &'static str,
}

/// The cards, as a lead gets them from its workers, processes, peers and
/// pull requests. Five show: the first four, as the rail always shows, and
/// a peer's question, which needs the user, though rho tones it a warning.
/// The rest fold: new comments rho tones as an accent, and routine news.
/// A rail that went by the tones instead would show the comments and the
/// subagent that stopped, and fold the question.
pub const CARDS: [FakeCard; 8] = [
    FakeCard {
        id: "n-test",
        title: "Failed (exit 101) · cargo test -j 6 -p rho-ui",
        tone: CardTone::Error,
        urgency: NotificationUrgency::NeedsYou,
        subject: FakeSubject::Process("p-3"),
        items: &[(
            NotificationOutcome::Failed,
            "Failed (exit 101) · cargo test -j 6 -p rho-ui",
        )],
        body: "test ui_tests::transcript_keeps_scroll_on_resize ... FAILED",
    },
    FakeCard {
        id: "n-pr-99",
        title: "New commits, checks passed · matthewyjiang/rho-ui#99",
        tone: CardTone::Success,
        urgency: NotificationUrgency::Notable,
        subject: FakeSubject::PullRequest {
            repo: "matthewyjiang/rho-ui",
            number: 99,
        },
        items: &[
            (
                NotificationOutcome::NewCommits,
                "New commits: the head is now 4f2a91c",
            ),
            (
                NotificationOutcome::ChecksPassed,
                "All 9 checks passed on 4f2a91c",
            ),
        ],
        body: "- New commits: the head is now `4f2a91c`.\n- All 9 checks passed on `4f2a91c`.",
    },
    FakeCard {
        id: "n-brief",
        title: "Completed · brief rereads",
        tone: CardTone::Success,
        urgency: NotificationUrgency::Notable,
        subject: FakeSubject::Subagent("run-7"),
        items: &[(NotificationOutcome::Completed, "Completed · brief rereads")],
        body: "Fixed and pushed to `feat/lead-brief-after-compaction`.",
    },
    FakeCard {
        id: "n-pr-102",
        title: "Merged · matthewyjiang/rho-ui#102",
        tone: CardTone::Success,
        urgency: NotificationUrgency::Notable,
        subject: FakeSubject::PullRequest {
            repo: "matthewyjiang/rho-ui",
            number: 102,
        },
        items: &[(
            NotificationOutcome::Merged,
            "Merged into main by matthewyjiang",
        )],
        body: "",
    },
    FakeCard {
        id: "n-question",
        title: "Waiting for an answer · first",
        tone: CardTone::Warning,
        urgency: NotificationUrgency::NeedsYou,
        subject: FakeSubject::Session("s1"),
        items: &[(
            NotificationOutcome::Question,
            "Waiting for an answer · first",
        )],
        body: "Which branch should the fix go on?",
    },
    FakeCard {
        id: "n-pr-101",
        title: "New comments · matthewyjiang/rho-ui#101",
        tone: CardTone::Accent,
        urgency: NotificationUrgency::Notable,
        subject: FakeSubject::PullRequest {
            repo: "matthewyjiang/rho-ui",
            number: 101,
        },
        items: &[(NotificationOutcome::NewComments, "1 new comment")],
        body: "- 1 new comment:\n  - coderabbitai: \"Split the status view out of #10?\"",
    },
    FakeCard {
        id: "n-pr-2040",
        title: "New commits · matthewyjiang/rho#2040",
        tone: CardTone::Neutral,
        urgency: NotificationUrgency::Routine,
        subject: FakeSubject::PullRequest {
            repo: "matthewyjiang/rho",
            number: 2040,
        },
        items: &[(
            NotificationOutcome::NewCommits,
            "New commits: the head is now 9c0e1d2",
        )],
        body: "",
    },
    FakeCard {
        id: "n-sweep",
        title: "Stopped · lint sweep",
        tone: CardTone::Accent,
        urgency: NotificationUrgency::Routine,
        subject: FakeSubject::Subagent("run-9"),
        items: &[(NotificationOutcome::Stopped, "Stopped · lint sweep")],
        body: "",
    },
];

impl FakeCard {
    /// The card's event, as every rho sends it.
    pub(super) fn event(&self) -> SessionEvent {
        let display_tone = match self.tone {
            CardTone::Accent => NotificationTone::Accent,
            CardTone::Success => NotificationTone::Success,
            CardTone::Warning => NotificationTone::Warning,
            CardTone::Error => NotificationTone::Error,
            _ => NotificationTone::Neutral,
        };
        SessionEvent::Card {
            title: self.title.to_owned(),
            markdown: self.body.to_owned(),
            plain: self.body.to_owned(),
            tone: self.tone,
            display: Some(Box::new(NotificationCard {
                title: self.title.to_owned(),
                sender: "github".to_owned(),
                recipient: "session".to_owned(),
                delivery: NotificationDelivery::Received,
                tone: display_tone,
                preview: NotificationPreview::Truncated,
                visibility: NotificationVisibility::Conversation,
                reference: None,
                subtitle: None,
                body: self.body.to_owned(),
                output: None,
                details: Vec::new(),
            })),
        }
    }

    /// What the card is about, as a rho that types notifications sends it
    /// beside the card.
    pub(super) fn info(&self) -> Box<NotificationInfo> {
        let subject = self.subject.wire();
        let items = self
            .items
            .iter()
            .map(|&(outcome, text)| NotificationItem::new(outcome, subject.clone(), text))
            .collect();
        Box::new(NotificationInfo::new(
            NotificationId::new(self.id),
            self.urgency,
            subject,
            items,
        ))
    }
}

/// Which rho sends a card.
#[derive(Clone, Copy, Debug)]
pub(super) enum Sender {
    /// One that types notifications: each card with when it happened and
    /// what it is about.
    Typed,
    /// One from before that: the card alone, untimed.
    Older,
}

impl FakeSession {
    /// Says it looks, as message `id`'s first part, then sends [`CARDS`]
    /// as `sender` does.
    pub(super) fn say_cards(&mut self, id: &str, sender: Sender) {
        self.say(
            &format!("{id}-a"),
            "Six workers running. Watching their PRs.",
        );
        for card in &CARDS {
            self.notify(card, sender);
        }
    }
}
