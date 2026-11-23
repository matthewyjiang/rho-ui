//! The fake daemon's pull requests: each session's set and each pull
//! request's detail, as a test scripts them, the four
//! `rho/session/pull_request/*` methods, and what clients asked of them.
//!
//! Every session's origin is `acme/widgets` on github.com, and until a test
//! scripts it, its workspace is in no git repository and links nothing.
//! [`FakeDaemon::open_pull_request`] gives a session the pull request
//! `acme/widgets#12`, and [`FakeDaemon::edit_pull_requests`] changes a
//! session's set as a read would. Each change tells the session's
//! followers, as rho's `pull_requests` event does.

use std::collections::{BTreeMap, HashMap};

use rho_protocol::{
    BranchLookup, BranchPullRequest, ErrorCode, LinkedPullRequest, ListedSessionPullRequests,
    PullRequestCheck, PullRequestCheckStatus, PullRequestDetail, PullRequestId,
    PullRequestLinkOrigin, PullRequestMergeMethod, PullRequestMergeable, PullRequestPost,
    PullRequestRead, PullRequestReview, PullRequestRollup, PullRequestState, PullRequestSummary,
    PullRequestTarget, PullRequestThread, PullRequestThreadState, PullRequestTrouble,
    PullRequestVerdict, RpcError, SessionEvent, SessionPullRequestLinkParams,
    SessionPullRequestLinkResponse, SessionPullRequestListParams, SessionPullRequestListResponse,
    SessionPullRequestShowParams, SessionPullRequestShowResponse, SessionPullRequestUnlinkParams,
    SessionPullRequestUnlinkResponse, SessionPullRequests, SESSION_PULL_REQUEST_LINK,
    SESSION_PULL_REQUEST_LIST, SESSION_PULL_REQUEST_SHOW, SESSION_PULL_REQUEST_UNLINK,
};

use super::{FakeDaemon, State, UiSessionId};

/// Whether the fake daemon serves pull requests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PullRequestMethods {
    /// It serves the four `rho/session/pull_request/*` methods.
    #[default]
    Served,
    /// As a daemon from before pull requests: each `list` and `show` is
    /// counted, then all four answer `MethodNotFound`.
    Unserved,
}

/// The description of the pull request [`FakeDaemon::open_pull_request`]
/// opens, with a diagram.
pub const PULL_REQUEST_BODY: &str =
    "Adds **widgets**.\n\n```mermaid\nflowchart LR\n  A --> B\n```\n";

/// The forge every session's origin is on, and the only one the fake reads.
const HOST: &str = "github.com";

/// Every session's origin: where `#12` points.
const ORIGIN: (&str, &str) = ("acme", "widgets");

/// When a link the fake makes was linked, in unix seconds, before it
/// counts its links: 2026-01-02, as session "s1" was saved.
const LINKED_AT: u64 = 1_767_312_000;

/// The fake's pull requests, and what clients asked of them.
#[derive(Debug)]
pub(super) struct FakePullRequests {
    methods: PullRequestMethods,
    /// Each session's set, once a test or a link changed it.
    sets: BTreeMap<String, SessionPullRequests>,
    /// Each pull request's detail, which a `show` answers with.
    details: HashMap<PullRequestId, PullRequestDetail>,
    /// How many `list` requests clients sent, the unserved ones too.
    lists: usize,
    /// Each `show` clients sent, the unserved ones too: its session and
    /// how it read.
    shows: Vec<(String, PullRequestRead)>,
    /// How many links clients made.
    links: u64,
}

impl FakePullRequests {
    pub(super) fn new(methods: PullRequestMethods) -> Self {
        Self {
            methods,
            sets: BTreeMap::new(),
            details: HashMap::new(),
            lists: 0,
            shows: Vec::new(),
            links: 0,
        }
    }

    /// `MethodNotFound` for `method` while the fake plays an older daemon.
    fn served(&self, method: &str) -> Result<(), RpcError> {
        match self.methods {
            PullRequestMethods::Served => Ok(()),
            PullRequestMethods::Unserved => Err(RpcError::new(
                ErrorCode::MethodNotFound,
                format!("this rho daemon does not serve {method}"),
            )),
        }
    }

    /// `session`'s set: by default, a workspace in no git repository that
    /// links nothing.
    fn set(&self, session: &str) -> SessionPullRequests {
        self.sets.get(session).cloned().unwrap_or_else(|| {
            SessionPullRequests::new(
                Some(BranchPullRequest::new(None, BranchLookup::NotGit, None)),
                Vec::new(),
            )
        })
    }

    /// A new link to `id` by a person, read already when the fake has its
    /// detail, and else found to be no pull request.
    fn link(&mut self, id: PullRequestId) -> LinkedPullRequest {
        self.links += 1;
        let url = format!(
            "https://{}/{}/{}/pull/{}",
            id.host, id.owner, id.name, id.number
        );
        let summary = self.details.get(&id).map(|detail| detail.summary.clone());
        let mut link =
            LinkedPullRequest::new(id, url, PullRequestLinkOrigin::User, LINKED_AT + self.links);
        link.trouble = summary.is_none().then(|| PullRequestTrouble::NotFound {
            message: "Could not resolve to a PullRequest.".to_owned(),
        });
        link.summary = summary;
        link
    }
}

/// The pull request `text` names, as rho reads what a person typed: a
/// github.com pull URL, `owner/name#12`, or `#12` in the origin's
/// repository.
fn parse(text: &str) -> Option<PullRequestId> {
    let text = text.trim();
    let (repo, number) = if let Some(path) = text.strip_prefix("https://github.com/") {
        let mut parts = path.split('/');
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some(owner), Some(name), Some("pull"), Some(number)) if parts.next().is_none() => {
                ((owner, name), number)
            }
            _ => return None,
        }
    } else if let Some(number) = text.strip_prefix('#') {
        (ORIGIN, number)
    } else {
        let (repo, number) = text.split_once('#')?;
        (repo.split_once('/')?, number)
    };
    let (owner, name) = repo;
    let number = number.parse::<u32>().ok().filter(|number| *number > 0)?;
    let named = !owner.is_empty() && !name.is_empty() && !name.contains('/');
    named.then(|| PullRequestId::new(HOST, owner, name, number))
}

/// The refusal of text that names no pull request.
fn not_a_pull_request(text: &str) -> RpcError {
    RpcError::new(
        ErrorCode::InvalidParams,
        format!("{text} is not a pull request: give its URL, owner/name#12, or #12"),
    )
}

/// The summary of the branch's pull request, once found.
fn branch_summary(set: &SessionPullRequests) -> Option<&PullRequestSummary> {
    match &set.branch.as_ref()?.lookup {
        BranchLookup::Found { pull_request } => Some(pull_request.as_ref()),
        _ => None,
    }
}

impl State {
    /// Replaces `session`'s set with `set`, telling its followers when it
    /// changed.
    fn replace_pull_requests(&mut self, session: &str, set: SessionPullRequests) {
        if self.pull_requests.set(session) == set {
            return;
        }
        if let Some(fake) = self.sessions.get_mut(session) {
            fake.publish(SessionEvent::PullRequests {
                pull_requests: Box::new(set.clone()),
            });
        }
        self.pull_requests.sets.insert(session.to_owned(), set);
    }

    /// Changes `session`'s set with `edit`, as a read would: the detail of
    /// each pull request whose summary it holds takes that summary.
    fn edit_pull_requests(&mut self, session: &str, edit: impl FnOnce(&mut SessionPullRequests)) {
        assert!(
            self.sessions.contains_key(session),
            "the fake daemon has no session {session}"
        );
        let mut set = self.pull_requests.set(session);
        edit(&mut set);
        let linked = set.linked.iter().filter_map(|link| link.summary.as_ref());
        for summary in branch_summary(&set).into_iter().chain(linked) {
            if let Some(detail) = self.pull_requests.details.get_mut(&summary.id) {
                detail.summary = summary.clone();
            }
        }
        self.replace_pull_requests(session, set);
    }

    /// `rho/session/pull_request/list`: each session asked that the fake
    /// has, in the order asked.
    pub(super) fn list_pull_requests(
        &mut self,
        params: &SessionPullRequestListParams,
    ) -> Result<SessionPullRequestListResponse, RpcError> {
        self.pull_requests.lists += 1;
        self.pull_requests.served(SESSION_PULL_REQUEST_LIST)?;
        let sessions = params
            .sessions
            .iter()
            .filter(|id| self.sessions.contains_key(id.as_str()))
            .map(|id| {
                ListedSessionPullRequests::new(id.clone(), self.pull_requests.set(id.as_str()))
            })
            .collect();
        Ok(SessionPullRequestListResponse::new(sessions))
    }

    /// `rho/session/pull_request/show`: the session's set and its target's
    /// detail, if the fake has one. A linked target is named as `link`
    /// takes it.
    pub(super) fn show_pull_request(
        &mut self,
        params: &SessionPullRequestShowParams,
    ) -> Result<SessionPullRequestShowResponse, RpcError> {
        let session = params.session.as_str();
        self.pull_requests
            .shows
            .push((session.to_owned(), params.read));
        self.pull_requests.served(SESSION_PULL_REQUEST_SHOW)?;
        self.session(&params.session)?;
        let set = self.pull_requests.set(session);
        let id = match &params.target {
            PullRequestTarget::Branch => branch_summary(&set).map(|summary| summary.id.clone()),
            PullRequestTarget::Linked { pull_request } => {
                let id = parse(pull_request).ok_or_else(|| not_a_pull_request(pull_request))?;
                if !set.linked.iter().any(|link| link.id == id) {
                    return Err(RpcError::new(
                        ErrorCode::InvalidParams,
                        format!("{id} is not linked to the session"),
                    ));
                }
                Some(id)
            }
            _ => {
                return Err(RpcError::new(
                    ErrorCode::InvalidParams,
                    "the fake daemon shows no such target",
                ))
            }
        };
        let detail = id
            .and_then(|id| self.pull_requests.details.get(&id).cloned())
            .map(Box::new);
        Ok(SessionPullRequestShowResponse::new(set, detail))
    }

    /// `rho/session/pull_request/link`: links what the text names, once.
    pub(super) fn link_pull_request(
        &mut self,
        params: &SessionPullRequestLinkParams,
    ) -> Result<SessionPullRequestLinkResponse, RpcError> {
        self.pull_requests.served(SESSION_PULL_REQUEST_LINK)?;
        self.session(&params.session)?;
        let id =
            parse(&params.pull_request).ok_or_else(|| not_a_pull_request(&params.pull_request))?;
        let session = params.session.as_str();
        let mut set = self.pull_requests.set(session);
        if !set.linked.iter().any(|link| link.id == id) {
            set.linked.push(self.pull_requests.link(id.clone()));
        }
        self.replace_pull_requests(session, set.clone());
        Ok(SessionPullRequestLinkResponse::new(id, set))
    }

    /// `rho/session/pull_request/unlink`: unlinks what the text names, if
    /// linked.
    pub(super) fn unlink_pull_request(
        &mut self,
        params: &SessionPullRequestUnlinkParams,
    ) -> Result<SessionPullRequestUnlinkResponse, RpcError> {
        self.pull_requests.served(SESSION_PULL_REQUEST_UNLINK)?;
        self.session(&params.session)?;
        let id =
            parse(&params.pull_request).ok_or_else(|| not_a_pull_request(&params.pull_request))?;
        let session = params.session.as_str();
        let mut set = self.pull_requests.set(session);
        set.linked.retain(|link| link.id != id);
        self.replace_pull_requests(session, set.clone());
        Ok(SessionPullRequestUnlinkResponse::new(set))
    }
}

impl FakeDaemon {
    /// Gives `session`'s branch `feature` the pull request `acme/widgets#12`,
    /// "Add widgets", open, its checks failing, and has its detail: a
    /// failed check and a passed one, [`PULL_REQUEST_BODY`], a comment and
    /// an approval, an open thread and a resolved one, and squash merges
    /// first of two methods. Its people's pictures are at `pictures`, an
    /// address their logins follow. Tells the session's followers.
    ///
    /// # Panics
    ///
    /// If the daemon has no session `session`: the test has a bug.
    pub fn open_pull_request(&self, session: &UiSessionId, pictures: &str) {
        let detail = widgets(pictures);
        let summary = detail.summary.clone();
        let mut state = self.service.lock();
        state
            .pull_requests
            .details
            .insert(summary.id.clone(), detail);
        state.edit_pull_requests(session.as_str(), |set| {
            set.branch = Some(BranchPullRequest::new(
                Some("feature".to_owned()),
                BranchLookup::Found {
                    pull_request: Box::new(summary),
                },
                None,
            ));
        });
    }

    /// Changes `session`'s pull requests with `edit`, as the daemon's read
    /// of GitHub, or the agent's link, would: a summary it changes is the
    /// detail's too. Tells the session's followers when they changed.
    ///
    /// # Panics
    ///
    /// If the daemon has no session `session`: the test has a bug.
    pub fn edit_pull_requests(
        &self,
        session: &UiSessionId,
        edit: impl FnOnce(&mut SessionPullRequests),
    ) {
        self.service
            .lock()
            .edit_pull_requests(session.as_str(), edit);
    }

    /// `session`'s pull requests now.
    pub fn pull_requests(&self, session: &UiSessionId) -> SessionPullRequests {
        self.service.lock().pull_requests.set(session.as_str())
    }

    /// How many `rho/session/pull_request/list` requests clients sent,
    /// those answered `MethodNotFound` too.
    pub fn pull_request_lists(&self) -> usize {
        self.service.lock().pull_requests.lists
    }

    /// How each `rho/session/pull_request/show` for `session` read, oldest
    /// first, those answered `MethodNotFound` too.
    pub fn pull_request_shows(&self, session: &UiSessionId) -> Vec<PullRequestRead> {
        self.service
            .lock()
            .pull_requests
            .shows
            .iter()
            .filter(|(shown, _)| shown == session.as_str())
            .map(|&(_, read)| read)
            .collect()
    }
}

/// The detail of `acme/widgets#12`, with its people's pictures at
/// `pictures`.
fn widgets(pictures: &str) -> PullRequestDetail {
    let (owner, name) = ORIGIN;
    let url = format!("https://{HOST}/{owner}/{name}/pull/12");
    let summary = PullRequestSummary::new(
        PullRequestId::new(HOST, owner, name, 12),
        "Add widgets",
        url.clone(),
        PullRequestState::Open,
        PullRequestRollup::Failing,
        "abc",
        "2026-11-15T00:00:00Z",
    );
    let post = |login: &str, at: i64, body: &str, anchor: &str| {
        let mut post = PullRequestPost::new(anchor, login, at, body, format!("{url}#{anchor}"));
        post.avatar_url = Some(format!("{pictures}/{login}.png"));
        post
    };
    let check =
        |name: &str, status| PullRequestCheck::new(name, Some("CI".to_owned()), status, None, None);
    let thread = |id: &str, state, path: &str, line: u32, body: &str| {
        let comment = post("octocat", 1_794_697_200, body, id);
        PullRequestThread::new(id, path, Some(line), state, vec![comment], 0)
    };
    let mut approval = post("monalisa", 1_794_699_000, "Ship it.", "r1");
    approval.verdict = Some(PullRequestVerdict::Approved);

    // Read at 2026-11-15T00:00:00Z, when it was last updated.
    let mut detail = PullRequestDetail::new(summary, 1_794_700_800);
    "octocat".clone_into(&mut detail.author);
    "feature".clone_into(&mut detail.head);
    "main".clone_into(&mut detail.base);
    PULL_REQUEST_BODY.clone_into(&mut detail.body);
    detail.additions = 10;
    detail.deletions = 2;
    detail.changed_files = 3;
    detail.review = PullRequestReview::Approved;
    detail.mergeable = PullRequestMergeable::Clean;
    detail.merge_methods = vec![
        PullRequestMergeMethod::Squash,
        PullRequestMergeMethod::Merge,
    ];
    // Those that need a look first, as rho sends them.
    detail.checks = vec![
        check("macos", PullRequestCheckStatus::Failed),
        check("linux", PullRequestCheckStatus::Passed),
    ];
    detail.checks_total = 2;
    detail.posts = vec![
        post(
            "hubot",
            1_794_693_600,
            "Looks good, one nit on the error path.",
            "c1",
        ),
        approval,
    ];
    detail.threads = vec![
        thread(
            "t1",
            PullRequestThreadState::Open,
            "src/lib.rs",
            7,
            "Should this be `pub(crate)`?",
        ),
        thread(
            "t2",
            PullRequestThreadState::Resolved,
            "src/widgets/render.rs",
            42,
            "Typo.",
        ),
    ];
    detail.viewer = Some("octocat".to_owned());
    detail
}
