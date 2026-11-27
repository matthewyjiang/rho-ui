//! The fake daemon's `rho/session/search`: the saved sessions, newest
//! first, with a user or agent message that holds every word of the query,
//! ignoring case. A word matches anywhere in a message, which covers rho's
//! prefix match on the last word, though not its stemming. Each hit has
//! its first two matching messages, cut around their first match, located
//! by the message ids their replay gives them. One page; no cursor.

use std::collections::BTreeMap;

use regex::{Regex, RegexBuilder};
use rho_protocol::{
    ContentBlock, ErrorCode, ExcerptRole, MessageId, RpcError, SearchExcerpt, SearchLocation,
    SessionEvent, SessionSearchHit, SessionSearchParams, SessionSearchResponse,
};

use super::{service::detailed, FakeSession, State};

/// Sessions on a page, as rho's when the request doesn't say: rho-ui
/// doesn't.
const PAGE: usize = 20;
/// How much of a message an excerpt holds, and of that, how much comes
/// before its first match, in bytes: about what rho's hold, in chars.
const EXCERPT: usize = 320;
const LEAD_IN: usize = 80;

/// A message of a saved transcript, as search sees it.
struct Message {
    role: ExcerptRole,
    id: MessageId,
    text: String,
    saved_at: Option<u64>,
}

impl State {
    pub(super) fn search(
        &self,
        params: &SessionSearchParams,
    ) -> Result<SessionSearchResponse, RpcError> {
        let words: Vec<Regex> = params
            .query
            .split_whitespace()
            .map(|word| {
                RegexBuilder::new(&regex::escape(word))
                    .case_insensitive(true)
                    .build()
                    .expect("an escaped word compiles")
            })
            .collect();
        if words.is_empty() {
            return Err(RpcError::new(
                ErrorCode::InvalidParams,
                "the search query is blank",
            ));
        }
        let hits: Vec<SessionSearchHit> = self
            .saved()
            .into_iter()
            .filter_map(|(id, session)| hit(id, session, &words))
            .collect();
        let total = u64::try_from(hits.len()).expect("a fake's sessions fit in u64");
        let page = hits.into_iter().take(PAGE).collect();
        Ok(SessionSearchResponse::new(page, None, total, false))
    }
}

/// Session `id` as a hit for `words`, if a message of it holds them all.
fn hit(id: &str, session: &FakeSession, words: &[Regex]) -> Option<SessionSearchHit> {
    let matching: Vec<Message> = messages(session)
        .into_iter()
        .filter(|message| words.iter().all(|word| word.is_match(&message.text)))
        .collect();
    if matching.is_empty() {
        return None;
    }
    let count = u64::try_from(matching.len()).expect("a fake's messages fit in u64");
    let excerpts = matching
        .into_iter()
        .take(2)
        .map(|message| excerpt(message, words))
        .collect();
    Some(SessionSearchHit::new(
        detailed(id, session),
        count,
        excerpts,
    ))
}

/// The user and agent messages of `session`'s saved transcript, in order,
/// an agent message's chunks joined.
fn messages(session: &FakeSession) -> Vec<Message> {
    let mut messages: Vec<Message> = Vec::new();
    let mut agent: BTreeMap<MessageId, usize> = BTreeMap::new();
    for sent in session.transcript() {
        let saved_at = sent.at.map(|at| at / 1000);
        match sent.event {
            SessionEvent::UserMessage {
                message, content, ..
            } => {
                let text = content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                messages.push(Message {
                    role: ExcerptRole::User,
                    id: message,
                    text,
                    saved_at,
                });
            }
            SessionEvent::AgentMessageChunk { message, text } => {
                if let Some(&ix) = agent.get(&message) {
                    messages[ix].text.push_str(&text);
                } else {
                    agent.insert(message.clone(), messages.len());
                    messages.push(Message {
                        role: ExcerptRole::Agent,
                        id: message,
                        text,
                        saved_at,
                    });
                }
            }
            _ => {}
        }
    }
    messages
}

/// Up to [`EXCERPT`] bytes of `message` from a little before its first
/// match, with every word's matches in them highlighted.
fn excerpt(message: Message, words: &[Regex]) -> SearchExcerpt {
    let text = &message.text;
    let first = words
        .iter()
        .filter_map(|word| word.find(text).map(|found| found.start()))
        .min()
        .unwrap_or(0);
    let start = floor_char(text, first.saturating_sub(LEAD_IN));
    let end = floor_char(text, (start + EXCERPT).min(text.len()));
    let cut = &text[start..end];
    let mut found: Vec<(usize, usize)> = words
        .iter()
        .flat_map(|word| {
            word.find_iter(cut)
                .map(|found| (found.start(), found.end()))
        })
        .collect();
    found.sort_unstable();
    let mut highlights: Vec<std::ops::Range<u32>> = Vec::new();
    for (start, end) in found {
        let (start, end) = (to_u32(start), to_u32(end));
        match highlights.last_mut() {
            Some(last) if start <= last.end => last.end = last.end.max(end),
            _ => highlights.push(start..end),
        }
    }
    SearchExcerpt::new(
        message.role,
        cut.to_owned(),
        highlights,
        Some(SearchLocation::Message { id: message.id }),
        message.saved_at,
    )
}

/// The char boundary at or before `ix` in `text`.
fn floor_char(text: &str, mut ix: usize) -> usize {
    while !text.is_char_boundary(ix) {
        ix -= 1;
    }
    ix
}

fn to_u32(ix: usize) -> u32 {
    u32::try_from(ix).expect("an excerpt is far shorter than 4 GiB")
}
