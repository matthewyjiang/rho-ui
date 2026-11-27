//! The sidebar's filter: the popover beside the search, which stays open
//! while rows are clicked, and the chips under the search while a filter is
//! on (`sidebar/filter.rs`).

use std::collections::BTreeSet;

use crate::session_list::Filter;

use super::*;

/// The filter as the state database holds it.
fn saved(ui: &Harness) -> Filter {
    SavedFilter::load(&ui.state).filter().clone()
}

fn filter(hidden: &[&MachineId], projects: &[&Place]) -> Filter {
    Filter::new(
        hidden.iter().copied().cloned().collect(),
        projects.iter().copied().cloned().collect::<BTreeSet<_>>(),
    )
}

/// A project whose chip, with its machine's label, is wider than the
/// sidebar at its narrowest.
const LONG: &str = "/w/a-project-with-a-name-too-long-for-a-chip";

/// Opens the app on this machine, which holds the projects /w/a and /w/b,
/// and the workstation, which holds /w/a and one with a long name, each
/// with sessions in /w/a and /w/b, and a machine that can't be reached; it
/// waits for their rows.
/// The workstation's daemon runs while the returned one lives.
fn open_two_machines(name: &'static str) -> (Harness, FakeDaemon) {
    let local = FakeDaemon::start(Script::default());
    let remote = FakeDaemon::start(Script::default());
    let DaemonTarget::Socket(remote_socket) = remote.target() else {
        panic!("a fake daemon listens on a socket");
    };
    keep(&local, &["s1", "old"]);
    keep(&remote, &["s1", "old"]);
    let mut projects = Projects::default();
    projects.add(Place::local("/w/a"));
    projects.add(Place::local("/w/b"));
    projects.add(Place::new(MachineId::remote("workstation"), "/w/a"));
    projects.add(Place::new(MachineId::remote("workstation"), LONG));
    let mut settings = Settings::default();
    settings.machines = vec![
        RemoteMachine::new("workstation"),
        RemoteMachine::new("gone"),
    ];
    let mut ui = Harness::open_with(
        name,
        local.target(),
        Some(local),
        Machines {
            projects,
            settings,
            remote_sockets: vec![("workstation".to_owned(), remote_socket)],
            ..Machines::default()
        },
    );
    ui.wait_until("both machines' sessions, and Retry", |window, _, _| {
        is_visible(window, "session-local-old")
            && is_visible(window, "session-workstation-s1")
            && is_visible(window, "retry-gone")
    });
    (ui, remote)
}

/// The popover stays open while projects are chosen one after another;
/// Only on a machine shows it alone and drops the project chosen on the
/// machine it hides; Escape closes it. The filter then shows as chips,
/// which still fit a sidebar at its narrowest, a long name cut short
/// before its ×, and each chip's × takes its part off, until the dashed
/// Clear takes the rest and the row goes.
#[test]
fn the_filter_keeps_its_popover_open_and_shows_as_chips() {
    let (mut ui, _workstation) = open_two_machines("sidebar-filter");
    let (here, gone) = (MachineId::local(), MachineId::remote("gone"));
    let mine = Place::local("/w/a");
    let theirs = Place::new(MachineId::remote("workstation"), "/w/a");
    let long = Place::new(MachineId::remote("workstation"), LONG);
    ui.click("sidebar-filter");
    ui.wait_until("the popover", |window, _, _| {
        is_visible(window, "sidebar-filter-menu")
    });
    ui.act(|window, _| {
        assert_eq!(window.find("sidebar-filter-clear").disabled(), Some(true));
        assert!(
            !is_visible(window, "sidebar-filter-chips"),
            "no filter, no chips"
        );
    });
    ui.shot("open");

    // Two projects, one after another: the popover stays open between.
    ui.click("filter-project-local:/w/a");
    ui.click("filter-project-workstation:/w/a");
    assert_eq!(saved(&ui), filter(&[], &[&mine, &theirs]));
    ui.wait_until("their sessions alone, the popover open", |window, _, _| {
        is_visible(window, "sidebar-filter-menu")
            && is_visible(window, "session-local-s1")
            && is_visible(window, "session-workstation-s1")
            && !is_visible(window, "session-local-old")
    });
    ui.act(|window, _| {
        assert_eq!(
            window.find("filter-project-local:/w/a").checked(),
            Some(true)
        );
        assert_eq!(
            window.find("filter-project-local:/w/b").checked(),
            Some(false)
        );
        assert_ne!(window.find("sidebar-filter-clear").disabled(), Some(true));
    });

    // Only shows on hover, in place of the count.
    ui.act(|window, cx| window.hover("filter-machine-workstation", cx));
    ui.wait_until("Only", |window, _, _| {
        is_visible(window, "filter-only-machine-workstation")
            && !is_visible(window, "filter-only-machine-local")
    });
    ui.shot("only");
    ui.click("filter-only-machine-workstation");
    assert_eq!(saved(&ui), filter(&[&here, &gone], &[&theirs]));
    ui.wait_until("the workstation's session alone", |window, _, _| {
        is_visible(window, "session-workstation-s1") && !is_visible(window, "session-local-s1")
    });
    ui.click("filter-project-workstation:/w/a-project-with-a-name-too-long-for-a-chip");
    assert_eq!(saved(&ui), filter(&[&here, &gone], &[&theirs, &long]));

    ui.act(|window, cx| window.press("escape", cx));
    ui.wait_until("the popover closed, and the chips", |window, _, _| {
        !is_visible(window, "sidebar-filter-menu")
            && is_visible(window, "filter-chip-project-workstation:/w/a")
            && is_visible(window, "filter-chip-machine-local")
            && is_visible(window, "filter-chip-machine-gone")
    });
    ui.shot("chips");

    // At the sidebar's narrowest the chips wrap, each × and Clear still
    // inside it.
    ui.act(|window, cx| {
        let edge = window.find("sidebar").bounds();
        let from = gpui_kit::point(edge.right() - px(2.), edge.center().y);
        window.drag(from, gpui_kit::point(px(0.), from.y), cx);
    });
    ui.act(|window, _| {
        let sidebar = window.find("sidebar").bounds();
        for id in [
            "filter-chip-project-workstation:/w/a-project-with-a-name-too-long-for-a-chip",
            "sidebar-filter-chips-clear",
        ] {
            let chip = window.find(id).bounds();
            assert!(
                chip.right() <= sidebar.right(),
                "{id} fits: {chip:?} in {sidebar:?}"
            );
        }
    });
    ui.shot("chips-narrow");

    ui.click("filter-chip-machine-local");
    assert_eq!(saved(&ui), filter(&[&gone], &[&theirs, &long]));
    ui.click("filter-chip-project-workstation:/w/a");
    ui.click("filter-chip-project-workstation:/w/a-project-with-a-name-too-long-for-a-chip");
    assert_eq!(saved(&ui), filter(&[&gone], &[]));
    ui.wait_until("this machine's sessions back", |window, _, _| {
        is_visible(window, "session-local-old")
            && !is_visible(window, "filter-chip-machine-local")
            && is_visible(window, "filter-chip-machine-gone")
    });
    ui.click("sidebar-filter-chips-clear");
    assert!(saved(&ui).is_empty());
    ui.wait_until("no chips", |window, _, _| {
        !is_visible(window, "sidebar-filter-chips")
    });
}

/// With more than eight projects, the popover's search has the keyboard
/// as it opens and narrows the projects as it is typed in; Escape closes
/// the popover from it, and the search is empty when it opens again. Tab
/// goes on to the rows, where Space turns one on and Only shows.
#[test]
fn the_filters_search_narrows_many_projects() {
    let daemon = FakeDaemon::start(Script::default());
    keep(&daemon, &["s1", "old"]);
    let paths: Vec<String> = (1..=9).map(|n| format!("/w/p{n}")).collect();
    let mut ui = Harness::open(
        "sidebar-filter-search",
        daemon.target(),
        Some(daemon),
        Projects::of(paths.iter().map(String::as_str)),
    );
    ui.click("sidebar-filter");
    ui.wait_until("the popover's search", |window, _, _| {
        is_visible(window, "filter-project-local:/w/p9")
    });
    ui.act(|window, cx| window.input("p7", cx));
    ui.wait_until("p7 alone", |window, _, _| {
        is_visible(window, "filter-project-local:/w/p7")
            && !is_visible(window, "filter-project-local:/w/p1")
    });
    ui.shot("narrowed");

    ui.act(|window, cx| window.press("escape", cx));
    ui.wait_until("the popover closed", |window, _, _| {
        !is_visible(window, "sidebar-filter-menu")
    });
    ui.click("sidebar-filter");
    ui.wait_until("every project again", |window, _, _| {
        is_visible(window, "filter-project-local:/w/p1")
            && is_visible(window, "filter-project-local:/w/p7")
    });
    assert!(saved(&ui).is_empty(), "searching filters nothing");

    let first = "filter-project-local:/w/p1";
    // From the search, past Clear while it is off.
    ui.act(|window, cx| window.press("tab", cx));
    ui.act(|window, cx| window.press("space", cx));
    assert_eq!(saved(&ui), filter(&[], &[&Place::local("/w/p1")]));
    ui.wait_until("p1 on, with Only", |window, _, _| {
        window.find(first).checked() == Some(true)
            && is_visible(window, "filter-only-project-local:/w/p1")
            && is_visible(window, "sidebar-filter-menu")
    });
}
