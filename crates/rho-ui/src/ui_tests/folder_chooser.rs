//! Add project… opens rho-ui's folder chooser (`folder_chooser.rs`), on
//! this machine and on a remote one, reached over the fake sshd, and adds
//! the folder chosen's project. Unix only: the fake sshd runs this
//! machine's `sh`, and Unix permissions make the unreadable folder.
#![cfg(unix)]

use rho_ui_core::fake_sshd::FakeSshd;

use super::*;

/// What the footer says Add adds, as its full path.
fn adds(window: &Window) -> Option<String> {
    window
        .try_find("folder-adds")
        .filter(gpui_kit::test::ElementSnapshot::visible)
        .and_then(|adds| adds.label().map(str::to_owned))
}

/// Whether the footer says Add adds `path`.
fn adds_path(path: &Path) -> impl Fn(&Window, &App, &RhoApp) -> bool {
    let path = path.display().to_string();
    move |window, _, _| adds(window).as_deref() == Some(path.as_str())
}

fn row(path: &Path) -> SharedString {
    format!("folder-row:{}", path.display()).into()
}

/// The app's projects' folders, on any machine.
fn added(ui: &mut Harness) -> Vec<Place> {
    ui.cx.update(|cx| ui.app.read(cx).projects(cx))
}

/// Checks the chooser closed, once Kit's dialog has faded out on the
/// harness's clock.
fn assert_closed(ui: &mut Harness) {
    ui.frame();
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.frame();
    ui.act(|window, _| assert!(!is_visible(window, "folder-add"), "the chooser closed"));
}

/// Opens the folder chooser from a new draft's Add project…, and lets
/// Kit's dialog fade in on the harness's clock, past a remote read's
/// debounce ([`crate::folders::REMOTE_PAUSE`]) too.
fn open_chooser(ui: &mut Harness) {
    ui.click("project-add");
    ui.wait_until("the folder chooser", |window, _, _| {
        is_visible(window, "folder-add")
    });
    ui.cx.advance_clock(Duration::from_secs(1));
}

/// On this machine, the chooser opens in the folder its projects are in,
/// with hidden folders left out. A name typed and Tab go into the one
/// folder it starts; a typed path goes where it leads, and a folder there
/// that can't be read says so and can't be added. Ctrl+Enter adds the
/// selected folder's project and moves the draft there.
#[test]
fn the_folder_chooser_adds_a_project_on_this_machine() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().expect("a temporary directory");
    let dir = platform::canonicalize(dir.path()).expect("it resolves");
    let code = dir.join("code");
    std::fs::create_dir_all(&code).expect("code");
    let git = |args: &[&str]| crate::local::test::git(&code, args);
    git(&["init", "-q", "-b", "main", "app"]);
    git(&["init", "-q", "-b", "feat", "lib"]);
    let (app, lib, notes, denied) = (
        code.join("app"),
        code.join("lib"),
        code.join("notes"),
        code.join("denied"),
    );
    for folder in [notes.join("deep"), code.join(".hidden"), denied.clone()] {
        std::fs::create_dir_all(folder).expect("a folder");
    }
    std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    // Root reads any folder.
    let unreadable = std::fs::read_dir(&denied).is_err();

    let daemon = FakeDaemon::start(Script::default());
    let mut ui = Harness::open(
        "folder-chooser-here",
        daemon.target(),
        Some(daemon),
        Projects::of([app.clone()]),
    );
    ui.wait_until("the sidebar", |window, _, _| {
        is_visible(window, "new-session")
    });
    ui.act(|window, cx| window.press("secondary-shift-n", cx));
    open_chooser(&mut ui);
    let listed = [row(&app), row(&lib), row(&notes)];
    let hidden = row(&code.join(".hidden"));
    ui.wait_until(
        "code's folders, the hidden one left out",
        move |window, _, _| {
            listed.iter().all(|id| is_visible(window, id.clone()))
                && !is_visible(window, hidden.clone())
        },
    );
    ui.wait_until("Add adds the folder shown", adds_path(&code));
    ui.shot("here");

    // `no` and Tab go into notes.
    ui.act(|window, cx| window.input("no", cx));
    ui.wait_until("notes, selected", adds_path(&notes));
    ui.shot("typed");
    ui.act(|window, cx| window.press("tab", cx));
    ui.wait_until("inside notes", move |window, cx, app| {
        is_visible(window, row(&notes.join("deep"))) && adds_path(&notes)(window, cx, app)
    });
    ui.shot("completed");

    // A path typed from notes goes up and into the unreadable folder.
    ui.act(|window, cx| window.input("../denied/", cx));
    if unreadable {
        ui.wait_until("why denied can't be read", |window, _, _| {
            is_visible(window, "folder-unread") && adds(window).as_deref() == Some("")
        });
        ui.act(|window, _| {
            let add = window.try_find("folder-add").expect("Add");
            assert_eq!(add.disabled(), Some(true), "nothing to add");
        });
        ui.shot("unreadable");
    }

    // Up to code, where `l` selects lib, and Ctrl+Enter adds it.
    ui.act(|window, cx| window.press(key("alt-up", "cmd-up"), cx));
    ui.wait_until("back in code", adds_path(&code));
    ui.act(|window, cx| window.input("l", cx));
    ui.wait_until("lib, selected", adds_path(&lib));
    ui.act(|window, cx| window.press("secondary-enter", cx));
    let picked = Place::local(&lib);
    ui.wait_until("the draft in lib", move |_, cx, app| {
        app.draft()
            .is_some_and(|draft| *draft.read(cx).cwd() == picked)
    });
    assert_closed(&mut ui);
    assert_eq!(added(&mut ui), [Place::local(&app), Place::local(&lib)]);
    std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// On a remote machine, the chooser reads its folders over ssh, git
/// checkouts and their branches too, and Add adds the selected folder's
/// project there.
#[test]
fn the_folder_chooser_adds_a_project_on_a_remote_machine() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let home = platform::canonicalize(dir.path())
        .expect("it resolves")
        .join("home");
    let code = home.join("code");
    std::fs::create_dir_all(code.join("notes")).expect("notes");
    let git = |args: &[&str]| crate::local::test::git(&code, args);
    git(&["init", "-q", "-b", "main", "api"]);
    git(&["init", "-q", "-b", "dev", "web"]);
    let (api, web) = (code.join("api"), code.join("web"));

    let remote = FakeDaemon::start(Script::default());
    std::fs::create_dir_all(home.join(".local/bin")).expect("~/.local/bin");
    write_program(
        &home.join(".local/bin/rho"),
        &format!(
            "#!/bin/sh\ncase \"$*\" in\n  'daemon status --json') printf '{{\"pid\": 7, \"socket\": \"%s\"}}\\n' '{}' ;;\nesac\n",
            remote.socket().display()
        ),
    );
    let sshd = FakeSshd::start(&home);
    let there = MachineId::remote("box");
    let mut settings = Settings::default();
    settings.machines = vec![RemoteMachine::new("box")];
    let mut projects = Projects::default();
    projects.add(Place::new(there.clone(), &api));
    let local = FakeDaemon::start(Script::default());
    let mut ui = Harness::open_with(
        "folder-chooser-remote",
        local.target(),
        Some(local),
        Machines {
            projects,
            settings,
            remote_ssh: vec![("box".to_owned(), sshd.target())],
            ..Machines::default()
        },
    );
    let reached = there.clone();
    ui.wait_until("box connected over ssh", move |_, cx, app| {
        matches!(
            app.machine_status(&reached, cx),
            Some(MachineStatus::Ready { .. })
        )
    });
    ui.act(|window, cx| window.press("secondary-shift-n", cx));
    ui.click("project-machine");
    click_menu_item(&mut ui, "box");
    let api_row: &'static str = format!("project-box:{}", api.display()).leak();
    ui.wait_until("box's project", move |window, _, _| {
        is_visible(window, api_row)
    });
    open_chooser(&mut ui);
    let listed = [row(&api), row(&web), row(&code.join("notes"))];
    ui.wait_until("box's folders, read over ssh", move |window, _, _| {
        listed.iter().all(|id| is_visible(window, id.clone()))
    });
    ui.wait_until("Add adds the folder shown", adds_path(&code));
    ui.shot("remote");

    ui.act(|window, cx| window.input("w", cx));
    ui.wait_until("web, selected", adds_path(&web));
    ui.click("folder-add");
    let picked = Place::new(there.clone(), &web);
    ui.wait_until("the draft in web on box", move |_, cx, app| {
        app.draft()
            .is_some_and(|draft| *draft.read(cx).cwd() == picked)
    });
    assert_closed(&mut ui);
    assert_eq!(
        added(&mut ui),
        [Place::new(there.clone(), &api), Place::new(there, &web)]
    );
}
