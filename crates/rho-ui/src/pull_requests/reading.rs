//! What a Pull request surface shows, derived once from what
//! [`PullRequests`] holds ([`PullRequests::reading`]): the surface only
//! draws it.

use gpui_kit::SharedString;
use rho_ui_core::{
    BranchLookup, BranchPullRequest, LinkedPullRequest, PullRequestSummary, PullRequestTrouble,
};

use super::{not_connected, on, unserved, PullRequests, Serving, Shown, Which};
use crate::{forge::ForgeCli, machines::MachineId, time};

/// What the daemon last said of the PR a surface shows.
pub(crate) enum Seen<'a> {
    /// The session's machine isn't connected, as the sentence says.
    Disconnected(SharedString),
    /// Its daemon is from before pull requests, as the sentence says.
    Unserved(SharedString),
    /// Not answered yet, or still looking.
    Looking,
    /// There is no PR to show, and why, as a person reads it.
    Nothing(SharedString),
    Pr(&'a PullRequestSummary),
}

impl Seen<'_> {
    /// Whether the daemon can be asked about it: its machine is connected,
    /// and the daemon isn't from before pull requests.
    pub(crate) fn asks(&self) -> bool {
        match self {
            Seen::Disconnected(_) | Seen::Unserved(_) => false,
            Seen::Looking | Seen::Nothing(_) | Seen::Pr(_) => true,
        }
    }
}

/// What a surface shows; why it may be out of date, as a person reads it;
/// and the session's link to the PR it shows, if it links it.
pub(crate) struct Reading<'a> {
    pub(crate) seen: Seen<'a>,
    pub(crate) trouble: Option<SharedString>,
    pub(crate) link: Option<&'a LinkedPullRequest>,
}

impl PullRequests {
    /// What the surface that shows `shown` shows. Why it may be out of
    /// date is, first of: what the daemon says of the PR or the branch's
    /// lookup, why it refused to show it, why its machine's daemon last
    /// failed to answer.
    pub(crate) fn reading(&self, shown: &Shown) -> Reading<'_> {
        let machine = &shown.session.machine;
        let only = |seen| Reading {
            seen,
            trouble: None,
            link: None,
        };
        let Some(entry) = self.machines.get(machine) else {
            return only(Seen::Disconnected(not_connected(machine)));
        };
        if entry.serving == Serving::Unserved {
            return only(Seen::Unserved(unserved(machine)));
        }
        let refused = self
            .show_refused
            .as_ref()
            .filter(|(of, _)| of == shown)
            .map(|(_, why)| why)
            .or(entry.failed.as_ref())
            .cloned();
        let looking = Reading {
            seen: Seen::Looking,
            trouble: refused.clone(),
            link: None,
        };
        let Some(set) = entry.sessions.get(&shown.session.id) else {
            return looking;
        };
        let (seen, trouble, link) = match &shown.which {
            Which::Branch => {
                let Some(branch) = &set.branch else {
                    return looking;
                };
                let seen = match (set.branch_pull_request(), nothing_found(branch)) {
                    (Some(pr), _) => Seen::Pr(pr),
                    (None, Some(why)) => Seen::Nothing(why),
                    (None, None) => Seen::Looking,
                };
                let link = set.branch_pull_request().and_then(|pr| set.link(&pr.id));
                (seen, branch.trouble.as_ref(), link)
            }
            Which::Linked(id) => {
                let Some(link) = set.link(id) else {
                    // Not in the set yet, or just unlinked: its tab closes.
                    return looking;
                };
                match (&link.summary, &link.trouble) {
                    (Some(summary), trouble) => (Seen::Pr(summary), trouble.as_ref(), Some(link)),
                    // The message says it all.
                    (None, Some(PullRequestTrouble::NotFound { .. })) => {
                        let why = format!(
                            "There's no pull request #{} in {}/{}.",
                            id.number, id.owner, id.name
                        );
                        (Seen::Nothing(why.into()), None, Some(link))
                    }
                    (None, trouble) => (Seen::Looking, trouble.as_ref(), Some(link)),
                }
            }
        };
        Reading {
            seen,
            trouble: trouble
                .map(|trouble| trouble_text(trouble, machine))
                .or(refused),
            link,
        }
    }
}

/// Why the lookup of a session's branch found no PR, as a person reads
/// it; `None` while it looks, or once it found one.
fn nothing_found(branch: &BranchPullRequest) -> Option<SharedString> {
    let name = branch.branch.as_deref();
    let text = match &branch.lookup {
        BranchLookup::Looking | BranchLookup::Found { .. } => return None,
        BranchLookup::NotGit => "This session's folder isn't in a git repository.".to_owned(),
        BranchLookup::Detached { commit } => {
            format!("This session's checkout is on no branch, at {commit}.")
        }
        BranchLookup::NoForge => "This repository has no origin remote on github.com.".to_owned(),
        BranchLookup::DefaultBranch => match name {
            Some(name) => format!("This session works on {name}, the default branch."),
            None => "This session works on the default branch.".to_owned(),
        },
        BranchLookup::None => match name {
            Some(name) => format!("No pull request for {name}."),
            None => "No pull request for this branch.".to_owned(),
        },
        BranchLookup::OnlyForks { count } => {
            let from = name.map_or_else(
                || "this branch".to_owned(),
                |name| format!("a branch named {name}"),
            );
            format!(
                "The {count} newest pull requests from {from} all come from forks. If this \
                 repository has its own, it is older."
            )
        }
        // A lookup a newer rho sends.
        _ => "This rho-ui doesn't know what rho found. Update rho-ui.".to_owned(),
    };
    Some(text.into())
}

/// Why what shows of a PR on `machine` may be out of date, as a person
/// reads it. The daemon reads with the `gh` on its own `PATH`, which
/// Settings → Source control doesn't steer, so the fix is on that machine.
fn trouble_text(trouble: &PullRequestTrouble, machine: &MachineId) -> SharedString {
    let on = on(machine);
    match trouble {
        PullRequestTrouble::NotInstalled => {
            format!("gh isn't installed where rho runs on {on}: install it on rho's PATH there.")
                .into()
        }
        PullRequestTrouble::SignedOut { host } => {
            let login = ForgeCli::Gh.login_command(Some(host.as_str()));
            format!("gh on {on} isn't signed in to {host}: run `{login}` there.").into()
        }
        PullRequestTrouble::Paused {
            remaining,
            floor,
            resets_at,
            ..
        } => {
            // An upper bound, which stays true as the minutes pass unredrawn.
            // GitHub's limit resets hourly; hours are for a forge that doesn't.
            let minutes = (resets_at - time::now_unix())
                .max(0)
                .unsigned_abs()
                .div_ceil(60);
            let wait = if minutes <= 60 {
                format!("{minutes} min")
            } else {
                format!("{} h", minutes.div_ceil(60))
            };
            format!(
                "Paused: this GitHub account has {remaining} API points left, and rho leaves the \
                 last {floor} for other uses. Its limit resets within {wait}."
            )
            .into()
        }
        PullRequestTrouble::NotFound { message } | PullRequestTrouble::Failed { message } => {
            message.clone().into()
        }
        // A trouble a newer rho sends.
        _ => "rho reported a problem this rho-ui doesn't know. Update rho-ui.".into(),
    }
}
