//! The Brief surface: a lead's `brief.md`, which it keeps current in its
//! folder and rereads after compaction, drawn as Markdown, read-only. It reads
//! the file on the lead's machine when its tab opens, and again when the
//! lead's turn ends or it comes back on screen ([`super::RightPanel`] says
//! when), showing what it read until the next read lands, and through one
//! that fails, under why.

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    alert::Alert,
    spinner::Spinner,
    text::{TextView, TextViewState},
    v_flex, ActiveTheme as _, Sizable as _,
};
use gpui_kit::{
    div, AnyElement, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Task,
    Window,
};

use super::message;
use crate::{folders::FileRead, machines::Place, workspaces::Workspaces};

/// The file a lead keeps its brief in, in its folder.
const BRIEF: &str = "brief.md";

/// The largest brief the surface shows, in bytes. Kit's text view lays the
/// whole text out each frame: in a debug build a frame with the user
/// guide's Markdown cut to 64 KiB (840 lines) took 21 ms, 128 KiB 48 ms and
/// 242 KiB 405 ms, against 3 ms for 1 KiB. A brief is a few KiB.
const BRIEF_LIMIT: usize = 64 << 10;

/// The line over the brief: how the lead uses it, and how to change it.
const CAPTION: &str =
    "The lead keeps this current and rereads it after compaction. To change it, tell the lead.";

/// What the surface shows.
enum Shown {
    /// Nothing read yet.
    Reading,
    /// The brief: its text, and that text parsed as Markdown; and if the
    /// last read failed since, why, which shows over the text it read
    /// before.
    Text {
        source: String,
        view: Entity<TextViewState>,
        failed: Option<SharedString>,
    },
    /// The lead hasn't written one.
    Gone,
    /// Over [`BRIEF_LIMIT`].
    TooLarge,
    /// The last read failed with no text to show, and why.
    Failed(SharedString),
}

/// The Brief surface of [`super::RightPanel`], for one lead.
pub(super) struct BriefSurface {
    workspaces: Entity<Workspaces>,
    /// The lead's folder, which holds its brief.
    place: Place,
    shown: Shown,
    /// The read under way; a newer one replaces it.
    load: Task<()>,
}

impl BriefSurface {
    pub(super) fn new(
        workspaces: Entity<Workspaces>,
        place: Place,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let mut this = Self {
            workspaces,
            place,
            shown: Shown::Reading,
            load: Task::ready(()),
        };
        this.reload(cx);
        this
    }

    /// Reads the brief again, keeping what shows until the read lands.
    pub(super) fn reload(&mut self, cx: &mut Context<'_, Self>) {
        let reader = self.workspaces.read(cx).reader(&self.place.machine);
        let folder = self.place.path.clone();
        let executor = cx.background_executor().clone();
        self.load = cx.spawn(async move |this, cx| {
            let read = reader
                .read_file(&folder, BRIEF, BRIEF_LIMIT, &executor)
                .await;
            let _ = this.update(cx, |this, cx| this.read(read, cx));
        });
    }

    fn read(&mut self, read: Result<FileRead, String>, cx: &mut Context<'_, Self>) {
        self.shown = match (read, std::mem::replace(&mut self.shown, Shown::Reading)) {
            (Ok(FileRead::Bytes(bytes)), shown) => {
                let text = String::from_utf8_lossy(&bytes).into_owned();
                match shown {
                    // The same text again leaves the view, and where it
                    // was scrolled, alone.
                    Shown::Text { source, view, .. } if source == text => Shown::Text {
                        source,
                        view,
                        failed: None,
                    },
                    Shown::Text { view, .. } => {
                        view.update(cx, |view, cx| view.set_text(&text, cx));
                        Shown::Text {
                            source: text,
                            view,
                            failed: None,
                        }
                    }
                    Shown::Reading | Shown::Gone | Shown::TooLarge | Shown::Failed(_) => {
                        let view = cx.new(|cx| TextViewState::markdown(&text, cx));
                        Shown::Text {
                            source: text,
                            view,
                            failed: None,
                        }
                    }
                }
            }
            (Ok(FileRead::TooLarge), _) => Shown::TooLarge,
            (Ok(FileRead::Gone), _) => Shown::Gone,
            // What it read before stays, under why it can't read it now.
            (Err(why), Shown::Text { source, view, .. }) => Shown::Text {
                source,
                view,
                failed: Some(why.into()),
            },
            (Err(why), Shown::Reading | Shown::Gone | Shown::TooLarge | Shown::Failed(_)) => {
                Shown::Failed(why.into())
            }
        };
        cx.notify();
    }

    /// The text of the brief it shows, for UI tests.
    #[cfg(test)]
    pub(super) fn text(&self) -> Option<String> {
        match &self.shown {
            Shown::Text { source, .. } => Some(source.clone()),
            Shown::Reading | Shown::Gone | Shown::TooLarge | Shown::Failed(_) => None,
        }
    }
}

/// The brief in `view`, under a line saying how the lead uses it and, if
/// the last read `failed`, why.
fn render_text(
    view: &Entity<TextViewState>,
    failed: Option<&SharedString>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    v_flex()
        .size_full()
        .child(
            div()
                .flex_shrink_0()
                .px_4()
                .py_2()
                .text_xs()
                .text_color(theme.muted_foreground)
                .border_b_1()
                .border_color(theme.border)
                .child(CAPTION),
        )
        .children(failed.map(|why| {
            div()
                .flex_shrink_0()
                .px_4()
                .pt_2()
                .child(Alert::warning("brief-failed", why.clone()).small())
        }))
        .child(
            v_flex()
                .id("brief-text")
                .test_support()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px_4()
                .py_3()
                .child(div().text_sm().child(TextView::new(view).selectable(true))),
        )
        .into_any_element()
}

impl Render for BriefSurface {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        match &self.shown {
            Shown::Reading => message(
                "Reading the brief…",
                Some(Spinner::new().into_any_element()),
            ),
            Shown::Text { view, failed, .. } => render_text(view, failed.as_ref(), cx),
            Shown::Gone => message("The lead hasn't written its brief yet.", None),
            Shown::TooLarge => message(
                format!(
                    "The brief is over {} KiB, too large to show.",
                    BRIEF_LIMIT >> 10
                ),
                None,
            ),
            Shown::Failed(why) => message(why.clone(), None),
        }
    }
}
