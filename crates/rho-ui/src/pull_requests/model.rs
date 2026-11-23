//! What a pull request is, as rho-ui reads it from a forge: the
//! repositories, branches and PRs it names, what a lookup finds, and what
//! one batched query asks and answers. Plain data, shared by the forge
//! CLIs that read it ([`crate::forge`]) and [`super::PullRequests`],
//! which polls for it.

use gpui_kit::SharedString;

/// A repository on a forge, named as its origin remote's URL names it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RepoId {
    /// Such as `github.com`, in lowercase.
    pub(crate) host: String,
    pub(crate) owner: String,
    pub(crate) name: String,
}

impl RepoId {
    /// The repository a remote `url` names: `https://host/owner/name`,
    /// `ssh://git@host:22/owner/name.git`, or `git@host:owner/name.git`.
    /// `None` for a local path or a URL without an owner.
    pub(crate) fn from_url(url: &str) -> Option<Self> {
        let url = url.trim().trim_end_matches('/');
        let url = url.strip_suffix(".git").unwrap_or(url);
        // `scheme://[user@]host[:port]/path`, or scp's `[user@]host:path`.
        let (host, path) = if let Some((_, rest)) = url.split_once("://") {
            let (authority, path) = rest.split_once('/')?;
            let host = without_user(authority);
            (host.split_once(':').map_or(host, |(host, _)| host), path)
        } else {
            let (authority, path) = url.split_once(':')?;
            if authority.contains('/') {
                return None;
            }
            (without_user(authority), path)
        };
        let mut segments = path.rsplit('/').filter(|segment| !segment.is_empty());
        let name = segments.next()?;
        let owner = segments.next()?;
        if host.is_empty() {
            return None;
        }
        Some(Self {
            host: host.to_ascii_lowercase(),
            owner: owner.to_owned(),
            name: name.to_owned(),
        })
    }
}

/// `host` from a URL's `[user@]host`.
fn without_user(authority: &str) -> &str {
    authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host)
}

/// A branch of a repository, which may have a pull request.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct BranchId {
    pub(crate) repo: RepoId,
    pub(crate) branch: String,
}

/// A pull request, by its repository and number.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PrId {
    pub(crate) repo: RepoId,
    pub(crate) number: u32,
}

impl PrId {
    /// The PR `text` names: its URL (`https://github.com/owner/name/pull/12`),
    /// `owner/name#12`, or `#12` or `12` in `repo`.
    ///
    /// # Errors
    ///
    /// When `text` names no PR, or only a number with no `repo`: why.
    pub(crate) fn parse(text: &str, repo: Option<&RepoId>) -> Result<Self, &'static str> {
        const FORMS: &str = "Give the pull request's URL, owner/name#number, or #number.";
        let text = text.trim();
        let number = |text: &str| text.parse::<u32>().ok().filter(|&number| number > 0);
        if let Some((_, rest)) = text.split_once("://") {
            let mut parts = rest.split('/');
            let (Some(host), Some(owner), Some(name), Some("pull"), Some(n)) = (
                parts.next(),
                parts.next(),
                parts.next(),
                parts.next(),
                parts.next(),
            ) else {
                return Err(FORMS);
            };
            let number = number(n.split(['#', '?']).next().unwrap_or(n)).ok_or(FORMS)?;
            let repo = RepoId {
                host: without_user(host).to_ascii_lowercase(),
                owner: owner.to_owned(),
                name: name.to_owned(),
            };
            return Ok(Self { repo, number });
        }
        if let Some((repo_name, n)) = text.split_once('#').filter(|(name, _)| !name.is_empty()) {
            let (owner, name) = repo_name.split_once('/').ok_or(FORMS)?;
            let host = repo.map_or("github.com", |repo| repo.host.as_str());
            let repo = RepoId {
                host: host.to_owned(),
                owner: owner.to_owned(),
                name: name.to_owned(),
            };
            return Ok(Self {
                repo,
                number: number(n).ok_or(FORMS)?,
            });
        }
        let number = number(text.trim_start_matches('#')).ok_or(FORMS)?;
        let repo =
            repo.ok_or("This session's folder has no repository on a forge: give the URL.")?;
        Ok(Self {
            repo: repo.clone(),
            number,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrState {
    Open,
    Draft,
    Merged,
    Closed,
}

/// What a PR's checks come to, together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rollup {
    /// It has none.
    None,
    Pending,
    Passing,
    Failing,
}

/// What changes when anything about a PR does: its last update, its head
/// commit, and its checks, which change neither. A detail read with the
/// same key would read the same.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChangeKey {
    pub(crate) updated_at: String,
    pub(crate) head_oid: String,
    pub(crate) rollup: Rollup,
}

/// What a branch's lookup tells of its PR.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PrSummary {
    pub(crate) id: PrId,
    pub(crate) title: SharedString,
    pub(crate) url: SharedString,
    pub(crate) state: PrState,
    pub(crate) change: ChangeKey,
}

/// Whether a PR's reviews let it merge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Review {
    /// The repository asks for none, or none came.
    None,
    Required,
    ChangesRequested,
    Approved,
}

/// How one check stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum CheckStatus {
    // In the order the surface lists them: what needs a look first.
    Failed,
    Running,
    Pending,
    Cancelled,
    Passed,
    Neutral,
    Skipped,
}

/// One check of a PR's head commit: a CI job, or a commit status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CheckRun {
    pub(crate) name: SharedString,
    /// The CI workflow it ran in, if any.
    pub(crate) workflow: Option<SharedString>,
    pub(crate) status: CheckStatus,
    /// Where it shows its log or result.
    pub(crate) url: Option<SharedString>,
}

/// Everything the Pull request surface shows of a PR.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PrDetail {
    pub(crate) summary: PrSummary,
    pub(crate) author: SharedString,
    pub(crate) head: SharedString,
    pub(crate) base: SharedString,
    /// Its description, as Markdown.
    pub(crate) body: SharedString,
    pub(crate) additions: u32,
    pub(crate) deletions: u32,
    pub(crate) changed_files: u32,
    pub(crate) review: Review,
    /// The first checks, those that need a look first; see
    /// [`Self::checks_total`].
    pub(crate) checks: Vec<CheckRun>,
    /// How many checks there are, which may be more than `checks` holds.
    pub(crate) checks_total: u32,
    /// The newest comments and reviews, oldest first.
    pub(crate) posts: Vec<Post>,
    /// Comments and reviews older than `posts`, not read.
    pub(crate) older_posts: u32,
    /// The newest comment threads on lines of the code.
    pub(crate) threads: Vec<Thread>,
    /// Threads older than `threads`, not read.
    pub(crate) older_threads: u32,
    pub(crate) mergeable: Mergeable,
    /// The ways its repository lets it merge, the viewer's default first.
    pub(crate) merge_methods: Vec<MergeMethod>,
}

/// How a PR merges into its base.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

impl MergeMethod {
    /// What its button says.
    pub(crate) fn label(self) -> &'static str {
        match self {
            MergeMethod::Merge => "Merge",
            MergeMethod::Squash => "Squash and merge",
            MergeMethod::Rebase => "Rebase and merge",
        }
    }

    /// What its confirmation asks to do.
    pub(crate) fn verb(self) -> &'static str {
        match self {
            MergeMethod::Merge => "Merge",
            MergeMethod::Squash => "Squash",
            MergeMethod::Rebase => "Rebase",
        }
    }
}

/// Whether a PR merges cleanly into its base, as the forge last worked out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mergeable {
    Yes,
    Conflicting,
    /// Not worked out yet, or the PR is closed.
    Unknown,
}

/// What the surface asks the forge to do to a PR, as the session's account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PrAction {
    Comment(String),
    /// Marks a draft ready for review.
    Ready,
    /// Turns it back into a draft.
    Draft,
    /// Merges it, if its head is still commit `head`, the one the user saw:
    /// the session may push more. It doesn't ask to delete the branch, a
    /// session may work on it; a repository set to delete head branches
    /// on merge still does.
    Merge {
        method: MergeMethod,
        head: String,
    },
}

/// What someone said on a PR: a comment, a review, or a comment on a line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Post {
    pub(crate) author: SharedString,
    /// Where the forge serves the author's picture, which
    /// [`PullRequests::avatar`](super::PullRequests::avatar) fetches;
    /// `None` for a deleted account's.
    pub(crate) avatar: Option<SharedString>,
    /// Unix seconds.
    pub(crate) at: i64,
    /// Markdown; a review may have none.
    pub(crate) body: SharedString,
    /// What a review decided; `None` for a comment.
    pub(crate) verdict: Option<Verdict>,
    /// Where it shows on the forge, which also names it.
    pub(crate) url: SharedString,
}

/// What a review decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
}

/// Comments on one place in the code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Thread {
    /// The forge's own name for it.
    pub(crate) id: SharedString,
    pub(crate) path: SharedString,
    /// The line it is on, which an outdated thread no longer has.
    pub(crate) line: Option<u32>,
    pub(crate) state: ThreadState,
    /// The first comments, which start it.
    pub(crate) comments: Vec<Post>,
    /// Comments past `comments`, not read.
    pub(crate) more_comments: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThreadState {
    Open,
    /// Open, on code that has changed since.
    Outdated,
    Resolved,
}

/// What one poll asks one host, through one reader. The query asks for
/// each branch's PR, each linked PR's summary, and the full detail of each
/// PR in `details`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FetchPlan {
    pub(crate) branches: Vec<BranchId>,
    pub(crate) links: Vec<PrId>,
    pub(crate) details: Vec<PrId>,
}

impl FetchPlan {
    /// What it looks up, as the surface follows each.
    pub(super) fn targets(&self) -> impl Iterator<Item = Target> + '_ {
        let branches = self.branches.iter().cloned().map(Target::Branch);
        branches.chain(self.links.iter().cloned().map(Target::Pr))
    }

    pub(super) fn is_empty(&self) -> bool {
        self.branches.is_empty() && self.links.is_empty() && self.details.is_empty()
    }
}

/// What a forge said to one [`FetchPlan`].
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Batch {
    pub(crate) budget: Option<Budget>,
    /// Each repository's default branch, which has no PR of its own.
    pub(crate) default_branches: Vec<(RepoId, Option<String>)>,
    /// Each branch's PR; a branch of a missing repository isn't here.
    pub(crate) branches: Vec<(BranchId, Found)>,
    /// Each linked PR, [`Found::None`] when there is no such PR.
    pub(crate) links: Vec<(PrId, Found)>,
    pub(crate) details: Vec<(PrId, Option<PrDetail>)>,
    /// Repositories the account can't see, or that don't exist, with what
    /// the forge said.
    pub(crate) missing: Vec<(RepoId, String)>,
}

/// What a lookup found: a branch's PR, or a linked one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Found {
    Pr(PrSummary),
    None,
    /// The newest PRs it read, this many, all came from forks' branches of
    /// the same name: the repository's own may be older.
    OnlyForks(usize),
}

/// What a host's API allows its account before its limit resets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Budget {
    pub(crate) remaining: u32,
    /// Unix seconds.
    pub(crate) resets_at: i64,
}

/// What the Pull request surface follows: the PR of the session's branch,
/// or one linked by hand.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Target {
    Branch(BranchId),
    Pr(PrId),
}

impl Target {
    pub(crate) fn repo(&self) -> &RepoId {
        match self {
            Target::Branch(branch) => &branch.repo,
            Target::Pr(id) => &id.repo,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_urls_name_their_repository() {
        let repo = |host: &str, owner: &str, name: &str| {
            Some(RepoId {
                host: host.to_owned(),
                owner: owner.to_owned(),
                name: name.to_owned(),
            })
        };
        let cases = [
            (
                "https://github.com/acme/widgets",
                repo("github.com", "acme", "widgets"),
            ),
            (
                "https://github.com/acme/widgets.git/",
                repo("github.com", "acme", "widgets"),
            ),
            (
                "git@github.com:acme/widgets.git",
                repo("github.com", "acme", "widgets"),
            ),
            (
                "ssh://git@GHE.corp:2222/acme/widgets.git",
                repo("ghe.corp", "acme", "widgets"),
            ),
            (
                "https://me:token@github.com/acme/widgets",
                repo("github.com", "acme", "widgets"),
            ),
            ("/srv/git/widgets.git", None),
            ("file:///srv/git/widgets.git", None),
            ("https://github.com/widgets", None),
        ];
        for (url, expected) in cases {
            assert_eq!(RepoId::from_url(url), expected, "{url}");
        }
    }

    /// A PR is named by its URL, as `owner/name#N`, or by number in the
    /// session's repository.
    #[test]
    fn links_name_their_pr() {
        let widgets = RepoId {
            host: "github.com".to_owned(),
            owner: "acme".to_owned(),
            name: "widgets".to_owned(),
        };
        let pr = |owner: &str, name: &str, number| PrId {
            repo: RepoId {
                host: "github.com".to_owned(),
                owner: owner.to_owned(),
                name: name.to_owned(),
            },
            number,
        };
        let cases = [
            (
                "https://github.com/up/stream/pull/7",
                Some(pr("up", "stream", 7)),
            ),
            (
                "https://GitHub.com/up/stream/pull/7/files?w=1",
                Some(pr("up", "stream", 7)),
            ),
            (
                "https://github.com/up/stream/pull/7#issuecomment-1",
                Some(pr("up", "stream", 7)),
            ),
            ("up/stream#7", Some(pr("up", "stream", 7))),
            (" #12 ", Some(pr("acme", "widgets", 12))),
            ("12", Some(pr("acme", "widgets", 12))),
            ("https://github.com/up/stream/issues/7", None),
            ("#0", None),
            ("stream#7", None),
            ("widgets", None),
        ];
        for (text, expected) in cases {
            assert_eq!(PrId::parse(text, Some(&widgets)).ok(), expected, "{text}");
        }
        assert!(
            PrId::parse("#12", None).is_err(),
            "a number needs a repository"
        );
    }
}
