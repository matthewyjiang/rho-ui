//! The projects a session works in, and how they are drawn wherever a
//! session's project is named: one project as its badge and name, as
//! ever; several as their badges overlapped in a stack, which fans apart
//! while hovered, and their names together, as `rho-ui + rho`.
//!
//! Which projects a session has ([`session_workspaces`]): its own
//! workspace's, unless it is in no project ([`in_no_project`]), as a lead
//! or a session in a scratch folder is; then a lead's added directories,
//! the projects it was given, or a plain session's that are the user's
//! projects.

use gpui_kit::base::{spring, TestSupportExt as _};
use gpui_kit::component::{tooltip::Tooltip, ActiveTheme as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, rems, App, Div, ElementId, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    Pixels, Rems, RenderOnce, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _,
    Window,
};

use crate::{
    badge::Badge, leads::Kind, no_project::in_no_project, projects::Projects, workspaces::Workspace,
};

/// The most badges a stack draws; the names say how many more there are.
/// Three still read as separate tiles at the sidebar's 16px.
const MAX_TILES: usize = 3;

/// How far each badge in a stack starts after the one before it, as a
/// share of a badge's side: enough of each shows for its color to read.
const STACKED_STEP: f32 = 0.55;

/// The gap between fanned badges, in rems: 4px at the default scale, the
/// gap the app keeps between a badge and its name.
const FANNED_GAP_REMS: f32 = 0.25;

/// The ring around each stacked badge, in the color of the surface under
/// it, that cuts one badge from the next, in rems: about 1.5px at the
/// default scale, which reads as a cut rather than a border.
const RING_REMS: f32 = 0.09;

/// How much of a receded stack shows, as Kit's opacity would: the share
/// a shelved row's badge has kept.
const RECEDED: f32 = 0.4;

/// The workspaces whose projects a session of `kind` works in, first to
/// last: its `own` workspace, unless it is in no project
/// ([`in_no_project`]), then a lead's `added` directories, as it was given
/// its projects, or those of a plain session's in one of the user's
/// `projects`. A view not given the user's projects counts none of a plain
/// session's added directories. Two checkouts of one project both stay, so
/// a filter can match either's folder; [`session_projects`] names each
/// project once.
pub(crate) fn session_workspaces<'a>(
    kind: Kind,
    own: Option<&'a Workspace>,
    added: impl IntoIterator<Item = &'a Workspace>,
    projects: Option<&Projects>,
) -> Vec<&'a Workspace> {
    let is_project =
        |workspace: &Workspace| projects.is_some_and(|projects| is_users(workspace, projects));
    let own = own.filter(|own| !in_no_project(kind, own));
    let added = added
        .into_iter()
        .filter(|workspace| kind == Kind::Lead || is_project(workspace));
    own.into_iter().chain(added).collect()
}

/// The projects a session works in, as [`session_workspaces`] finds
/// them, each once: what a session is named by.
pub(crate) fn session_projects<'a>(
    kind: Kind,
    own: Option<&'a Workspace>,
    added: impl IntoIterator<Item = &'a Workspace>,
    projects: Option<&Projects>,
) -> Vec<&'a Workspace> {
    distinct(session_workspaces(kind, own, added, projects))
}

/// `workspaces`, keeping the first checkout of each project.
pub(crate) fn distinct(workspaces: Vec<&Workspace>) -> Vec<&Workspace> {
    let mut named: Vec<&Workspace> = Vec::with_capacity(workspaces.len());
    for workspace in workspaces {
        // Two checkouts of one project, as a worktree and its repository,
        // are one project.
        if !named
            .iter()
            .any(|known| known.project_key() == workspace.project_key())
        {
            named.push(workspace);
        }
    }
    named
}

/// Whether `workspace` is in one of the user's `projects`: their folder,
/// or a folder or worktree of it.
fn is_users(workspace: &Workspace, projects: &Projects) -> bool {
    projects
        .roots()
        .any(|root| *root == workspace.root() || root == workspace.place())
}

/// The most projects [`names`] names one by one; beyond it, it counts the
/// rest.
const NAMED_IN_FULL: usize = 2;

/// How [`names`] names a single project; several are always named
/// without their owners, to fit.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Naming {
    /// Without its owner, as the title bar names a project.
    Short,
    /// As `owner/name`, as the sidebar names one project.
    Full,
}

/// The projects' names together: one by `naming`, up to
/// [`NAMED_IN_FULL`] as `rho-ui + rho`, more as `rho-ui + 2 more`. Empty
/// with none.
pub(crate) fn names(projects: &[&Workspace], naming: Naming) -> SharedString {
    match projects {
        [] => SharedString::default(),
        [one] if naming == Naming::Full => one.name.clone(),
        [one] => one.short_name.clone(),
        named if named.len() <= NAMED_IN_FULL => named
            .iter()
            .map(|workspace| workspace.short_name.as_ref())
            .collect::<Vec<_>>()
            .join(" + ")
            .into(),
        [first, rest @ ..] => format!("{} + {} more", first.short_name, rest.len()).into(),
    }
}

/// Every project's full name, for a label that reads them all.
fn all_names(projects: &[&Workspace]) -> String {
    projects
        .iter()
        .map(|workspace| workspace.name.as_ref())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The projects' badges: one as it is, several overlapped, the first on
/// top, fanning apart while the pointer is over them so each reads alone.
/// The fan moves on Kit's control spring, as Kit's own controls do, and
/// snaps under reduced motion.
#[derive(IntoElement)]
pub(crate) struct ProjectStack {
    /// Keys the hover and the spring, so a stack keeps them across frames;
    /// unique in its window.
    id: ElementId,
    initials: Vec<SharedString>,
    /// Every project's full name, which the stack is labelled with.
    label: SharedString,
    /// Whether hovering it lists every project's name: when the names
    /// beside it leave some out, unless [`Self::untipped`].
    tooltip: bool,
    side: Rems,
    /// The color under the stack, which rings each badge; the theme's
    /// background unless [`Self::on`] says.
    surface: Option<Hsla>,
    receded: bool,
}

impl ProjectStack {
    /// The badges of `projects`, each `side` square; nothing with none.
    pub(crate) fn new(id: impl Into<ElementId>, projects: &[&Workspace], side: Rems) -> Self {
        Self {
            id: id.into(),
            initials: projects
                .iter()
                .take(MAX_TILES)
                .map(|workspace| workspace.initials.clone())
                .collect(),
            label: all_names(projects).into(),
            tooltip: projects.len() > NAMED_IN_FULL,
            side,
            surface: None,
            receded: false,
        }
    }

    /// Draws the stack on a surface of `surface`'s color.
    pub(crate) fn on(mut self, surface: Hsla) -> Self {
        self.surface = Some(surface);
        self
    }

    /// Lists no names on hover, where something else lists them.
    pub(crate) fn untipped(mut self) -> Self {
        self.tooltip = false;
        self
    }

    /// Shows the stack faded into its surface, as a shelved row's badge.
    pub(crate) fn receded(mut self) -> Self {
        self.receded = true;
        self
    }
}

impl RenderOnce for ProjectStack {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // A veil of the surface's color over the badges shows them as
        // opacity would, but keeps overlapped badges opaque to each other.
        let surface = self.surface.unwrap_or(cx.theme().background);
        let veil = self.receded.then(|| surface.opacity(1. - RECEDED));
        let with_veil = move |stack: Stateful<Div>| {
            stack.when_some(veil, |stack, veil| {
                stack.child(div().absolute().inset_0().bg(veil))
            })
        };
        let label = self.label.clone();
        let base = div()
            .id(self.id.clone())
            .relative()
            .flex_shrink_0()
            .when(self.tooltip, |base| {
                base.tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
            });
        match self.initials.as_slice() {
            // A badge's room, so what follows lines up with its
            // neighbors' in a list.
            [] => return base.size(self.side).test_support().into_any_element(),
            [one] => {
                let badge = Badge::new(one.clone(), self.side);
                return with_veil(base.child(badge))
                    .test_support()
                    .aria_label(self.label)
                    .into_any_element();
            }
            _ => {}
        }
        let rem = window.rem_size();
        let side = self.side.to_pixels(rem);
        let ring = rems(RING_REMS).to_pixels(rem);
        let hovered = window.use_keyed_state((self.id.clone(), "hovered"), cx, |_, _| false);
        let target = if *hovered.read(cx) { 1. } else { 0. };
        let policy = cx.theme().motion_tokens().spring_control;
        let fanned: f32 = spring((self.id.clone(), "fan"), target, policy, window, cx);
        let step = stack_step(side, rems(FANNED_GAP_REMS).to_pixels(rem), fanned);
        let corner = Badge::corner(self.side).to_pixels(rem) + ring;
        let mut left = Pixels::ZERO;
        let mut tiles = Vec::with_capacity(self.initials.len());
        for initials in self.initials {
            tiles.push(
                div()
                    .absolute()
                    .left(left - ring)
                    .top(-ring)
                    .p(ring)
                    .rounded(corner)
                    .bg(surface)
                    .child(Badge::new(initials, self.side)),
            );
            left += step;
        }
        // The last badge's start, plus its side.
        let width = left - step + side;
        // The first badge on top: the last drawn.
        tiles.reverse();
        with_veil(
            base.w(width)
                .h(side)
                .children(tiles)
                .on_hover(move |over, _, cx| {
                    hovered.update(cx, |hovered, cx| {
                        if *hovered != *over {
                            *hovered = *over;
                            cx.notify();
                        }
                    });
                }),
        )
        .test_support()
        .aria_label(self.label)
        .into_any_element()
    }
}

/// How far apart a stack's badges of side `side` start, `fanned` of the way
/// from overlapped to `gap` apart.
fn stack_step(side: Pixels, gap: Pixels, fanned: f32) -> Pixels {
    let stacked = side * STACKED_STEP;
    let apart = side + gap;
    stacked + (apart - stacked) * fanned.clamp(0., 1.)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lead names the projects it was given and never its own folder; a
    /// plain session names its workspace's project, unless that is a
    /// scratch folder, then the added directories that are the user's
    /// projects, each project once.
    #[test]
    fn a_session_names_its_projects_by_its_kind() {
        /// The session's kind, workspace and added directories, whether the
        /// view knows the user's projects, and the names it shows.
        type Case<'a> = (Kind, &'a Workspace, Vec<&'a Workspace>, bool, &'a [&'a str]);
        let user = Projects::of(["/w/rho-ui", "/w/rho"]);
        let leads = Workspace::local("/rho/leads/fix-it", "fix-it", "/rho/leads/fix-it");
        let ui = Workspace::local("/w/rho-ui", "me/rho-ui", "/w/rho-ui");
        let worktree = Workspace::local("/rho/worktrees/ui-1", "me/rho-ui", "/w/rho-ui");
        let rho = Workspace::local("/w/rho", "me/rho", "/w/rho");
        let crates = Workspace::local("/w/rho/crates", "me/rho", "/w/rho");
        let notes = Workspace::local("/tmp/notes", "notes", "/tmp/notes");
        let scratch =
            Workspace::local("/rho/scratch/try-it", "try-it", "/rho/scratch/try-it").scratch();
        let table: [Case<'_>; 9] = [
            (
                Kind::Lead,
                &leads,
                vec![&ui, &rho],
                true,
                &["rho-ui", "rho"],
            ),
            (Kind::Lead, &leads, vec![], true, &[]),
            // A lead's projects are what it was given, the user's or not.
            (Kind::Lead, &leads, vec![&notes], true, &["notes"]),
            (
                Kind::Session,
                &ui,
                vec![&rho, &notes],
                true,
                &["rho-ui", "rho"],
            ),
            // A scratch folder is no project, as a lead's folder is.
            (Kind::Session, &scratch, vec![], true, &[]),
            (Kind::Session, &scratch, vec![&rho, &notes], true, &["rho"]),
            // A folder in a project, and a worktree's own project.
            (
                Kind::Session,
                &worktree,
                vec![&ui, &crates],
                true,
                &["rho-ui", "rho"],
            ),
            (Kind::Session, &ui, vec![], true, &["rho-ui"]),
            (Kind::Session, &ui, vec![&rho], false, &["rho-ui"]),
        ];
        for (kind, own, added, knows, expected) in table {
            let projects = knows.then_some(&user);
            let named = session_projects(kind, Some(own), added, projects);
            let named: Vec<&str> = named.iter().map(|w| w.short_name.as_ref()).collect();
            assert_eq!(named, expected, "{kind:?} in {:?}", own.cwd());
        }
    }
}
