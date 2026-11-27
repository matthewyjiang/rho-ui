//! Search across a daemon's saved sessions (`rho/session/search`): the
//! sessions whose conversation holds every word of a query, newest first,
//! each with its best matching messages and where an attach's replay shows
//! them, so a client can open the session on the match.

use std::ops::Range;

use rho_protocol::{methods, ErrorCode, SessionSearchParams};

use super::{Daemon, DaemonError, SessionSummary};

/// What a daemon found for a query: its first page of sessions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchPage {
    /// The matching sessions on the page, newest first.
    pub hits: Vec<SearchHit>,
    /// How many sessions match in all, on the page or not.
    pub total: u64,
    /// The daemon hasn't read every transcript yet, so it may have missed
    /// matches: search again later.
    pub indexing: bool,
}

/// A session that matches, with its best matching messages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHit {
    /// The session, as the session list shows it.
    pub session: SessionSummary,
    /// How many of its messages match.
    pub matching_messages: u64,
    /// Its best one or two matching messages, best first.
    pub excerpts: Vec<Excerpt>,
}

/// Part of a matching message, around its first match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Excerpt {
    /// Who wrote the message; `None` for a kind this client doesn't know.
    pub role: Option<ExcerptRole>,
    /// Up to a few hundred characters of the message.
    pub text: String,
    /// The matched words in `text`, as byte ranges, in order.
    pub highlights: Vec<Range<usize>>,
    /// The entry that shows the message once the session is open.
    pub at: Option<Location>,
    /// When the message was saved, in Unix seconds.
    pub saved_at: Option<u64>,
}

/// Who wrote a matching message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExcerptRole {
    /// The user's prompt.
    User,
    /// The agent's reply.
    Agent,
    /// A tool's output.
    Tool,
    /// A tool's error.
    ToolError,
}

/// A message, by the id the session's events give it: what
/// [`crate::Session::entry_at`] finds the entry of.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Location {
    /// A user, agent or thought message.
    Message(String),
    /// A tool call, which shows a tool's output.
    ToolCall(String),
}

impl Excerpt {
    fn from_wire(excerpt: rho_protocol::SearchExcerpt) -> Self {
        let role = match excerpt.role {
            rho_protocol::ExcerptRole::User => Some(ExcerptRole::User),
            rho_protocol::ExcerptRole::Agent => Some(ExcerptRole::Agent),
            rho_protocol::ExcerptRole::ToolResult => Some(ExcerptRole::Tool),
            rho_protocol::ExcerptRole::ToolError => Some(ExcerptRole::ToolError),
            _ => None,
        };
        let at = match excerpt.at {
            Some(rho_protocol::SearchLocation::Message { id }) => {
                Some(Location::Message(id.as_str().to_owned()))
            }
            Some(rho_protocol::SearchLocation::ToolCall { id }) => {
                Some(Location::ToolCall(id.as_str().to_owned()))
            }
            _ => None,
        };
        let highlights = highlights(&excerpt.text, &excerpt.highlights);
        Self {
            role,
            text: excerpt.text,
            highlights,
            at,
            saved_at: excerpt.saved_at,
        }
    }
}

/// The daemon's highlight ranges that lie in `text` on char boundaries,
/// in order and apart: a range a client can't draw is dropped rather than
/// trusted.
fn highlights(text: &str, ranges: &[Range<u32>]) -> Vec<Range<usize>> {
    let mut kept: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        let (Ok(start), Ok(end)) = (usize::try_from(range.start), usize::try_from(range.end))
        else {
            continue;
        };
        let drawable = start < end && text.get(start..end).is_some();
        let after_last = kept.last().is_none_or(|last| last.end <= start);
        if drawable && after_last {
            kept.push(start..end);
        }
    }
    kept
}

impl Daemon {
    /// The first page of saved sessions whose conversation holds every
    /// word of `query`, newest first.
    ///
    /// # Errors
    ///
    /// Fails if the daemon is from before session search, refuses the
    /// query, or the connection is gone.
    pub async fn search_sessions(&self, query: &str) -> Result<SearchPage, DaemonError> {
        let params = SessionSearchParams::new(query.to_owned(), None, None, None);
        let page = match self.client.request::<methods::SessionSearch>(&params).await {
            Ok(page) => page,
            Err(error) if error.error_code() == Some(ErrorCode::MethodNotFound) => {
                return Err(DaemonError::Refused(
                    "this machine's rho is from before session search".to_owned(),
                ));
            }
            Err(error) => return Err(error.into()),
        };
        let hits = page.sessions.into_iter().map(|hit| SearchHit {
            session: SessionSummary::from_detailed(hit.session),
            matching_messages: hit.matching_messages,
            excerpts: hit.excerpts.into_iter().map(Excerpt::from_wire).collect(),
        });
        Ok(SearchPage {
            hits: hits.collect(),
            total: page.total,
            indexing: page.indexing,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use super::highlights;

    /// Highlights as sent, and the ones kept.
    type Case = (&'static [Range<u32>], Vec<Range<usize>>);

    /// A highlight the client can't draw, past the text, inside a
    /// character, empty or overlapping the one before, is dropped.
    #[test]
    #[expect(
        clippy::single_range_in_vec_init,
        reason = "each case is a list of ranges, some of one"
    )]
    fn keeps_only_drawable_highlights() {
        let text = "naïve tree walk";
        let cases: [Case; 5] = [
            (&[0..6, 7..11], vec![0..6, 7..11]),
            (&[0..3], vec![]),
            (&[13..40], vec![]),
            (&[4..4, 7..11], vec![7..11]),
            (&[7..11, 9..16], vec![7..11]),
        ];
        for (ranges, want) in cases {
            assert_eq!(highlights(text, ranges), want, "ranges {ranges:?}");
        }
    }
}
