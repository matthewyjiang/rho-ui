//! The pictures of the people who post on PRs, each fetched once from
//! where the forge serves it ([`forge::avatar`](crate::forge::avatar)) and
//! kept until rho-ui quits: 72 px a side, so about 20 kB each once drawn.

use std::{
    collections::{hash_map::Entry, HashMap},
    sync::Arc,
};

use gpui_kit::{Image, SharedString};

use super::model::PrDetail;

/// A person's picture.
enum Avatar {
    Fetching,
    Fetched(Arc<Image>),
    /// It didn't load: their initials show until Refresh asks again.
    Failed,
}

/// People's pictures, by where the forge serves each.
#[derive(Default)]
pub(super) struct Avatars(HashMap<SharedString, Avatar>);

impl Avatars {
    /// The picture at `url`, once fetched.
    pub(super) fn get(&self, url: &str) -> Option<Arc<Image>> {
        match self.0.get(url)? {
            Avatar::Fetched(image) => Some(Arc::clone(image)),
            Avatar::Fetching | Avatar::Failed => None,
        }
    }

    /// Where the pictures of those who posted on `detail` are, less those
    /// asked for before: the caller fetches each, and [sets](Self::set) it.
    pub(super) fn claim(&mut self, detail: &PrDetail) -> Vec<SharedString> {
        let threads = detail.threads.iter().flat_map(|thread| &thread.comments);
        let mut claimed = Vec::new();
        for url in detail
            .posts
            .iter()
            .chain(threads)
            .filter_map(|post| post.avatar.as_ref())
        {
            if let Entry::Vacant(entry) = self.0.entry(url.clone()) {
                entry.insert(Avatar::Fetching);
                claimed.push(url.clone());
            }
        }
        claimed
    }

    /// Takes the picture at `url`, or that it didn't load.
    pub(super) fn set(&mut self, url: SharedString, picture: Option<Arc<Image>>) {
        let avatar = picture.map_or(Avatar::Failed, Avatar::Fetched);
        self.0.insert(url, avatar);
    }

    /// Forgets the pictures that didn't load, so the next read of their
    /// PRs asks for them again.
    pub(super) fn forget_failed(&mut self) {
        self.0.retain(|_, avatar| !matches!(avatar, Avatar::Failed));
    }
}
