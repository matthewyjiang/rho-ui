//! The timeline rail the cards rho sends between replies sit on.

use std::time::Duration;

use gpui_kit::test::{TestEventExt as _, TestWindowExt as _};
use gpui_kit::SharedString;
use rho_ui_core::{
    fake_daemon::{FakeCard, Script, CARDS},
    Entry, LinkedPullRequest, Notice, NotificationId, PullRequestId, PullRequestLinkOrigin,
    RunState, Session, SessionId,
};

use super::{is_visible, Harness};
use crate::{rail, right_panel::PanelLayout, time};

/// The cards in `session`, with their entries.
fn cards(session: &Session) -> Vec<(usize, Notice)> {
    let entries = session.entries().iter().enumerate();
    entries
        .filter_map(|(ix, entry)| match entry {
            Entry::Notice(notice) => Some((ix, notice.clone())),
            _ => None,
        })
        .collect()
}

/// Row `n` of the card whose elements' IDs start with `key`.
fn line(key: &str, n: usize) -> SharedString {
    SharedString::from(format!("{key}-line-{n}"))
}

/// A conversation opened again shows its cards as rho saved them: each at
/// the time it happened, to the second, as rho's file keeps it; one row
/// for each event a card tells of; and past the first few, the cards that
/// don't need the user fold behind one row, whatever tone rho gave them,
/// while a peer's question, toned a warning, shows. A pull request's chip
/// shows it in the panel, and a session a card is about opens from its row.
#[test]
fn replayed_cards_keep_their_times_and_fold_by_urgency() {
    let script = Script {
        old_cards: true,
        ..Script::default()
    };
    let mut ui = Harness::start("rail-replayed", script);
    // rho tells of the PRs its agent watches, which links them.
    let old = SessionId::from("old");
    let pr = PullRequestId::new("github.com", "matthewyjiang", "rho-ui", 99);
    let url = "https://github.com/matthewyjiang/rho-ui/pull/99";
    let link = LinkedPullRequest::new(pr.clone(), url, PullRequestLinkOrigin::Agent, 1_767_258_000);
    ui.fake()
        .edit_pull_requests(&old, |set| set.linked.push(link));
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-old")
    });
    ui.click("session-local-old");
    ui.wait_for_session("the replayed cards", |session| {
        !session.is_replaying() && matches!(session.entries().last(), Some(Entry::Agent(_)))
    });
    let session = ui.session();
    let cards = cards(&session);
    assert_eq!(cards.len(), CARDS.len(), "every card is in the transcript");
    for ((_, notice), card) in cards.iter().zip(&CARDS) {
        assert_eq!(notice.id().map(NotificationId::as_str), Some(card.id));
        let live = ui
            .fake()
            .card_time(&old, card.id)
            .expect("the fake sent it");
        let saved = i64::try_from(live / 1_000 * 1_000).expect("a time in range");
        assert_eq!(
            notice.arrived().map(|at| at.timestamp_millis()),
            Some(saved),
            "card {} replays the time rho saved",
            card.id
        );
    }

    // `CARDS` says why: the comments, toned as an accent, and the routine
    // news fold; the question shows though it comes after the fourth.
    let folded = ["n-pr-101", "n-pr-2040", "n-sweep"];
    let key = |card: &FakeCard| format!("card-{}", card.id);
    let (shown, hidden): (Vec<&FakeCard>, Vec<&FakeCard>) =
        CARDS.iter().partition(|card| !folded.contains(&card.id));
    assert_eq!(shown.len(), rail::FOLD_AFTER + 1);
    ui.wait_until("each shown card's rows, and the fold", |window, _, _| {
        is_visible(window, "rail-fold-card-n-pr-101")
            && shown
                .iter()
                .all(|card| (0..card.items.len()).all(|n| is_visible(window, line(&key(card), n))))
    });
    ui.act(|window, cx| {
        for card in &shown {
            let past = line(&key(card), card.items.len());
            assert!(
                window.try_find(past).is_none(),
                "{}: a row per item",
                card.id
            );
        }
        for card in &hidden {
            assert!(
                !is_visible(window, line(&key(card), 0)),
                "{} folds",
                card.id
            );
        }
        // Its time says it in full, to the second rho saved, on hover.
        let time = window.find("card-n-test-time");
        let failed = cards[0].1.arrived().expect("a replayed time");
        assert_eq!(time.label(), Some(time::stamp(failed).as_str()));
        window.pointer_move(time.bounds().center(), None, cx);
    });
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.frame();
    ui.shot("replayed");

    ui.click("card-n-pr-99-chip-0");
    ui.wait_until("the PR in the panel", |window, cx, _| {
        PanelLayout::get(cx) && is_visible(window, "panel-tab-pull-request")
    });
    ui.wait_for_daemon("the card's PR asked for", |fake| {
        fake.pull_requests_shown(&old).last() == Some(&Some(pr.clone()))
    });

    ui.act(|window, cx| {
        let row = window.find(line("card-n-question", 0)).bounds().center();
        window.pointer_move(row, None, cx);
    });
    ui.click("card-n-question-open-0");
    ui.wait_for_session("the session the question is from", |session| {
        session.id().as_str() == "s1"
    });
}

/// Cards from a rho that sends no times and doesn't say what they are
/// about still sit on the rail, one row each, titled, at the time they
/// came: past the first few, those toned as routine news fold behind one
/// row, which unfolds them on a click, and an error or an accent never
/// does. A card's row opens its body under it.
#[test]
fn an_older_rhos_cards_sit_on_the_rail_by_their_tone() {
    let mut ui = Harness::start("rail", Script::default());
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.send("older cards");
    ui.wait_for_session("the answer after the cards", |session| {
        session.state() == &RunState::Idle
            && matches!(session.entries().last(), Some(Entry::Agent(_)))
    });
    let session = ui.session();
    let cards = cards(&session);
    assert_eq!(cards.len(), CARDS.len(), "every card is in the transcript");
    assert!(
        cards.iter().all(|(_, card)| card.arrived().is_some()),
        "a card that came live has the time it came"
    );
    let key = |ix: usize| format!("card-{ix}");
    // By tone, the peer's question, a warning, and the new commits fold;
    // the accents show.
    let folded = [cards[4].0, cards[6].0];
    let fold = SharedString::from(format!("rail-fold-{}", key(folded[0])));
    ui.wait_until("the cards that show, and the fold", |window, _, _| {
        is_visible(window, fold.clone())
            && cards
                .iter()
                .filter(|(ix, _)| !folded.contains(ix))
                .all(|(ix, _)| is_visible(window, line(&key(*ix), 0)))
    });
    ui.act(|window, _| {
        for (ix, _) in &cards {
            assert!(
                window.try_find(line(&key(*ix), 1)).is_none(),
                "card {ix}: one row"
            );
        }
        for ix in folded {
            assert!(
                !is_visible(window, line(&key(ix), 0)),
                "card {ix} is folded"
            );
        }
    });
    // The model answers after the cards as a turn of their own, opened
    // with its wake, as the conversation replays.
    let after = cards[cards.len() - 1].0 + 1;
    assert!(matches!(session.entries()[after], Entry::Wake));
    ui.shot("folded");

    ui.click(fold.to_string().leak());
    ui.wait_until("every card, unfolded", |window, _, _| {
        cards
            .iter()
            .all(|(ix, _)| is_visible(window, line(&key(*ix), 0)))
            && !is_visible(window, fold.clone())
    });
    let failed = cards[0].0;
    ui.click(line(&key(failed), 0).to_string().leak());
    ui.wait_until("the failure's body", |window, _, _| {
        is_visible(
            window,
            SharedString::from(format!("{}-detail", key(failed))),
        )
    });
    ui.shot("unfolded");
}
