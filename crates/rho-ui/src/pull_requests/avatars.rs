//! The pictures of the people who post on PRs, each fetched once from
//! where the forge serves it ([`forge::avatar`](crate::forge::avatar)) and
//! kept until rho-ui quits: 72 px a side, so about 20 kB each once drawn.
//!
//! A picture is known by its address less the query: that holds its size
//! and version, and may hold a token the forge makes anew for each read,
//! which would have the same picture fetched again every time.

use std::{
    collections::{hash_map::Entry, HashMap, VecDeque},
    sync::Arc,
};

use gpui_kit::{Image, SharedString};
use rho_ui_core::{PullRequestDetail, PullRequestPost};

use crate::machines::MachineId;

/// Where one picture is.
enum Picture {
    /// Waiting its turn, or being fetched.
    Fetching,
    Fetched(Arc<Image>),
    /// It didn't load: its person's initials show until Refresh asks
    /// again.
    Failed,
}

/// Everything said on `detail`: its comments and reviews, then its
/// threads' comments.
pub(crate) fn every_post(detail: &PullRequestDetail) -> impl Iterator<Item = &PullRequestPost> {
    let threads = detail.threads.iter().flat_map(|thread| &thread.comments);
    detail.posts.iter().chain(threads)
}

/// A picture's address less its query, which names it.
pub(super) fn name(url: &str) -> &str {
    url.split_once('?').map_or(url, |(name, _)| name)
}

/// People's pictures, by [`name`].
#[derive(Default)]
pub(super) struct Avatars {
    pictures: HashMap<String, Picture>,
    /// Fetches waiting their turn: where each picture is, and the machine
    /// that fetches it. One runs
    /// at a time: on a remote machine each is an ssh session on the shared
    /// connection, which sshd's default `MaxSessions` holds to 10, and the
    /// daemon connection, the Files surface and git need some too.
    queue: VecDeque<(MachineId, SharedString)>,
    fetching: bool,
}

impl Avatars {
    /// The picture at `url`, once fetched.
    pub(super) fn get(&self, url: &str) -> Option<Arc<Image>> {
        match self.pictures.get(name(url))? {
            Picture::Fetched(image) => Some(Arc::clone(image)),
            Picture::Fetching | Picture::Failed => None,
        }
    }

    /// Queues the pictures of those who posted on `detail`, less those
    /// asked for before, to fetch on `machine`, which read it.
    pub(super) fn queue(&mut self, detail: &PullRequestDetail, machine: &MachineId) {
        for url in every_post(detail).filter_map(|post| post.avatar_url.as_ref()) {
            if let Entry::Vacant(entry) = self.pictures.entry(name(url).to_owned()) {
                entry.insert(Picture::Fetching);
                self.queue
                    .push_back((machine.clone(), SharedString::from(url.clone())));
            }
        }
    }

    /// The next picture to fetch and the machine to fetch it on, unless a
    /// fetch runs or none waits. The caller fetches it, and
    /// [finishes](Self::finish).
    pub(super) fn start_next(&mut self) -> Option<(MachineId, SharedString)> {
        if self.fetching {
            return None;
        }
        let next = self.queue.pop_front()?;
        self.fetching = true;
        Some(next)
    }

    /// Takes the picture at `url`, or that it didn't load, as its fetch
    /// ends.
    pub(super) fn finish(&mut self, url: &str, picture: Option<Arc<Image>>) {
        self.fetching = false;
        let picture = picture.map_or(Picture::Failed, Picture::Fetched);
        self.pictures.insert(name(url).to_owned(), picture);
    }

    /// Ends the fetch of the picture at `url` unfetched, with those
    /// waiting for `machine`, which isn't connected: none failed, so the
    /// next read of their PRs queues them again.
    pub(super) fn abandon(&mut self, url: &str, machine: &MachineId) {
        self.fetching = false;
        self.pictures.remove(name(url));
        let (dropped, kept) = std::mem::take(&mut self.queue)
            .into_iter()
            .partition(|(on, _)| on == machine);
        self.queue = kept;
        for (_, url) in dropped {
            self.pictures.remove(name(&url));
        }
    }

    /// Forgets the pictures that didn't load, so the next read of their
    /// PRs asks for them again.
    pub(super) fn forget_failed(&mut self) {
        self.pictures
            .retain(|_, picture| !matches!(picture, Picture::Failed));
    }
}
