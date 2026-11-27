//! Search across sessions (Cmd/Ctrl+Shift+F): a palette that asks every
//! connected machine's daemon for the saved sessions whose conversation
//! holds the query (`rho/session/search`), lists them newest first with
//! the text that matched, and opens the one picked with the find bar on
//! that match ([`crate::session_view::SessionView::find_at`]).
//!
//! Each machine answers apart, so a slow one holds back only its own rows;
//! one that can't search says why under the list.

use std::{ops::Range, path::PathBuf, time::Duration};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    command::{Command, CommandItem, CommandState},
    dialog::DialogEntrance,
    h_flex, v_flex, ActiveTheme as _, Icon, IndexPath, Sizable as _, WindowExt as _,
};
use gpui_kit::{
    div, App, AppContext as _, Context, Entity, EventEmitter, FontWeight, HighlightStyle,
    IntoElement, ParentElement as _, Render, SharedString, Styled as _, StyledText, Task, Window,
};
use rho_ui_core::{Daemon, ExcerptRole, Location, SearchHit, SearchPage};

use crate::{
    leads::Kind,
    machines::{MachineId, Machines, SessionKey},
    session_list::UNTITLED,
    time,
};

/// How long typing pauses before a query goes out: about the gap between
/// keys at 80 words a minute (400 characters, 150 ms each), so a fast
/// typist's word asks once, not once a letter.
const SETTLE: Duration = Duration::from_millis(150);

/// How far into a snippet its first match may start, in characters: a
/// quarter of the about 100 that a row's snippet line holds (measured in
/// the `search-hits` UI shot), so the match and what follows it show.
const SNIPPET_LEAD: usize = 24;

/// The palette's width, in rems: Kit's dialog is 28 (448 px), which holds
/// about 60 characters of snippet, too few to read a match in context; 40
/// holds about 100, and fits the narrowest window rho-ui opens.
const WIDTH_REMS: f32 = 40.;

/// A session picked in the palette, to open on its match.
#[derive(Clone, Debug)]
pub(crate) struct OpenHit {
    pub(crate) key: SessionKey,
    pub(crate) cwd: PathBuf,
    pub(crate) title: Option<SharedString>,
    pub(crate) kind: Kind,
    /// What the find bar opens with.
    pub(crate) query: SharedString,
    /// The message the find bar's current match is in, or after.
    pub(crate) at: Option<Location>,
}

/// A matching session, and the index of its machine.
type Row = (usize, SearchHit);

/// What one machine answered for the query on screen.
enum Found {
    Waiting,
    Page(SearchPage),
    Failed(String),
}

/// A machine's part of the search.
struct Machine {
    id: MachineId,
    daemon: Daemon,
    found: Found,
}

/// The palette's view.
pub(crate) struct SessionSearch {
    state: Entity<CommandState>,
    machines: Vec<Machine>,
    labels: Entity<Machines>,
    /// The query the results are for.
    query: SharedString,
    /// Each matching session on screen, newest first, by the index of its
    /// machine: the rows of Kit's model, which its selection indexes.
    rows: Vec<Row>,
    /// The rows an answer brought, for the next frame to show.
    fresh: Option<Vec<Row>>,
    /// The searches under way, one per machine; dropping them stops them.
    searches: Vec<Task<()>>,
}

impl EventEmitter<OpenHit> for SessionSearch {}

/// Opens the palette over `window`, searching `machines` for `query`, its
/// field taking the keyboard; `None` when another dialog is open. Escape
/// with an empty field, or a click outside, closes it. Its view emits
/// [`OpenHit`] for the session picked.
pub(crate) fn open(
    machines: Vec<(MachineId, Daemon)>,
    labels: Entity<Machines>,
    query: &str,
    window: &mut Window,
    cx: &mut App,
) -> Option<Entity<SessionSearch>> {
    if window.has_active_dialog(cx) {
        return None;
    }
    let machines = machines
        .into_iter()
        .map(|(id, daemon)| Machine {
            id,
            daemon,
            found: Found::Waiting,
        })
        .collect();
    let view = cx.new(|cx| SessionSearch {
        state: cx.new(|cx| CommandState::new(window, cx)),
        machines,
        labels,
        query: SharedString::default(),
        rows: Vec::new(),
        fresh: None,
        searches: Vec::new(),
    });
    view.update(cx, |this, cx| {
        // Before Kit's callbacks are in, so this searches once.
        this.state.update(cx, |state, cx| {
            state.set_query(query.to_owned(), window, cx);
        });
        this.search(query, window, cx);
    });
    let first = std::cell::Cell::new(true);
    let palette = view.clone();
    window.open_dialog(cx, move |dialog, window, cx| {
        if first.replace(false) {
            let state = view.read(cx).state.clone();
            state.update(cx, |state, cx| state.focus(window, cx));
        }
        let view = view.clone();
        dialog
            .entrance(DialogEntrance::Fade)
            .w(window.rem_size() * WIDTH_REMS)
            .p_0()
            .content(move |content, _, _| content.child(view.clone()))
    });
    Some(palette)
}

impl SessionSearch {
    /// Asks every machine for `query`, once typing settles; a blank query
    /// clears the list.
    fn search(&mut self, query: &str, window: &mut Window, cx: &mut Context<'_, Self>) {
        let query = query.trim();
        if query == self.query.as_ref() {
            return;
        }
        self.query = SharedString::from(query.to_owned());
        self.fresh = Some(Vec::new());
        for machine in &mut self.machines {
            machine.found = Found::Waiting;
        }
        if query.is_empty() {
            self.searches.clear();
            self.set_loading(window, cx);
            cx.notify();
            return;
        }
        self.searches = self
            .machines
            .iter()
            .enumerate()
            .map(|(ix, machine)| {
                let (daemon, query) = (machine.daemon.clone(), self.query.clone());
                cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(SETTLE).await;
                    let found = daemon.search_sessions(&query).await;
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.found(ix, &query, found, window, cx);
                    });
                })
            })
            .collect();
        self.set_loading(window, cx);
        cx.notify();
    }

    /// Machine `ix` answered `query`: its rows join the list, unless the
    /// query changed meanwhile.
    fn found(
        &mut self,
        ix: usize,
        query: &str,
        found: Result<SearchPage, rho_ui_core::DaemonError>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if query != self.query.as_ref() {
            return;
        }
        self.machines[ix].found = match found {
            Ok(page) => Found::Page(page),
            Err(error) => Found::Failed(error.to_string()),
        };
        let mut rows: Vec<Row> = Vec::new();
        for (machine, searched) in self.machines.iter().enumerate() {
            if let Found::Page(page) = &searched.found {
                rows.extend(page.hits.iter().map(|hit| (machine, hit.clone())));
            }
        }
        // Newest first across machines, as each machine pages its own.
        rows.sort_by_key(|(_, hit)| std::cmp::Reverse(hit.session.updated_at()));
        self.fresh = Some(rows);
        self.set_loading(window, cx);
        cx.notify();
    }

    fn set_loading(&self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let loading = !self.query.is_empty()
            && self
                .machines
                .iter()
                .any(|machine| matches!(machine.found, Found::Waiting));
        self.state
            .update(cx, |state, cx| state.set_loading(loading, window, cx));
    }

    /// Puts `rows` on screen. Kit keeps its highlight on a position, so as a
    /// later machine's hits sort in above the highlighted session, the
    /// highlight moves with that session, and Enter opens what it showed.
    fn show(&mut self, rows: Vec<Row>, window: &mut Window, cx: &App) {
        let selected = self.state.read(cx).selected_index().map(|index| index.row);
        let moved = selected.and_then(|was| {
            let (machine, hit) = self.rows.get(was)?;
            let now = rows.iter().position(|(other, found)| {
                other == machine && found.session.id() == hit.session.id()
            })?;
            (now != was).then_some(now)
        });
        self.rows = rows;
        if let Some(row) = moved {
            let state = self.state.clone();
            // Raw GPUI: once this frame has put the rows in Kit's model,
            // which only a render of the palette does.
            window.on_next_frame(move |window, cx| {
                state.update(cx, |state, cx| {
                    state.set_selected_index(Some(IndexPath::new(row)), window, cx);
                });
            });
        }
    }

    /// Row `row`, picked: the session to open and where.
    fn open_row(&self, row: usize, cx: &mut Context<'_, Self>) {
        let Some((machine, hit)) = self.rows.get(row) else {
            return;
        };
        let id = self.machines[*machine].id.clone();
        let session = &hit.session;
        cx.emit(OpenHit {
            key: SessionKey::new(id, session.id().clone()),
            cwd: session.cwd().to_owned(),
            title: session.title().map(|title| title.to_owned().into()),
            kind: session.agent().map_or(Kind::Session, Kind::of),
            query: self.query.clone(),
            at: hit.excerpts.first().and_then(|excerpt| excerpt.at.clone()),
        });
    }

    /// One line under the list for each machine that couldn't search, is
    /// still reading its transcripts, or found more than it listed.
    fn notes(&self, cx: &App) -> Vec<SharedString> {
        let labels = self.labels.read(cx);
        self.machines
            .iter()
            .filter_map(|machine| {
                let name = labels.label(&machine.id);
                let note = match &machine.found {
                    Found::Failed(error) => format!("{name}: {error}"),
                    Found::Page(page) if page.indexing => {
                        format!("{name} is still reading its sessions; more may match soon")
                    }
                    Found::Page(page)
                        if page.total > u64::try_from(page.hits.len()).unwrap_or(u64::MAX) =>
                    {
                        format!(
                            "{name}: the newest {} of {} matching sessions",
                            page.hits.len(),
                            page.total
                        )
                    }
                    Found::Waiting | Found::Page(_) => return None,
                };
                Some(note.into())
            })
            .collect()
    }

    /// Row `row`'s content.
    fn item(&self, row: usize, cx: &App) -> CommandItem {
        let (machine, hit) = &self.rows[row];
        let id = &self.machines[*machine].id;
        let title: SharedString = hit
            .session
            .title()
            .map_or_else(|| UNTITLED.into(), |title| title.to_owned().into());
        let excerpt = hit.excerpts.first();
        let saved = excerpt
            .and_then(|excerpt| excerpt.saved_at)
            .unwrap_or_else(|| hit.session.updated_at());
        let age = time::relative_age(time::now_unix() - i64::try_from(saved).unwrap_or(i64::MAX));
        let project = hit
            .session
            .cwd()
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        let meta: SharedString = project
            .into_iter()
            .chain(self.labels.read(cx).tag(id).map(String::from))
            .chain([age])
            .collect::<Vec<_>>()
            .join(" · ")
            .into();
        // The messages that match besides the one shown.
        let more = hit.matching_messages.saturating_sub(1);
        let more = (more > 0).then(|| SharedString::from(format!("+{more}")));
        let (snippet, marks) = excerpt
            .map(|excerpt| snippet(&excerpt.text, &excerpt.highlights))
            .unwrap_or_default();
        let icon = match excerpt.and_then(|excerpt| excerpt.role) {
            Some(ExcerptRole::User) => Lucide::User,
            Some(ExcerptRole::Agent) => Lucide::Bot,
            // A kind of message newer than this app.
            None => Lucide::CircleQuestionMark,
            Some(ExcerptRole::Tool) => Lucide::Wrench,
            Some(ExcerptRole::ToolError) => Lucide::CircleAlert,
        };
        let label = title.clone();
        CommandItem::new().label(label).child(move |_, cx| {
            let theme = cx.theme();
            let mark = HighlightStyle {
                color: Some(theme.foreground),
                font_weight: Some(FontWeight::SEMIBOLD),
                ..HighlightStyle::default()
            };
            let marks: Vec<(Range<usize>, HighlightStyle)> =
                marks.iter().map(|range| (range.clone(), mark)).collect();
            h_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .items_start()
                .child(Icon::new(icon).small().text_color(theme.muted_foreground))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(
                            h_flex()
                                .min_w_0()
                                .gap_2()
                                .child(div().flex_1().min_w_0().truncate().child(title.clone()))
                                .child(
                                    div()
                                        .flex_none()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(meta.clone()),
                                ),
                        )
                        .child(
                            h_flex()
                                .min_w_0()
                                .gap_2()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(div().flex_1().min_w_0().truncate().child(
                                    // Raw GPUI: Kit's `Label` marks one substring, not ranges.
                                    StyledText::new(snippet.clone()).with_highlights(marks),
                                ))
                                .children(more.clone().map(|more| div().flex_none().child(more))),
                        ),
                )
        })
    }
}

/// `text` on one line, its whitespace runs as one space, cut so its first
/// match starts within [`SNIPPET_LEAD`] characters, with `highlights`
/// moved to match. A cut start reads "…".
fn snippet(text: &str, highlights: &[Range<usize>]) -> (SharedString, Vec<Range<usize>>) {
    // Where each byte of `text` lands in the line, for its marks.
    let mut line = String::with_capacity(text.len());
    let mut moved = vec![0; text.len() + 1];
    let mut space = false;
    for (ix, ch) in text.char_indices() {
        moved[ix] = line.len();
        if !ch.is_whitespace() {
            line.push(ch);
        } else if !space && !line.is_empty() {
            line.push(' ');
        }
        space = ch.is_whitespace();
    }
    moved[text.len()] = line.len();
    let mut marks: Vec<Range<usize>> = highlights
        .iter()
        .map(|range| moved[range.start]..moved[range.end])
        .filter(|range| !range.is_empty())
        .collect();
    let first = marks.first().map_or(0, |range| range.start);
    let lead = line[..first].chars().count();
    if lead <= SNIPPET_LEAD {
        return (line.into(), marks);
    }
    // Cut at a word's start, when one lies close enough before the match.
    let skip = lead - SNIPPET_LEAD;
    let mut cut = line.char_indices().nth(skip).map_or(first, |(ix, _)| ix);
    if let Some(space) = line[cut..first].find(' ') {
        cut += space + 1;
    }
    let ellipsis = "…";
    let shift = |ix: usize| ix - cut + ellipsis.len();
    for range in &mut marks {
        *range = shift(range.start)..shift(range.end);
    }
    (format!("{ellipsis}{}", &line[cut..]).into(), marks)
}

impl Render for SessionSearch {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        if let Some(rows) = self.fresh.take() {
            self.show(rows, window, cx);
        }
        let items: Vec<CommandItem> = (0..self.rows.len()).map(|row| self.item(row, cx)).collect();
        let notes = self.notes(cx);
        let typed = !self.query.is_empty();
        let query = self.query.clone();
        // Weak: Kit's state keeps these callbacks, and this view keeps it.
        let (on_query, on_confirm) = (cx.weak_entity(), cx.weak_entity());
        Command::new(&self.state)
            .filterable(false)
            .bordered(false)
            .placeholder("Search all sessions")
            .max_h(gpui_kit::rems(28.))
            .items(items)
            .empty(move |_, _, cx| {
                let text = if typed {
                    format!("No saved session mentions “{query}”")
                } else {
                    "Find the session where something was said or done".to_owned()
                };
                div()
                    .p_4()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(text)
            })
            .footer(move |_, _, cx| {
                v_flex()
                    .px_3()
                    .pb_2()
                    .gap_0p5()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .children(notes.iter().cloned())
            })
            .on_query(move |query, window, cx| {
                let _ = on_query.update(cx, |this, cx| this.search(query, window, cx));
            })
            .on_confirm(move |index: IndexPath, _, cx| {
                let _ = on_confirm.update(cx, |this, cx| this.open_row(index.row, cx));
            })
    }
}

#[cfg(test)]
mod tests {
    use super::snippet;

    /// A snippet keeps its marks on the words they marked: across
    /// collapsed whitespace, and after a cut that brings a late match into
    /// view.
    #[test]
    #[expect(
        clippy::single_range_in_vec_init,
        reason = "each case is a list of ranges, of one"
    )]
    fn snippet_lines_up_marks_and_brings_the_match_forward() {
        let cases: [(&str, Vec<std::ops::Range<usize>>, &str, &str); 4] = [
            ("walk the\n\n  tree", vec![12..16], "walk the tree", "tree"),
            ("walk  the tree", vec![0..4], "walk the tree", "walk"),
            ("naïve walk", vec![7..11], "naïve walk", "walk"),
            (
                "a long run of words that comes well before the needle here",
                vec![47..53],
                "…comes well before the needle here",
                "needle",
            ),
        ];
        for (text, highlights, want, marked) in cases {
            let (line, marks) = snippet(text, &highlights);
            assert_eq!(line.as_ref(), want, "text {text:?}");
            assert_eq!(&line[marks[0].clone()], marked, "text {text:?}");
        }
    }
}
