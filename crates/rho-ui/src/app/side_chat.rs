//! Side chats, from the app's side: forking a session for one, following
//! the fork in a view of its own in the right panel, and deleting the fork
//! once its tab closed and it wound down (see [`SessionView::discard`]).

use gpui_kit::component::{notification::Notification, WindowExt as _};
use gpui_kit::{AppContext as _, Context, Entity, Window};
use rho_ui_core::{Daemon, Prompt, Session};

use super::RhoApp;
use crate::{
    background_log::BackgroundLog, machines::SessionKey, right_panel::PanelLayout,
    session_view::SessionView,
};

impl RhoApp {
    /// Starts a side chat of `parent`: forks its session, then follows the
    /// fork in a view of its own, in a tab of the right panel, which sends
    /// `prompt` first, if given, once the fork's attach answers. A side
    /// chat that doesn't open gives `prompt` back to `parent`.
    pub(super) fn start_side_chat(
        &mut self,
        parent: &Entity<SessionView>,
        prompt: Option<Prompt>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let key = parent.read(cx).key();
        let Some((daemon, epoch)) = self.ready(&key.machine) else {
            self.not_connected(&key.machine, "Couldn't start a side chat", window, cx);
            parent.update(cx, |parent, cx| parent.side_chat_failed(prompt, window, cx));
            return;
        };
        let parent = parent.downgrade();
        cx.spawn_in(window, async move |this, cx| {
            let forked = daemon.fork_session(&key.id).await;
            let opened = this.update_in(cx, |this, window, cx| {
                let current = this.is_current(&key.machine, epoch);
                let parent = parent.upgrade();
                match (forked, parent) {
                    (Ok(setup), Some(parent)) if current => {
                        let session = Session::from_setup(setup);
                        let daemon = daemon.clone();
                        return Some(
                            this.open_side_chat(daemon, &parent, session, prompt, window, cx),
                        );
                    }
                    (Ok(setup), parent) => {
                        // The parent's view closed, or the connection
                        // changed, meanwhile: nothing would follow the fork.
                        let daemon = daemon.clone();
                        let fork = setup.id().clone();
                        cx.background_spawn(async move {
                            let _ = daemon.delete_session(&fork).await;
                        })
                        .detach();
                        if let Some(parent) = parent {
                            parent.update(cx, |parent, cx| {
                                parent.side_chat_failed(prompt, window, cx);
                            });
                        }
                    }
                    (Err(error), Some(parent)) => {
                        window.push_notification(
                            Notification::error(error.to_string())
                                .title("Couldn't start a side chat"),
                            cx,
                        );
                        parent.update(cx, |parent, cx| {
                            parent.side_chat_failed(prompt, window, cx);
                        });
                    }
                    (Err(_), None) => {}
                }
                None
            });
            let Ok(Some(id)) = opened else {
                return;
            };
            let attached = daemon.attach(&id.id, None).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.attached(&daemon, epoch, &id, attached, window, cx);
            });
        })
        .detach();
    }

    /// Opens a view for `session`, a fork of `parent`'s, in a new tab of
    /// the right panel, and shows it. Returns its key, for the attach.
    fn open_side_chat(
        &mut self,
        daemon: Daemon,
        parent: &Entity<SessionView>,
        session: Session,
        prompt: Option<Prompt>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> SessionKey {
        let parent_key = parent.read(cx).key();
        let id = SessionKey::new(parent_key.machine.clone(), session.id().clone());
        let auths = self.auths.clone();
        let view = cx.new(|cx| {
            SessionView::side(daemon, session, parent, prompt, window, cx)
                .with_sign_in(auths, window, cx)
        });
        self.insert_session(id.clone(), view.clone(), window, cx);
        PanelLayout::show(cx);
        self.panel.update(cx, |panel, cx| {
            panel.open_side_chat(&parent_key, view, window, cx);
        });
        id
    }

    /// Deletes side chat `id`, whose tab closed and which wound down,
    /// after dropping its view. Off the connection it stays saved, hidden,
    /// until its session is deleted, which deletes it.
    pub(super) fn discard_side_chat(
        &mut self,
        id: &SessionKey,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.forget_session(id, window, cx);
        let Some(daemon) = self.daemon(&id.machine).cloned() else {
            return;
        };
        let id = id.clone();
        cx.spawn_in(window, async move |_, cx| {
            // The daemon deletes no session a client follows.
            let _ = daemon.detach(&id.id).await;
            let deleted = daemon.delete_session(&id.id).await;
            let _ = cx.update(|window, cx| match deleted {
                Ok(()) => cx.global::<BackgroundLog>().forget(&id),
                Err(error) => window.push_notification(
                    Notification::error(error.to_string()).title("Couldn't delete the side chat"),
                    cx,
                ),
            });
        })
        .detach();
    }
}
