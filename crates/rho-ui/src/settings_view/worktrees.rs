//! The Worktrees page: the git worktrees the chosen machine's Rho added
//! for sessions, by project, each named by its session and saying what a
//! prune would do with it; and Remove, which prunes them as
//! `rho worktrees prune` does (see [`crate::worktrees`]). Opening Settings,
//! or picking a machine, checks them afresh; the refresh beside the title
//! checks again.
//!
//! Every machine's Rho keeps its own worktrees. With more than one
//! machine, a picker heads the page; this machine shows first.

use std::{path::Path, rc::Rc};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    setting::{SettingField, SettingGroup, SettingItem, SettingPage},
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, App, Context, Entity, Hsla, InteractiveElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription,
};
use rho_ui_core::{ReportedWorktree, WorktreeVerdict};

use super::{
    machine_picker, page_header, unreachable_note, value, PickedMachine, SettingsView, Tone,
};
use crate::{
    machines::{MachineId, Machines, SessionKey},
    repo::folder_name,
    sidebar::SidebarView,
    worktrees::{Listing, Pass, Worktrees},
};

/// The page's own state: which machine it shows.
pub(super) struct WorktreesFields {
    worktrees: Entity<Worktrees>,
    /// The listed sessions, whose titles name their worktrees.
    sidebar: Entity<SidebarView>,
    machine: PickedMachine,
    _subscriptions: [Subscription; 3],
}

impl WorktreesFields {
    /// Checks this machine's worktrees afresh.
    pub(super) fn new(
        machines: &Entity<Machines>,
        worktrees: Entity<Worktrees>,
        sidebar: Entity<SidebarView>,
        cx: &mut Context<'_, SettingsView>,
    ) -> Self {
        worktrees.update(cx, |worktrees, cx| {
            worktrees.run(&MachineId::local(), Pass::Check, cx);
        });
        let subscriptions = [
            cx.observe(&worktrees, |_, _, cx| cx.notify()),
            // A session's title arriving.
            cx.observe(&sidebar, |_, _, cx| cx.notify()),
            // A machine connecting: check it, if it is the one shown.
            cx.observe(machines, |this, _, cx| {
                let machine = this.worktrees_machine(cx);
                this.worktrees
                    .worktrees
                    .update(cx, |worktrees, cx| worktrees.ensure_checked(&machine, cx));
                cx.notify();
            }),
        ];
        Self {
            worktrees,
            sidebar,
            machine: PickedMachine::new(),
            _subscriptions: subscriptions,
        }
    }
}

/// The line heading the page: what was found or done, why, and Remove
/// when there is something to remove.
struct Summary {
    title: String,
    about: Option<String>,
    removable: bool,
}

impl SettingsView {
    /// The machine the page shows.
    fn worktrees_machine(&self, cx: &App) -> MachineId {
        self.worktrees.machine.shown(self.machine_list.read(cx))
    }

    /// Shows `machine`'s worktrees, checked afresh.
    fn pick_worktrees_machine(&mut self, machine: MachineId, cx: &mut Context<'_, Self>) {
        self.worktrees
            .worktrees
            .update(cx, |worktrees, cx| worktrees.run(&machine, Pass::Check, cx));
        if self.worktrees.machine.pick(machine) {
            cx.notify();
        }
    }

    pub(super) fn worktrees_page(&self, cx: &mut Context<'_, Self>) -> SettingPage {
        let machine = self.worktrees_machine(cx);
        let view = cx.entity().downgrade();
        let machines = self.machine_list.read(cx);
        let mut page = SettingPage::new("Worktrees")
            .icon(Lucide::FolderGit2)
            .header_style(&page_header());
        if machines.several() {
            page = page.group(SettingGroup::new().item(machine_picker(
                machines,
                &machine,
                "Each machine's Rho keeps its own worktrees.",
                view,
                Self::pick_worktrees_machine,
            )));
        }
        if let Some(why) = unreachable_note(machines, &machine) {
            return page.group(
                SettingGroup::new().item(SettingItem::new("Status", value(why, Tone::Muted))),
            );
        }
        let state = self.worktrees.worktrees.read(cx);
        let running = state.running(&machine);
        page = page.title_suffix(refresh_button(&self.worktrees.worktrees, &machine, running));
        let (summary, shown): (Summary, Vec<&ReportedWorktree>) = match state.listing(&machine) {
            Some(Listing::Unchecked) | None => (
                Summary {
                    title: "Checking worktrees…".to_owned(),
                    about: None,
                    removable: false,
                },
                Vec::new(),
            ),
            Some(Listing::Failed(why)) => (
                Summary {
                    title: "Couldn't check worktrees".to_owned(),
                    about: Some(why.to_string()),
                    removable: false,
                },
                Vec::new(),
            ),
            Some(Listing::Checked(all)) => (checked(all), all.iter().collect()),
            Some(Listing::Pruned { removed, kept }) => {
                (pruned(*removed, kept.len()), kept.iter().collect())
            }
        };
        page = page.group(SettingGroup::new().item(summary_item(
            summary,
            &self.worktrees.worktrees,
            &machine,
            running,
        )));
        let sidebar = self.worktrees.sidebar.read(cx);
        let title = |worktree: &ReportedWorktree| {
            let key = SessionKey::new(machine.clone(), worktree.session.clone()?);
            sidebar.session_title(&key)
        };
        for (project, mut worktrees) in by_project(shown) {
            worktrees.sort_by_key(|worktree| describe(&worktree.verdict).0);
            let items: Vec<SettingItem> = worktrees
                .into_iter()
                .map(|worktree| worktree_item(worktree, title(worktree)))
                .collect();
            page = page.group(SettingGroup::new().title(folder_name(project)).items(items));
        }
        page
    }
}

/// The summary of a check of `all`.
fn checked(all: &[ReportedWorktree]) -> Summary {
    let removable = all
        .iter()
        .filter(|worktree| matches!(worktree.verdict, WorktreeVerdict::Merged { .. }))
        .count();
    let total = all.len();
    if total == 0 {
        return Summary {
            title: "No session worktrees".to_owned(),
            about: Some("A session started in a new worktree adds one.".to_owned()),
            removable: false,
        };
    }
    if removable == 0 {
        return Summary {
            title: "Nothing to remove".to_owned(),
            about: Some(format!("All {total} are kept, for the reason beside each.")),
            removable: false,
        };
    }
    Summary {
        title: format!("{removable} of {} can be removed", count(total)),
        about: Some("Closed, clean and merged. Their branches are kept.".to_owned()),
        removable: true,
    }
}

/// The summary of a prune that removed `removed` worktrees and kept `kept`.
fn pruned(removed: usize, kept: usize) -> Summary {
    let (title, about) = match removed {
        0 => (
            "Nothing was removed".to_owned(),
            format!("{} stay.", count(kept)),
        ),
        n => (
            format!("Removed {}", count(n)),
            match kept {
                0 => "Their branches are kept.".to_owned(),
                kept => format!("Their branches are kept. {} stay.", count(kept)),
            },
        ),
    };
    Summary {
        title,
        about: Some(about),
        removable: false,
    }
}

/// "1 worktree", "3 worktrees".
fn count(n: usize) -> String {
    match n {
        1 => "1 worktree".to_owned(),
        n => format!("{n} worktrees"),
    }
}

/// `summary`, with Remove when there is something to remove.
fn summary_item(
    summary: Summary,
    worktrees: &Entity<Worktrees>,
    machine: &MachineId,
    running: Option<Pass>,
) -> SettingItem {
    let prune = run_pass(worktrees, machine, Pass::Prune);
    let removable = summary.removable;
    let field = SettingField::render(move |_, _, _| {
        let prune = Rc::clone(&prune);
        div().when(removable, |this| {
            this.child(
                Button::new("worktrees-remove")
                    .primary()
                    .small()
                    .label("Remove")
                    .loading(running == Some(Pass::Prune))
                    .disabled(running.is_some())
                    .on_click(move |_, _, cx| prune(cx)),
            )
        })
    });
    described(SettingItem::new(summary.title, field), summary.about)
}

/// `item`, with `description` under its title if there is one. Kit gives
/// even an empty description a line, which sets the title off its control.
fn described(item: SettingItem, description: Option<String>) -> SettingItem {
    match description {
        Some(description) => item.description(description),
        None => item,
    }
}

/// Runs `pass` on `machine`'s worktrees, from a click.
fn run_pass(
    worktrees: &Entity<Worktrees>,
    machine: &MachineId,
    pass: Pass,
) -> Rc<dyn Fn(&mut App)> {
    let (worktrees, machine) = (worktrees.clone(), machine.clone());
    Rc::new(move |cx| {
        worktrees.update(cx, |worktrees, cx| worktrees.run(&machine, pass, cx));
    })
}

/// Checks `machine`'s worktrees again; it spins while a check runs.
fn refresh_button(
    worktrees: &Entity<Worktrees>,
    machine: &MachineId,
    running: Option<Pass>,
) -> impl Fn(&mut gpui_kit::Window, &mut App) -> Button + 'static {
    let check = run_pass(worktrees, machine, Pass::Check);
    move |_, _| {
        let check = Rc::clone(&check);
        Button::new("worktrees-refresh")
            .ghost()
            .xsmall()
            .icon(IconName::RotateCw)
            .tooltip("Check again")
            .loading(running == Some(Pass::Check))
            .disabled(running.is_some())
            .on_click(move |_, _, cx| check(cx))
    }
}

/// `worktrees` by the project each was added from, in the order each
/// project first comes.
fn by_project(worktrees: Vec<&ReportedWorktree>) -> Vec<(&Path, Vec<&ReportedWorktree>)> {
    let mut projects: Vec<(&Path, Vec<&ReportedWorktree>)> = Vec::new();
    for worktree in worktrees {
        let origin = worktree.worktree.origin.as_path();
        match projects.iter_mut().find(|(project, _)| *project == origin) {
            Some((_, of)) => of.push(worktree),
            None => projects.push((origin, vec![worktree])),
        }
    }
    projects
}

/// How much a worktree's verdict asks of the user, most first: a project
/// lists those that need them at its top.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Concern {
    /// Rho could not check or remove it.
    Failed,
    /// It holds work no commit has.
    Dirty,
    /// Its branch has work to merge.
    Unmerged,
    /// Its session is using it.
    InUse,
    /// Kept for a reason this version of rho-ui doesn't know.
    Other,
    /// A prune removes it.
    Removable,
}

impl Concern {
    /// The icon that flags it, for those that need the user.
    fn icon(self) -> Option<IconName> {
        match self {
            Self::Failed => Some(IconName::CircleX),
            Self::Dirty => Some(IconName::TriangleAlert),
            Self::Unmerged | Self::InUse | Self::Other | Self::Removable => None,
        }
    }

    fn color(self, cx: &App) -> Hsla {
        let theme = cx.theme();
        match self {
            Self::Failed => theme.danger,
            Self::Dirty => theme.warning,
            Self::Unmerged => theme.foreground,
            Self::InUse | Self::Other | Self::Removable => theme.muted_foreground,
        }
    }
}

/// What `verdict` asks of the user, how the page says it, and the error
/// if it failed.
fn describe(verdict: &WorktreeVerdict) -> (Concern, String, Option<String>) {
    match verdict {
        WorktreeVerdict::Failed { error } => (
            Concern::Failed,
            "Check failed".to_owned(),
            Some(error.clone()),
        ),
        WorktreeVerdict::Dirty => (Concern::Dirty, "Uncommitted changes".to_owned(), None),
        WorktreeVerdict::Unmerged { base } => {
            (Concern::Unmerged, format!("Not merged into {base}"), None)
        }
        WorktreeVerdict::InUse => (Concern::InUse, "In use".to_owned(), None),
        WorktreeVerdict::Occupied => (Concern::InUse, "In use by a process".to_owned(), None),
        WorktreeVerdict::Recent => (Concern::InUse, "Used in the last day".to_owned(), None),
        WorktreeVerdict::Locked => (Concern::Other, "Locked".to_owned(), None),
        WorktreeVerdict::Merged { base } => {
            (Concern::Removable, format!("Merged into {base}"), None)
        }
        _ => (Concern::Other, "Kept".to_owned(), None),
    }
}

/// One worktree, named by its session's `title` when it has one, else its
/// branch, with what a prune does with it. Its branch and path find it in
/// the page's search.
fn worktree_item(worktree: &ReportedWorktree, title: Option<SharedString>) -> SettingItem {
    let branch = &worktree.worktree.branch;
    let (concern, verdict, error) = describe(&worktree.verdict);
    let stale = worktree.stale.then(|| "Untouched for 14 days.".to_owned());
    let lines: Vec<String> = title
        .is_some()
        .then(|| branch.clone())
        .into_iter()
        .chain(stale)
        .chain(error)
        .collect();
    let description = (!lines.is_empty()).then(|| lines.join("\n"));
    let title = title.unwrap_or_else(|| branch.clone().into());
    // A worktree's path is unique on its machine; its branch need not be.
    let id = SharedString::from(format!("worktree-{}", worktree.worktree.path.display()));
    let label = SharedString::from(format!("{title}: {verdict}"));
    let verdict = SharedString::from(verdict);
    let field = SettingField::render(move |_, _, cx| {
        let color = concern.color(cx);
        h_flex()
            .id(id.clone())
            .test_support()
            .aria_label(label.clone())
            .gap_1p5()
            .text_sm()
            .text_color(color)
            .when_some(concern.icon(), |this, icon| {
                this.child(Icon::new(icon).small().text_color(color))
            })
            .child(verdict.clone())
    });
    let keywords = [
        SharedString::from(branch.clone()),
        SharedString::from(worktree.worktree.path.display().to_string()),
    ];
    described(
        SettingItem::new(title, field).keywords(keywords),
        description,
    )
}
