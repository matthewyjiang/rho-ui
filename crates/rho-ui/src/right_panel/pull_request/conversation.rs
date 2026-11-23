//! The PR's conversation on the Pull request surface: its comments and
//! reviews in one timeline, its threads on the code, and the box to
//! comment in.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    avatar::Avatar,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{InputEvent, Textarea, TextareaState},
    tooltip::Tooltip,
    v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, AnyElement, AppContext as _, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Task, Window,
};

use rho_ui_core::{
    PullRequestDetail, PullRequestPost, PullRequestThread, PullRequestThreadState,
    PullRequestVerdict,
};

use super::{header::web_url, quiet, row, PullRequestSurface};
use crate::{forge::PrAction, pull_requests, time};

/// Where the comment being written stands.
pub(super) enum Posting {
    Idle,
    /// Posting it; dropping the task stops it.
    Sending {
        _task: Task<()>,
    },
    /// It wasn't posted, and why; the box still holds it.
    Failed(SharedString),
}

/// How tall the comment box grows, in lines, before it scrolls.
const COMMENT_ROWS: usize = 8;

impl PullRequestSurface {
    /// Makes the comment box, once.
    pub(super) fn ensure_comment_box(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.comment_box.is_some() {
            return;
        }
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, COMMENT_ROWS)
                .placeholder("Leave a comment")
        });
        let events =
            cx.subscribe_in(
                &input,
                window,
                |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::PressEnter {
                        secondary: true, ..
                    } => this.send_comment(window, cx),
                    InputEvent::Change => cx.notify(),
                    _ => {}
                },
            );
        self.comment_box = Some((input, events));
    }

    /// Posts what the comment box holds on the PR on screen, and empties
    /// the box once it is posted.
    fn send_comment(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some((input, _)) = &self.comment_box else {
            return;
        };
        let body = input.read(cx).value().trim().to_owned();
        if body.is_empty() || matches!(self.posting, Posting::Sending { .. }) {
            return;
        }
        let Some(id) = self.shown_pr(cx) else {
            return;
        };
        let input = input.clone();
        let posted = self.prs.update(cx, |prs, cx| {
            prs.act(id.clone(), PrAction::Comment(body), cx)
        });
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = posted.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.shown_pr(cx).as_ref() != Some(&id) {
                    // Another session's PR is on screen: this one's box is
                    // gone.
                    return;
                }
                this.posting = match result {
                    Ok(()) => {
                        input.update(cx, |input, cx| input.set_value("", window, cx));
                        Posting::Idle
                    }
                    Err(why) => Posting::Failed(why),
                };
                cx.notify();
            });
        });
        self.posting = Posting::Sending { _task: task };
        cx.notify();
    }

    /// The comment box, with who it posts as, or why it can't, and the
    /// button that posts it.
    fn comment_form(&self, cx: &Context<'_, Self>) -> Option<AnyElement> {
        let (input, _) = self.comment_box.as_ref()?;
        let theme = cx.theme();
        let sending = matches!(self.posting, Posting::Sending { .. });
        let empty = input.read(cx).value().trim().is_empty();
        let poster = self.prs.read(cx).poster(cx);
        let (note, color) = match (&self.posting, &poster) {
            (Posting::Failed(why), _) => (why.clone(), theme.red),
            (_, Err(why)) => (why.clone(), theme.muted_foreground),
            (_, Ok(poster)) => {
                let from = pull_requests::on(&poster.machine);
                let note = format!("As @{} from {from}", poster.login);
                (note.into(), theme.muted_foreground)
            }
        };
        let shortcut = if cfg!(target_os = "macos") {
            "Comment (⌘Enter)"
        } else {
            "Comment (Ctrl+Enter)"
        };
        let form = v_flex()
            .gap_2()
            .pt_2()
            .child(
                div()
                    .id("pull-request-comment-box")
                    .test_support()
                    .child(Textarea::new(input).disabled(sending)),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .id("pull-request-comment-note")
                            .test_support()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(color)
                            .child(note),
                    )
                    .child(
                        Button::new("pull-request-comment")
                            .primary()
                            .small()
                            .label("Comment")
                            .tooltip(shortcut)
                            .loading(sending)
                            .disabled(empty || sending || poster.is_err())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.send_comment(window, cx);
                            })),
                    ),
            );
        Some(form.into_any_element())
    }

    /// The comments and reviews, oldest first.
    pub(super) fn conversation(
        &self,
        detail: &PullRequestDetail,
        now: i64,
        cx: &Context<'_, Self>,
    ) -> AnyElement {
        v_flex()
            .gap_4()
            .when(detail.posts.is_empty() && detail.older_posts == 0, |this| {
                this.child(quiet("No comments yet.", cx))
            })
            .when(detail.older_posts > 0, |this| {
                this.child(quiet(format!("{} older on GitHub", detail.older_posts), cx))
            })
            .children(detail.posts.iter().enumerate().map(|(ix, post)| {
                self.post(format!("pull-request-post-{ix}").into(), post, now, cx)
            }))
            .children(self.comment_form(cx))
            .into_any_element()
    }

    /// Who said it, when, and what a review decided, over what they said.
    fn post(
        &self,
        id: SharedString,
        post: &PullRequestPost,
        now: i64,
        cx: &Context<'_, Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let verdict = post.verdict.and_then(|verdict| match verdict {
            PullRequestVerdict::Approved => Some(("approved", Lucide::CircleCheck, theme.green)),
            PullRequestVerdict::ChangesRequested => {
                Some(("requested changes", Lucide::CircleX, theme.red))
            }
            PullRequestVerdict::Commented => {
                Some(("reviewed", Lucide::Eye, theme.muted_foreground))
            }
            PullRequestVerdict::Dismissed => Some((
                "review dismissed",
                Lucide::CircleSlash,
                theme.muted_foreground,
            )),
            // A verdict a newer rho sends goes unsaid.
            _ => None,
        });
        let url = web_url(&post.url);
        let muted = theme.muted_foreground;
        let foreground = theme.foreground;
        // The author's picture once it is fetched, their initials until
        // then.
        let mut avatar = Avatar::new().name(post.author.clone()).small();
        let picture = post.avatar_url.as_ref();
        if let Some(picture) = picture.and_then(|at| self.prs.read(cx).avatar(at)) {
            avatar = avatar.src(picture);
        }
        v_flex()
            .id(id)
            .test_support()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .child(avatar)
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_medium()
                            .text_color(foreground)
                            .child(post.author.clone()),
                    )
                    .when_some(verdict, |this, (verdict, icon, color)| {
                        this.child(
                            h_flex()
                                .flex_shrink_0()
                                .gap_1()
                                .text_color(color)
                                .child(Icon::new(icon).xsmall())
                                .child(verdict),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("age")
                            .flex_shrink_0()
                            .text_color(muted)
                            .child(time::relative_age(now - post.at))
                            .when_some(url, |this, url| {
                                this.cursor_pointer()
                                    .hover(move |style| style.text_color(foreground))
                                    .tooltip(|window, cx| {
                                        Tooltip::new("Open on GitHub").build(window, cx)
                                    })
                                    .on_click(move |_, _, cx| cx.open_url(&url))
                            }),
                    ),
            )
            // Under the name, past the avatar.
            .when_some(self.text(&post.url), |this, text| {
                this.child(div().pl_8().child(text))
            })
    }

    /// The comment threads on the code, each under the place it is on.
    /// See [`open_threads`] for the heading's count.
    pub(super) fn threads(
        &self,
        detail: &PullRequestDetail,
        now: i64,
        cx: &Context<'_, Self>,
    ) -> AnyElement {
        v_flex()
            .gap_1()
            .when(detail.older_threads > 0, |this| {
                this.child(quiet(
                    format!("{} older on GitHub", detail.older_threads),
                    cx,
                ))
            })
            .children(
                detail
                    .threads
                    .iter()
                    .enumerate()
                    .map(|(ix, thread)| self.thread(ix, thread, now, cx)),
            )
            .into_any_element()
    }

    /// A thread: a row naming its file and line, which opens and closes
    /// it, over its comments. One in a state a newer rho sends draws as
    /// open.
    fn thread(
        &self,
        ix: usize,
        thread: &PullRequestThread,
        now: i64,
        cx: &Context<'_, Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let resolved = thread.state == PullRequestThreadState::Resolved;
        let open = resolved == self.flipped.contains(&thread.id);
        let (dir, file) = match thread.path.rsplit_once('/') {
            Some((dir, file)) => (Some(dir.to_owned()), file.to_owned()),
            None => (None, thread.path.clone()),
        };
        let place = match thread.line {
            Some(line) => format!("{file}:{line}"),
            None => file,
        };
        let state = match thread.state {
            PullRequestThreadState::Outdated => Some("Outdated"),
            PullRequestThreadState::Resolved => Some("Resolved"),
            // Open, or a state a newer rho sends.
            _ => None,
        };
        let count = u32::try_from(thread.comments.len())
            .unwrap_or(u32::MAX)
            .saturating_add(thread.more_comments);
        let id = thread.id.clone();
        let muted = theme.muted_foreground;
        v_flex()
            .child(
                row(format!("pull-request-thread-{ix}"), cx)
                    .text_xs()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.secondary))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.flipped.remove(&id) {
                            this.flipped.insert(id.clone());
                        }
                        cx.notify();
                    }))
                    .child(
                        Icon::new(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .xsmall()
                        .text_color(muted),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(if resolved { muted } else { theme.foreground })
                            .child(place),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(muted)
                            .children(dir),
                    )
                    .children(
                        state.map(|state| div().flex_shrink_0().text_color(muted).child(state)),
                    )
                    // Closed, how many comments it hides.
                    .when(!open, |this| {
                        this.child(
                            h_flex()
                                .flex_shrink_0()
                                .gap_1()
                                .text_color(muted)
                                .child(Icon::new(Lucide::MessageSquare).xsmall())
                                .child(count.to_string()),
                        )
                    }),
            )
            .when(open, |this| {
                this.child(
                    v_flex()
                        .pl_5()
                        .pt_2()
                        .pb_3()
                        .gap_3()
                        .children(thread.comments.iter().enumerate().map(|(jx, post)| {
                            self.post(
                                format!("pull-request-thread-{ix}-{jx}").into(),
                                post,
                                now,
                                cx,
                            )
                        }))
                        .when(thread.more_comments > 0, |this| {
                            this.child(quiet(
                                format!("{} more on GitHub", thread.more_comments),
                                cx,
                            ))
                        }),
                )
            })
    }
}

/// How many of the PR's threads are open: "2 open", or "All resolved".
pub(super) fn open_threads(detail: &PullRequestDetail) -> String {
    let open = detail
        .threads
        .iter()
        .filter(|thread| thread.state != PullRequestThreadState::Resolved)
        .count();
    match open {
        0 => "All resolved".to_owned(),
        open => format!("{open} open"),
    }
}
