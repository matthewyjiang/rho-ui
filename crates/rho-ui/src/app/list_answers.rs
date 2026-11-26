//! Answers from the sidebar's list: a request open on a session another
//! session started, answered from its row without following the session,
//! as rho's terminal UI answers one from `/jobs`.

use gpui_kit::component::{notification::Notification, WindowExt as _};
use gpui_kit::{Context, Window};
use rho_ui_core::{Answer, InputId};

use super::RhoApp;
use crate::machines::SessionKey;

impl RhoApp {
    /// Sends `answer` to request `request` of session `id` through its
    /// machine's daemon. The row holds its choices until the jobs stop
    /// listing the request, or, should the answer fail, until the sidebar
    /// hears so here.
    pub(super) fn answer_from_list(
        &mut self,
        id: SessionKey,
        request: InputId,
        answer: Answer,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let title = "Couldn't answer the session";
        let Some(daemon) = self.daemon(&id.machine).cloned() else {
            self.not_connected(&id.machine, title, window, cx);
            self.sidebar
                .update(cx, |sidebar, cx| sidebar.answer_failed(&id, request, cx));
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let Err(error) = daemon.answer(&id.id, request, &answer).await else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| {
                window.push_notification(Notification::error(error.to_string()).title(title), cx);
                this.sidebar
                    .update(cx, |sidebar, cx| sidebar.answer_failed(&id, request, cx));
            });
        })
        .detach();
    }
}
