//! Pull requests through `gh`: one GraphQL query per [`FetchPlan`], built
//! with an alias for each repository (`r0`), branch (`b0`), linked PR
//! (`l0`) and PR read in full (`p0`),
//! and read back by those aliases. A repository the account can't see
//! fails only its own alias; the rest of the batch still answers.

use std::{ffi::OsString, fmt::Write as _};

use gpui_kit::SharedString;
use serde::Deserialize;
use serde_json::Value;

use super::{Job, QueryError};
use crate::{
    folders, local,
    pull_requests::model::{
        Batch, Budget, ChangeKey, CheckRun, CheckStatus, FetchPlan, Found, MergeMethod, Mergeable,
        Post, PrAction, PrDetail, PrId, PrState, PrSummary, RepoId, Review, Rollup, Thread,
        ThreadState, Verdict,
    },
    time,
};

/// The POSIX `sh` script that runs `gh api graphql`: `gh` is its first
/// argument, the host its second and the query its third. After
/// [`MARKER`] it prints what `gh` printed, a NUL, and its error output.
/// `gh` exits 1 when part of the query failed, as when a repository isn't
/// found, yet prints the rest of the answer, so its status says nothing.
pub(super) const SCRIPT: &str = r#"unset CDPATH
export GH_PROMPT_DISABLED=1 GH_NO_UPDATE_NOTIFIER=1 GH_SPINNER_DISABLED=1 NO_COLOR=1
printf 'rho-ui-gh-api\0'
exec 3>&1
err=$("$1" api graphql --hostname "$2" -f query="$3" </dev/null 2>&1 1>&3)
printf '\0%s\0' "$err"
"#;

/// Printed before `gh`'s answer, so whatever a login file prints comes
/// first and is skipped.
const MARKER: &[u8] = b"rho-ui-gh-api\0";

/// Asks GitHub: [`SCRIPT`] on a remote machine, [`api_here`] here.
pub(super) const API: Job = Job {
    script: SCRIPT,
    here: api_here,
};

/// [`SCRIPT`] in Rust, with the same arguments, printing what it prints.
fn api_here(args: &[OsString]) -> Result<Vec<u8>, String> {
    let [gh, host, query] = args else {
        return Err("The gh query takes gh, a host and a query.".to_owned());
    };
    let mut field = OsString::from("query=");
    field.push(query);
    let mut command = super::gh::command(gh);
    command
        .args(["api", "graphql", "--hostname"])
        .arg(host)
        .arg("-f")
        .arg(field);
    let output = local::run(&mut command)?;
    Ok([MARKER, &output.stdout, b"\0", &output.stderr, b"\0"].concat())
}

/// The POSIX `sh` script that acts on a PR: `gh` is its first argument,
/// then the host, the repository as `owner/name`, the PR's number, and the
/// `gh pr` subcommand with its arguments ([`action_args`]). With `--repo`,
/// `gh` touches no checkout. After [`ACTION_MARKER`] it prints `gh`'s exit
/// status, a NUL, and its error output.
pub(super) const ACTION_SCRIPT: &str = r#"unset CDPATH
export GH_PROMPT_DISABLED=1 GH_NO_UPDATE_NOTIFIER=1 GH_SPINNER_DISABLED=1 NO_COLOR=1
printf 'rho-ui-gh-pr\0'
gh=$1 repo=$2/$3 number=$4 verb=$5
shift 5
err=$("$gh" pr "$verb" "$number" --repo "$repo" "$@" </dev/null 2>&1 >/dev/null)
printf '%s\0%s\0' "$?" "$err"
"#;

const ACTION_MARKER: &[u8] = b"rho-ui-gh-pr\0";

/// Acts on a PR: [`ACTION_SCRIPT`] on a remote machine, [`action_here`]
/// here.
pub(super) const ACTION: Job = Job {
    script: ACTION_SCRIPT,
    here: action_here,
};

/// [`ACTION_SCRIPT`] in Rust, with the same arguments, printing what it
/// prints.
fn action_here(args: &[OsString]) -> Result<Vec<u8>, String> {
    let [gh, host, repo, number, verb, rest @ ..] = args else {
        return Err("A gh pr action takes gh, a repository, a number and a verb.".to_owned());
    };
    let mut full = host.clone();
    full.push("/");
    full.push(repo);
    let mut command = super::gh::command(gh);
    command
        .arg("pr")
        .arg(verb)
        .arg(number)
        .arg("--repo")
        .arg(full)
        .args(rest);
    let output = local::run(&mut command)?;
    // As `$?` reads, so 0 is success.
    let code = output.status.code().unwrap_or(1).to_string();
    Ok([ACTION_MARKER, code.as_bytes(), b"\0", &output.stderr, b"\0"].concat())
}

/// The `gh pr` subcommand and arguments that do `action`. A merge never
/// passes `--delete-branch`: a session may still work on the branch.
pub(super) fn action_args(action: &PrAction) -> Vec<OsString> {
    let args: Vec<&str> = match action {
        PrAction::Comment(body) => vec!["comment", "--body", body],
        PrAction::Ready => vec!["ready"],
        PrAction::Draft => vec!["ready", "--undo"],
        PrAction::Merge { method, head } => {
            let how = match method {
                MergeMethod::Merge => "--merge",
                MergeMethod::Squash => "--squash",
                MergeMethod::Rebase => "--rebase",
            };
            vec!["merge", how, "--match-head-commit", head]
        }
    };
    args.into_iter().map(OsString::from).collect()
}

/// Whether [`ACTION_SCRIPT`] did it, from what it printed.
///
/// # Errors
///
/// When it didn't: what `gh` said.
pub(super) fn parse_action(output: &[u8]) -> Result<(), String> {
    let at =
        folders::find(output, ACTION_MARKER).ok_or_else(|| "gh printed nothing.".to_owned())?;
    let mut fields = output[at + ACTION_MARKER.len()..].splitn(3, |&byte| byte == 0);
    let code = String::from_utf8_lossy(fields.next().unwrap_or_default()).into_owned();
    if code == "0" {
        return Ok(());
    }
    let error = String::from_utf8_lossy(fields.next().unwrap_or_default());
    let line = error.lines().map(str::trim).find(|line| !line.is_empty());
    Err(match line {
        Some(line) => line.strip_prefix("gh: ").unwrap_or(line).to_owned(),
        None => format!("gh exited with status {code}."),
    })
}

/// The PRs a branch's lookup reads, newest first, to find the one from
/// the repository itself: forks' PRs from a branch of the same name, such
/// as `patch-1`, come back too. When all of these are forks', the surface
/// says so rather than that there is none.
const BRANCH_PRS: usize = 5;

/// What a branch's lookup reads of each PR.
const SUMMARY: &str = "fragment Summary on PullRequest {
  number title url state isDraft isCrossRepository updatedAt headRefOid
  commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
}
";

/// What the surface shows of a PR. Whether it merges cleanly and how its
/// repository lets it merge cost nothing more (measured on
/// matthewyjiang/rho-ui#35: still 1 point, 0.46 s). Its first 50 checks, far past the 2
/// rho-ui's CI runs, and its newest 30 comments, 30 reviews and 30 threads
/// of 10 comments each still cost one point a query: measured on
/// rust-lang/rust#137944 (210 comments, 129 reviews, 84 threads), which
/// read in 1.5 s, as it did with 50 of each. The surface says how many more
/// there are. A new comment updates the PR's `updatedAt` (cli/cli#10513:
/// comment 20:42:15, update 20:42:16), so a poll sees it.
const DETAIL: &str = "fragment Detail on PullRequest {
  ...Summary
  body author { login } headRefName baseRefName additions deletions changedFiles reviewDecision
  mergeable repository {
    mergeCommitAllowed squashMergeAllowed rebaseMergeAllowed viewerDefaultMergeMethod
  }
  commits(last: 1) { nodes { commit { statusCheckRollup { state contexts(first: 50) {
    totalCount nodes {
      __typename
      ... on CheckRun { name status conclusion detailsUrl checkSuite { workflowRun { workflow { name } } } }
      ... on StatusContext { context state targetUrl }
    }
  } } } } }
  comments(last: 30) { totalCount nodes { ...Post } }
  reviews(last: 30) { totalCount nodes { author { login } body state submittedAt url } }
  reviewThreads(last: 30) { totalCount nodes {
    id isResolved isOutdated path line comments(first: 10) { totalCount nodes { ...Post } }
  } }
}
fragment Post on Comment {
  author { login } body createdAt
  ... on IssueComment { url } ... on PullRequestReviewComment { url }
}
";

/// The repositories `plan` names, each once, in the order it first names
/// them: their aliases' order.
fn repos(plan: &FetchPlan) -> Vec<&RepoId> {
    let mut repos: Vec<&RepoId> = Vec::new();
    let named = plan
        .branches
        .iter()
        .map(|branch| &branch.repo)
        .chain(plan.links.iter().map(|id| &id.repo))
        .chain(plan.details.iter().map(|id| &id.repo));
    for repo in named {
        if !repos.contains(&repo) {
            repos.push(repo);
        }
    }
    repos
}

/// The query for `plan`. Fragments are only defined where used, as GitHub
/// refuses a query that defines one it doesn't use.
pub(super) fn query(plan: &FetchPlan) -> String {
    let mut query = String::from("query {\n  rateLimit { remaining resetAt }\n");
    // Writing to a String can't fail.
    for (ix, repo) in repos(plan).into_iter().enumerate() {
        let _ = writeln!(
            query,
            "  r{ix}: repository(owner: {}, name: {}) {{\n    defaultBranchRef {{ name }}",
            literal(&repo.owner),
            literal(&repo.name)
        );
        for (ix, branch) in plan.branches.iter().enumerate() {
            if branch.repo == *repo {
                let _ = writeln!(
                    query,
                    "    b{ix}: pullRequests(headRefName: {}, first: {BRANCH_PRS}, orderBy: \
                     {{field: UPDATED_AT, direction: DESC}}) {{ nodes {{ ...Summary }} }}",
                    literal(&branch.branch)
                );
            }
        }
        for (ix, id) in plan.links.iter().enumerate() {
            if id.repo == *repo {
                let _ = writeln!(
                    query,
                    "    l{ix}: pullRequest(number: {}) {{ ...Summary }}",
                    id.number
                );
            }
        }
        for (ix, id) in plan.details.iter().enumerate() {
            if id.repo == *repo {
                let _ = writeln!(
                    query,
                    "    p{ix}: pullRequest(number: {}) {{ ...Detail }}",
                    id.number
                );
            }
        }
        query.push_str("  }\n");
    }
    query.push_str("}\n");
    if !plan.branches.is_empty() || !plan.links.is_empty() || !plan.details.is_empty() {
        query.push_str(SUMMARY);
    }
    if !plan.details.is_empty() {
        query.push_str(DETAIL);
    }
    query
}

/// `text` as a GraphQL string: JSON's escapes are GraphQL's.
fn literal(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_default()
}

/// What `gh` answered to `plan`'s query, from what [`SCRIPT`] printed.
///
/// # Errors
///
/// When `gh` answered nothing usable: why, and whether its account is
/// signed out.
pub(super) fn parse(plan: &FetchPlan, output: &[u8]) -> Result<Batch, QueryError> {
    let at = folders::find(output, MARKER)
        .ok_or_else(|| QueryError::Failed("The gh query printed nothing.".to_owned()))?;
    let mut fields = output[at + MARKER.len()..].splitn(2, |&byte| byte == 0);
    let answer = fields.next().unwrap_or_default();
    let error = String::from_utf8_lossy(fields.next().unwrap_or_default())
        .trim_end_matches('\0')
        .trim()
        .to_owned();
    let response: Value = match serde_json::from_slice(answer) {
        Ok(response) => response,
        Err(_) if !error.is_empty() => return Err(failure(&error)),
        Err(why) => return Err(QueryError::Failed(format!("gh's answer isn't JSON: {why}"))),
    };
    let Some(data) = response.get("data").filter(|data| data.is_object()) else {
        let messages: Vec<&str> = response["errors"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|error| error["message"].as_str())
            .collect();
        let why = if messages.is_empty() {
            error
        } else {
            messages.join(" ")
        };
        return Err(failure(&why));
    };
    let errors = response["errors"].as_array().map_or(&[][..], Vec::as_slice);
    batch(plan, data, errors)
}

/// Why a query failed, telling a refused or missing sign-in, which a new
/// check of the CLI would see, from anything else.
fn failure(why: &str) -> QueryError {
    let line = why.lines().next().unwrap_or(why).trim();
    let line = line.strip_prefix("gh: ").unwrap_or(line).to_owned();
    if super::gh::refused(why) || why.contains("gh auth login") {
        QueryError::SignedOut(line)
    } else {
        QueryError::Failed(line)
    }
}

/// The answer to `plan` in `data`, by its aliases. A repository the
/// account can't see, as `errors` says of its alias, is missing; a PR
/// number with no PR reads as none; any other error fails the whole
/// answer, as does one that doesn't read.
fn batch(plan: &FetchPlan, data: &Value, errors: &[Value]) -> Result<Batch, QueryError> {
    let repos = repos(plan);
    let mut batch = Batch {
        budget: budget(&data["rateLimit"]),
        ..Batch::default()
    };
    for error in errors {
        let message = error["message"]
            .as_str()
            .unwrap_or("GitHub refused the query.");
        let alias = match error["path"].as_array().map(Vec::as_slice) {
            Some([Value::String(alias)]) => alias.as_str(),
            // GitHub says so of a PR number with no PR, such as a typo in
            // a link, and answers its alias with null.
            Some([Value::String(_), Value::String(pr)])
                if error["type"] == "NOT_FOUND" && pr.starts_with(['l', 'p']) =>
            {
                continue;
            }
            _ => return Err(QueryError::Failed(message.to_owned())),
        };
        let repo = alias
            .strip_prefix('r')
            .and_then(|ix| ix.parse::<usize>().ok())
            .and_then(|ix| repos.get(ix));
        match (repo, error["type"].as_str()) {
            (Some(repo), Some("NOT_FOUND" | "FORBIDDEN")) => {
                batch.missing.push(((*repo).clone(), message.to_owned()));
            }
            _ => return Err(QueryError::Failed(message.to_owned())),
        }
    }
    let unreadable =
        |why: serde_json::Error| QueryError::Failed(format!("GitHub's answer didn't read: {why}"));
    for (ix, repo) in repos.into_iter().enumerate() {
        if batch.missing.iter().any(|(missing, _)| missing == repo) {
            continue;
        }
        let answer = &data[format!("r{ix}")];
        let default = answer["defaultBranchRef"]["name"]
            .as_str()
            .map(str::to_owned);
        batch.default_branches.push((repo.clone(), default));
        for (ix, branch) in plan.branches.iter().enumerate() {
            if branch.repo != *repo {
                continue;
            }
            let nodes = answer[format!("b{ix}")]["nodes"].clone();
            let found: Vec<GhSummary> = serde_json::from_value(nodes).map_err(unreadable)?;
            batch.branches.push((branch.clone(), pick(repo, found)));
        }
        for (ix, id) in plan.links.iter().enumerate() {
            if id.repo != *repo {
                continue;
            }
            // A linked PR may be a fork's.
            let read: Option<GhSummary> =
                serde_json::from_value(answer[format!("l{ix}")].clone()).map_err(unreadable)?;
            let found = read.map_or(Found::None, |read| Found::Pr(read.into_summary(repo)));
            batch.links.push((id.clone(), found));
        }
        for (ix, id) in plan.details.iter().enumerate() {
            if id.repo != *repo {
                continue;
            }
            let read: Option<GhDetail> =
                serde_json::from_value(answer[format!("p{ix}")].clone()).map_err(unreadable)?;
            batch
                .details
                .push((id.clone(), read.map(|read| read.into_detail(repo))));
        }
    }
    Ok(batch)
}

fn budget(rate_limit: &Value) -> Option<Budget> {
    let remaining = u32::try_from(rate_limit["remaining"].as_u64()?).ok()?;
    let resets_at = time::parse_utc(rate_limit["resetAt"].as_str()?)?;
    Some(Budget {
        remaining,
        resets_at,
    })
}

/// A branch's PR among `found`, newest first: one from the repository
/// itself, not a fork's of the same name, preferring one still open.
fn pick(repo: &RepoId, found: Vec<GhSummary>) -> Found {
    let read = found.len();
    let own: Vec<GhSummary> = found
        .into_iter()
        .filter(|pr| !pr.is_cross_repository)
        .collect();
    let open = own.iter().position(|pr| pr.state == "OPEN").unwrap_or(0);
    match own.into_iter().nth(open) {
        Some(pr) => Found::Pr(pr.into_summary(repo)),
        None if read == BRANCH_PRS => Found::OnlyForks(BRANCH_PRS),
        None => Found::None,
    }
}

/// A PR as a branch's lookup reads it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhSummary {
    number: u32,
    title: String,
    url: String,
    state: String,
    is_draft: bool,
    #[serde(default)]
    is_cross_repository: bool,
    updated_at: String,
    head_ref_oid: String,
    commits: Nodes<CommitNode>,
}

#[derive(Deserialize)]
struct Nodes<T> {
    nodes: Vec<T>,
}

#[derive(Deserialize)]
struct CommitNode {
    commit: Commit,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Commit {
    status_check_rollup: Option<GhRollup>,
}

#[derive(Deserialize)]
struct GhRollup {
    state: String,
    #[serde(default)]
    contexts: Option<Connection<GhCheck>>,
}

/// A check: a CI job, or a status another service set on the commit.
#[derive(Deserialize)]
#[serde(tag = "__typename", rename_all_fields = "camelCase")]
enum GhCheck {
    CheckRun {
        name: String,
        status: String,
        conclusion: Option<String>,
        details_url: Option<String>,
        check_suite: Option<CheckSuite>,
    },
    StatusContext {
        context: String,
        state: String,
        target_url: Option<String>,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CheckSuite {
    workflow_run: Option<WorkflowRun>,
}

#[derive(Deserialize)]
struct WorkflowRun {
    workflow: Workflow,
}

#[derive(Deserialize)]
struct Workflow {
    name: String,
}

/// A PR as its detail reads it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhDetail {
    #[serde(flatten)]
    summary: GhSummary,
    body: String,
    author: Option<Login>,
    head_ref_name: String,
    base_ref_name: String,
    additions: u32,
    deletions: u32,
    changed_files: u32,
    review_decision: Option<String>,
    mergeable: String,
    repository: GhMergeSettings,
    comments: Connection<GhComment>,
    reviews: Connection<GhReview>,
    review_threads: Connection<GhThread>,
}

/// How a PR's repository lets PRs merge.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhMergeSettings {
    merge_commit_allowed: bool,
    squash_merge_allowed: bool,
    rebase_merge_allowed: bool,
    viewer_default_merge_method: Option<String>,
}

impl GhMergeSettings {
    /// The methods allowed, the viewer's default first.
    fn methods(&self) -> Vec<MergeMethod> {
        let default = match self.viewer_default_merge_method.as_deref() {
            Some("SQUASH") => Some(MergeMethod::Squash),
            Some("REBASE") => Some(MergeMethod::Rebase),
            Some("MERGE") => Some(MergeMethod::Merge),
            _ => None,
        };
        let mut methods: Vec<MergeMethod> = [
            (MergeMethod::Merge, self.merge_commit_allowed),
            (MergeMethod::Squash, self.squash_merge_allowed),
            (MergeMethod::Rebase, self.rebase_merge_allowed),
        ]
        .into_iter()
        .filter_map(|(method, allowed)| allowed.then_some(method))
        .collect();
        methods.sort_by_key(|method| Some(*method) != default);
        methods
    }
}

#[derive(Deserialize)]
struct Login {
    login: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Connection<T> {
    total_count: u32,
    nodes: Vec<T>,
}

impl<T> Connection<T> {
    /// How many it has past those read.
    fn unread(&self) -> u32 {
        let read = u32::try_from(self.nodes.len()).unwrap_or(u32::MAX);
        self.total_count.saturating_sub(read)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhComment {
    author: Option<Login>,
    body: String,
    created_at: String,
    url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhReview {
    author: Option<Login>,
    body: String,
    state: String,
    submitted_at: Option<String>,
    url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhThread {
    id: String,
    is_resolved: bool,
    is_outdated: bool,
    path: String,
    line: Option<u32>,
    comments: Connection<GhComment>,
}

/// Whose it is: a deleted account's reads as GitHub shows it, `ghost`.
fn login(author: Option<Login>) -> SharedString {
    author.map_or_else(|| "ghost".into(), |author| author.login.into())
}

impl GhComment {
    fn into_post(self) -> Post {
        Post {
            author: login(self.author),
            at: time::parse_utc(&self.created_at).unwrap_or_default(),
            body: self.body.into(),
            verdict: None,
            url: self.url.into(),
        }
    }
}

impl GhReview {
    /// The review as a post, unless it says nothing: a review that only
    /// holds comments on lines, which their threads show, or one still
    /// being written.
    fn into_post(self) -> Option<Post> {
        let verdict = match self.state.as_str() {
            "APPROVED" => Verdict::Approved,
            "CHANGES_REQUESTED" => Verdict::ChangesRequested,
            "DISMISSED" => Verdict::Dismissed,
            "COMMENTED" if !self.body.trim().is_empty() => Verdict::Commented,
            _ => return None,
        };
        Some(Post {
            author: login(self.author),
            at: time::parse_utc(self.submitted_at.as_deref()?).unwrap_or_default(),
            body: self.body.into(),
            verdict: Some(verdict),
            url: self.url.into(),
        })
    }
}

/// The newest comments and reviews, oldest first, and how many older ones
/// weren't read. Of two lists cut short, each at its newest, the posts
/// older than the newer cut go too: the other list's posts from then
/// weren't read, and the timeline would have a hole.
fn timeline(comments: Connection<GhComment>, reviews: Connection<GhReview>) -> (Vec<Post>, u32) {
    let mut older = comments.unread() + reviews.unread();
    let oldest = |posts: &[Post], unread: u32| {
        (unread > 0)
            .then(|| posts.iter().map(|post| post.at).min())
            .flatten()
    };
    let comments_unread = comments.unread();
    let reviews_unread = reviews.unread();
    let comments: Vec<Post> = comments
        .nodes
        .into_iter()
        .map(GhComment::into_post)
        .collect();
    let reviews: Vec<Post> = reviews
        .nodes
        .into_iter()
        .filter_map(GhReview::into_post)
        .collect();
    let cut = oldest(&comments, comments_unread).max(oldest(&reviews, reviews_unread));
    let mut posts: Vec<Post> = comments.into_iter().chain(reviews).collect();
    if let Some(cut) = cut {
        let before = posts.len();
        posts.retain(|post| post.at >= cut);
        older += u32::try_from(before - posts.len()).unwrap_or(u32::MAX);
    }
    posts.sort_by_key(|post| post.at);
    (posts, older)
}

impl GhThread {
    fn into_thread(self) -> Thread {
        let more_comments = self.comments.unread();
        Thread {
            id: self.id.into(),
            path: self.path.into(),
            line: self.line,
            state: match (self.is_resolved, self.is_outdated) {
                (true, _) => ThreadState::Resolved,
                (false, true) => ThreadState::Outdated,
                (false, false) => ThreadState::Open,
            },
            comments: self
                .comments
                .nodes
                .into_iter()
                .map(GhComment::into_post)
                .collect(),
            more_comments,
        }
    }
}

impl GhSummary {
    fn rollup(&self) -> Option<&GhRollup> {
        self.commits
            .nodes
            .last()?
            .commit
            .status_check_rollup
            .as_ref()
    }

    fn into_summary(self, repo: &RepoId) -> PrSummary {
        let rollup = match self.rollup().map(|rollup| rollup.state.as_str()) {
            None => Rollup::None,
            Some("SUCCESS") => Rollup::Passing,
            Some("FAILURE" | "ERROR") => Rollup::Failing,
            Some(_) => Rollup::Pending,
        };
        let state = match (self.state.as_str(), self.is_draft) {
            ("MERGED", _) => PrState::Merged,
            ("CLOSED", _) => PrState::Closed,
            (_, true) => PrState::Draft,
            _ => PrState::Open,
        };
        PrSummary {
            id: PrId {
                repo: repo.clone(),
                number: self.number,
            },
            title: self.title.into(),
            url: self.url.into(),
            state,
            change: ChangeKey {
                updated_at: self.updated_at,
                head_oid: self.head_ref_oid,
                rollup,
            },
        }
    }
}

impl GhDetail {
    fn into_detail(mut self, repo: &RepoId) -> PrDetail {
        let contexts = self
            .summary
            .commits
            .nodes
            .last_mut()
            .and_then(|node| node.commit.status_check_rollup.as_mut())
            .and_then(|rollup| rollup.contexts.take());
        let (mut checks, checks_total) = match contexts {
            Some(contexts) => (
                contexts
                    .nodes
                    .into_iter()
                    .map(GhCheck::into_check)
                    .collect(),
                contexts.total_count,
            ),
            None => (Vec::new(), 0),
        };
        checks.sort_by_key(|check: &CheckRun| check.status);
        let review = match self.review_decision.as_deref() {
            Some("APPROVED") => Review::Approved,
            Some("CHANGES_REQUESTED") => Review::ChangesRequested,
            Some("REVIEW_REQUIRED") => Review::Required,
            _ => Review::None,
        };
        let (posts, older_posts) = timeline(self.comments, self.reviews);
        let mergeable = match self.mergeable.as_str() {
            "MERGEABLE" => Mergeable::Yes,
            "CONFLICTING" => Mergeable::Conflicting,
            _ => Mergeable::Unknown,
        };
        let older_threads = self.review_threads.unread();
        PrDetail {
            posts,
            older_posts,
            threads: self
                .review_threads
                .nodes
                .into_iter()
                .map(GhThread::into_thread)
                .collect(),
            older_threads,
            summary: self.summary.into_summary(repo),
            author: self
                .author
                .map(|author| author.login)
                .unwrap_or_default()
                .into(),
            head: self.head_ref_name.into(),
            base: self.base_ref_name.into(),
            body: self.body.into(),
            additions: self.additions,
            deletions: self.deletions,
            changed_files: self.changed_files,
            review,
            checks,
            checks_total,
            mergeable,
            merge_methods: self.repository.methods(),
        }
    }
}

impl GhCheck {
    fn into_check(self) -> CheckRun {
        match self {
            GhCheck::CheckRun {
                name,
                status,
                conclusion,
                details_url,
                check_suite,
            } => {
                let status = match (status.as_str(), conclusion.as_deref()) {
                    ("COMPLETED", Some("SUCCESS")) => CheckStatus::Passed,
                    ("COMPLETED", Some("CANCELLED")) => CheckStatus::Cancelled,
                    ("COMPLETED", Some("SKIPPED")) => CheckStatus::Skipped,
                    ("COMPLETED", Some("NEUTRAL" | "STALE")) => CheckStatus::Neutral,
                    ("COMPLETED", _) => CheckStatus::Failed,
                    ("IN_PROGRESS", _) => CheckStatus::Running,
                    _ => CheckStatus::Pending,
                };
                let workflow = check_suite
                    .and_then(|suite| suite.workflow_run)
                    .map(|run| SharedString::from(run.workflow.name));
                CheckRun {
                    name: name.into(),
                    workflow,
                    status,
                    url: details_url.map(SharedString::from),
                }
            }
            GhCheck::StatusContext {
                context,
                state,
                target_url,
            } => CheckRun {
                name: context.into(),
                workflow: None,
                status: match state.as_str() {
                    "SUCCESS" => CheckStatus::Passed,
                    "FAILURE" | "ERROR" => CheckStatus::Failed,
                    _ => CheckStatus::Pending,
                },
                url: target_url.map(SharedString::from),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pull_requests::model::BranchId;

    fn repo(name: &str) -> RepoId {
        RepoId {
            host: "github.com".to_owned(),
            owner: "matthewyjiang".to_owned(),
            name: name.to_owned(),
        }
    }

    fn branch(name: &str, branch: &str) -> BranchId {
        BranchId {
            repo: repo(name),
            branch: branch.to_owned(),
        }
    }

    /// What `gh` printed, as [`SCRIPT`] wraps it.
    fn printed(answer: &str, error: &str) -> Vec<u8> {
        let mut output = b"motd\n".to_vec();
        output.extend(MARKER);
        output.extend(answer.as_bytes());
        output.extend(format!("\0{error}\0").as_bytes());
        output
    }

    /// The plan the fixture answers: two branches and a PR of rho-ui, and
    /// a branch of a repository that doesn't exist. The fixture is a real
    /// answer to [`query`]'s query for it, its PR's body cut short.
    fn fixture_plan() -> FetchPlan {
        FetchPlan {
            branches: vec![
                branch("rho-ui", "ci-and-pr-template"),
                branch("rho-ui", "no-such-branch"),
                branch("does-not-exist-xyz", "main"),
            ],
            links: Vec::new(),
            details: vec![PrId {
                repo: repo("rho-ui"),
                number: 32,
            }],
        }
    }

    /// The query names each repository once, escapes what it quotes, and
    /// defines only the fragments it spreads, as GitHub refuses one defined
    /// and unused. (The fixture is GitHub's answer to the rest.)
    #[test]
    fn the_query_aliases_each_repository_once() {
        let text = query(&fixture_plan());
        assert_eq!(text.matches("repository(").count(), 2);

        let lookups = FetchPlan {
            branches: vec![branch("rho-ui", "say-\"hi\"")],
            links: Vec::new(),
            details: Vec::new(),
        };
        let text = query(&lookups);
        assert!(text.contains(r#"headRefName: "say-\"hi\"""#), "{text}");
        assert!(!text.contains("fragment Detail"), "defined but unused");
    }

    /// A real answer: a branch's own PR with its checks passed, a branch
    /// without one, a PR's detail with its checks, and a repository that
    /// isn't there, which fails only its own alias.
    #[test]
    fn a_real_answer_reads_by_its_aliases() {
        let plan = fixture_plan();
        let answer = include_str!("fixtures/gh_batch.json");
        let error = "gh: Could not resolve to a Repository with the name 'matthewyjiang/does-not-exist-xyz'.";
        let batch = parse(&plan, &printed(answer, error)).unwrap();
        assert_eq!(
            batch.budget,
            Some(Budget {
                remaining: 4988,
                resets_at: time::parse_utc("2026-11-15T06:18:47Z").unwrap()
            })
        );
        assert_eq!(
            batch.missing,
            [(
                repo("does-not-exist-xyz"),
                "Could not resolve to a Repository with the name \
                 'matthewyjiang/does-not-exist-xyz'."
                    .to_owned()
            )]
        );
        assert_eq!(
            batch.default_branches,
            [(repo("rho-ui"), Some("main".to_owned()))]
        );
        let [(_, Found::Pr(found)), (_, Found::None)] = batch.branches.as_slice() else {
            panic!("one PR and no PR: {:?}", batch.branches);
        };
        assert_eq!(found.id.number, 32);
        assert_eq!(found.state, PrState::Merged);
        assert_eq!(found.change.rollup, Rollup::Passing);
        let [(_, Some(detail))] = batch.details.as_slice() else {
            panic!("PR 32's detail: {:?}", batch.details);
        };
        assert_eq!(detail.summary, *found, "the detail reads the same summary");
        assert_eq!(
            (detail.additions, detail.deletions, detail.changed_files),
            (111, 4, 8)
        );
        assert_eq!(detail.checks_total, 2);
        assert!(detail
            .checks
            .iter()
            .all(|check| check.status == CheckStatus::Passed
                && check.workflow.as_deref() == Some("CI")));
    }

    /// A link to a PR number with no PR reads as no PR, in GitHub's real
    /// words for it, and leaves the rest of the answer alone.
    #[test]
    fn a_link_to_no_pr_finds_none() {
        let plan = FetchPlan {
            branches: vec![branch("rho-ui", "feature")],
            links: vec![PrId {
                repo: repo("rho-ui"),
                number: 999_999_999,
            }],
            details: Vec::new(),
        };
        let answer = r#"{"data":{"rateLimit":{"remaining":4990,"resetAt":"2026-11-15T06:18:47Z"},
            "r0":{"defaultBranchRef":{"name":"main"},"b0":{"nodes":[]},"l0":null}},
            "errors":[{"type":"NOT_FOUND","path":["r0","l0"],"locations":[{"line":4,"column":5}],
            "message":"Could not resolve to a PullRequest with the number of 999999999."}]}"#;
        let batch = parse(&plan, &printed(answer, "")).unwrap();
        assert!(matches!(batch.links.as_slice(), [(_, Found::None)]));
        assert!(matches!(batch.branches.as_slice(), [(_, Found::None)]));
        assert!(batch.missing.is_empty());
    }

    /// A real conversation, cli/cli#14578, bodies cut short: its comments
    /// and reviews in one timeline, oldest first, without the reviews that
    /// only hold comments on lines; and its threads, resolved, on lines
    /// since changed or gone.
    #[test]
    fn a_real_conversation_reads_oldest_first() {
        let id = PrId {
            repo: RepoId {
                host: "github.com".to_owned(),
                owner: "cli".to_owned(),
                name: "cli".to_owned(),
            },
            number: 14578,
        };
        let plan = FetchPlan {
            branches: Vec::new(),
            links: Vec::new(),
            details: vec![id],
        };
        let answer = include_str!("fixtures/gh_conversation.json");
        let batch = parse(&plan, &printed(answer, "")).unwrap();
        let [(_, Some(detail))] = batch.details.as_slice() else {
            panic!("the PR's detail: {:?}", batch.details);
        };
        let said: Vec<(&str, Option<Verdict>)> = detail
            .posts
            .iter()
            .map(|post| (post.author.as_ref(), post.verdict))
            .collect();
        assert_eq!(
            said,
            [
                ("copilot-pull-request-reviewer", Some(Verdict::Commented)),
                ("BagToad", None),
                ("copilot-pull-request-reviewer", Some(Verdict::Commented)),
                ("BagToad", None),
                ("williammartin", None),
                ("BagToad", None),
                ("BagToad", Some(Verdict::Approved)),
                ("williammartin", None),
            ]
        );
        assert!(detail.posts.is_sorted_by_key(|post| post.at));
        assert_eq!(detail.older_posts, 0);
        assert_eq!(detail.threads.len(), 5);
        assert!(detail
            .threads
            .iter()
            .all(|thread| thread.state == ThreadState::Resolved && thread.comments.len() == 2));
        assert_eq!(detail.threads[0].line, None, "outdated past its line");
        assert_eq!(detail.threads[1].line, Some(423));
    }

    /// Comments and reviews cut short at their newest: posts older than
    /// the later cut go, as the other list's posts from then weren't read,
    /// and count as older.
    #[test]
    fn a_cut_timeline_has_no_holes() {
        let comment = |at: &str| GhComment {
            author: None,
            body: "c".to_owned(),
            created_at: at.to_owned(),
            url: String::new(),
        };
        let review = |at: &str| GhReview {
            author: None,
            body: String::new(),
            state: "APPROVED".to_owned(),
            submitted_at: Some(at.to_owned()),
            url: String::new(),
        };
        let comments = Connection {
            total_count: 10,
            nodes: vec![
                comment("2026-01-02T00:00:00Z"),
                comment("2026-01-04T00:00:00Z"),
            ],
        };
        let reviews = Connection {
            total_count: 2,
            nodes: vec![
                review("2026-01-01T00:00:00Z"),
                review("2026-01-03T00:00:00Z"),
            ],
        };
        let (posts, older) = timeline(comments, reviews);
        assert_eq!(posts.len(), 3);
        assert_eq!(posts[0].verdict, None, "the cut is the oldest comment read");
        assert_eq!(older, 8 + 1);
    }

    /// An action done reads as such; one refused says what gh said.
    #[test]
    fn actions_say_why_they_failed() {
        let printed = |code: &str, error: &str| {
            let mut output = b"motd\n".to_vec();
            output.extend(ACTION_MARKER);
            output.extend(format!("{code}\0{error}\0").as_bytes());
            output
        };
        assert_eq!(parse_action(&printed("0", "")), Ok(()));
        assert_eq!(
            parse_action(&printed(
                "1",
                "\ngh: GraphQL: Could not add comment (addComment)\n"
            )),
            Err("GraphQL: Could not add comment (addComment)".to_owned())
        );
        assert_eq!(
            parse_action(&printed("4", "")),
            Err("gh exited with status 4.".to_owned())
        );
        assert!(parse_action(b"motd only").is_err());
    }

    /// A PR offers the merge methods its repository allows, the viewer's
    /// default first, else in GitHub's order.
    #[test]
    fn merges_offer_what_the_repository_allows() {
        use MergeMethod::{Merge, Rebase, Squash};
        let settings = |allowed: [bool; 3], default: Option<&str>| GhMergeSettings {
            merge_commit_allowed: allowed[0],
            squash_merge_allowed: allowed[1],
            rebase_merge_allowed: allowed[2],
            viewer_default_merge_method: default.map(str::to_owned),
        };
        let cases = [
            ([true; 3], Some("REBASE"), vec![Rebase, Merge, Squash]),
            ([true; 3], None, vec![Merge, Squash, Rebase]),
            ([false, true, false], Some("MERGE"), vec![Squash]),
        ];
        for (allowed, default, expected) in cases {
            assert_eq!(
                settings(allowed, default).methods(),
                expected,
                "{default:?}"
            );
        }
    }

    /// A branch named like a fork's takes the repository's own PR, an open
    /// one over an older closed one; with only forks' PRs, it has none,
    /// unless they fill the lookup, which may hide its own.
    #[test]
    fn a_branch_takes_its_own_open_pr() {
        #[derive(Debug, PartialEq)]
        enum Picked {
            Pr(u32),
            None,
            OnlyForks(usize),
        }
        let pr = |number: u32, state: &str, fork: bool| {
            serde_json::json!({
                "number": number, "title": "t", "url": "u", "state": state, "isDraft": false,
                "isCrossRepository": fork, "updatedAt": "2026-11-15T00:00:00Z",
                "headRefOid": "abc", "commits": { "nodes": [] }
            })
        };
        let picked = |nodes: Value| {
            let found: Vec<GhSummary> = serde_json::from_value(nodes).unwrap();
            match pick(&repo("rho-ui"), found) {
                Found::Pr(pr) => Picked::Pr(pr.id.number),
                Found::None => Picked::None,
                Found::OnlyForks(read) => Picked::OnlyForks(read),
            }
        };
        let forks: Vec<Value> = (1..=5).map(|number| pr(number, "OPEN", true)).collect();
        let cases = [
            (
                serde_json::json!([pr(9, "OPEN", true), pr(7, "CLOSED", false)]),
                Picked::Pr(7),
            ),
            (
                serde_json::json!([pr(8, "MERGED", false), pr(5, "OPEN", false)]),
                Picked::Pr(5),
            ),
            (serde_json::json!([pr(9, "OPEN", true)]), Picked::None),
            (serde_json::json!([]), Picked::None),
            (Value::Array(forks), Picked::OnlyForks(BRANCH_PRS)),
        ];
        for (nodes, expected) in cases {
            assert_eq!(picked(nodes.clone()), expected, "{nodes}");
        }
    }

    /// An answer with no data says why; a refused token or missing sign-in
    /// reads as signed out, which a new check of `gh` would see. Part of
    /// an answer failing other than a repository not found, or an answer
    /// that doesn't read, fails it all rather than reading as no PR.
    #[test]
    fn failures_tell_signed_out_from_the_rest() {
        let cases = [
            (
                "",
                "gh: HTTP 401: Bad credentials (https://api.github.com/graphql)",
                true,
            ),
            (
                "",
                "To get started with GitHub CLI, please run:  gh auth login",
                true,
            ),
            ("", "error connecting to api.github.com", false),
            (
                r#"{"errors":[{"message":"Something went wrong"}]}"#,
                "",
                false,
            ),
            (
                r#"{"data":{"r0":null},"errors":[{"type":"INTERNAL","path":["r0","b0"],"message":"Timeout"}]}"#,
                "",
                false,
            ),
            (
                r#"{"data":{"r0":{"defaultBranchRef":null,"b0":{"nodes":[{"number":"x"}]}}}}"#,
                "",
                false,
            ),
        ];
        let plan = FetchPlan {
            branches: vec![branch("rho-ui", "feature")],
            links: Vec::new(),
            details: Vec::new(),
        };
        for (answer, error, signed_out) in cases {
            match parse(&plan, &printed(answer, error)) {
                Err(QueryError::SignedOut(_)) => assert!(signed_out, "{error}"),
                Err(QueryError::Failed(_)) => assert!(!signed_out, "{error}"),
                Ok(batch) => panic!("{error:?} read as {batch:?}"),
            }
        }
    }
}
