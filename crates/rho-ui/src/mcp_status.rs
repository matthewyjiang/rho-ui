//! A session's MCP servers in trouble, in the composer's footer: a plug
//! mark that shows only while one of the session's servers failed, timed
//! out, has an invalid config, or is still connecting, or while the session
//! still loads its tool sources. Clicking it lists the session's servers,
//! the failed ones first with why they failed, and every server's lines
//! from the daemon on hover. When all is well, nothing shows.
//!
//! The daemon pushes no changes to the servers (`rho/session/mcp`), so the
//! view reads them once its session is loaded, again after a reconnect
//! loads it anew, and whenever a turn ends.
//! While they are still starting it reads again every [`STARTUP_POLL`]
//! until they settle.

use std::rc::Rc;
use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    popover::Popover,
    tag::Tag,
    tooltip::Tooltip,
    v_flex, ActiveTheme as _, Icon, Sizable as _, StyledExt as _,
};
use gpui_kit::{
    div, rems, Anchor, AnyElement, App, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Task,
    Window,
};
use rho_ui_core::{Daemon, SessionId, SessionMcpInventory, SessionMcpServer, SessionMcpStatus};

/// How long to wait to read again while a server connects or tool sources
/// load. rho starts a session's servers in seconds, and the read is one
/// small request, so the mark goes within a second of the last server
/// coming up.
const STARTUP_POLL: Duration = Duration::from_secs(1);

/// The MCP servers of one session, as last read, drawn as the mark. A
/// reconnect brings a new one, read once the session is loaded again.
pub(crate) struct McpStatus {
    daemon: Daemon,
    session: SessionId,
    /// The servers as last read, those that need the user first; a failed
    /// read keeps them, as the mark is about the servers, not the
    /// connection.
    servers: Rc<[SessionMcpServer]>,
    /// How many of them failed, and how many still connect.
    failed: usize,
    connecting: usize,
    tool_sources_loading: bool,
    /// The read under way, and those after it while servers start; a newer
    /// one replaces it.
    load: Task<()>,
}

impl McpStatus {
    /// The mark of `session`, on `daemon`, read once the session is loaded
    /// ([`Self::reload`]).
    pub(crate) fn new(daemon: Daemon, session: SessionId) -> Self {
        Self {
            daemon,
            session,
            servers: Rc::from([]),
            failed: 0,
            connecting: 0,
            tool_sources_loading: false,
            load: Task::ready(()),
        }
    }

    /// Reads the servers again, and again every [`STARTUP_POLL`] while they
    /// start.
    pub(crate) fn reload(&mut self, cx: &mut Context<'_, Self>) {
        let (daemon, session) = (self.daemon.clone(), self.session.clone());
        self.load = cx.spawn(async move |this, cx| loop {
            // A daemon older than the inventory, or a read that failed,
            // leaves what shows.
            let Ok(inventory) = daemon.mcp_servers(&session).await else {
                return;
            };
            let Ok(starting) = this.update(cx, |this, cx| this.read(inventory, cx)) else {
                return;
            };
            if !starting {
                return;
            }
            cx.background_executor().timer(STARTUP_POLL).await;
        });
    }

    /// Takes in what was read; says whether servers are still starting, so
    /// a read soon will say more.
    fn read(&mut self, inventory: SessionMcpInventory, cx: &mut Context<'_, Self>) -> bool {
        let mut servers = inventory.servers;
        // Each kind in the daemon's order.
        servers.sort_by_key(|server| need(server.status).1);
        let count = |of: Need| {
            servers
                .iter()
                .filter(|server| need(server.status).1 == of)
                .count()
        };
        self.failed = count(Need::Fix);
        self.connecting = count(Need::Wait);
        self.servers = servers.into();
        self.tool_sources_loading = inventory.tool_sources_loading;
        cx.notify();
        self.connecting > 0 || self.tool_sources_loading
    }

    /// What the mark says: how many servers failed and connect, or that
    /// tools load. `None` when all is well.
    fn summary(&self) -> Option<String> {
        let (failed, connecting) = (self.failed, self.connecting);
        let parts: Vec<String> = [
            (failed > 0).then(|| format!("{failed} failed")),
            (connecting > 0).then(|| format!("{connecting} connecting")),
            (self.tool_sources_loading && failed + connecting == 0)
                .then(|| "loading tools".to_owned()),
        ]
        .into_iter()
        .flatten()
        .collect();
        (!parts.is_empty()).then(|| format!("MCP servers: {}", parts.join(", ")))
    }
}

/// What a server needs from the user, in the order the list shows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Need {
    /// It failed to start: its config or the server needs fixing.
    Fix,
    /// It is still connecting.
    Wait,
    /// Connected, disabled, or not loaded.
    Nothing,
}

/// A server's status in words and as a tag, and what it needs.
fn need(status: SessionMcpStatus) -> ((&'static str, Tag), Need) {
    match status {
        SessionMcpStatus::Connected => (("Connected", Tag::success()), Need::Nothing),
        SessionMcpStatus::Connecting => (("Connecting…", Tag::info()), Need::Wait),
        SessionMcpStatus::Disabled => (("Disabled", Tag::secondary()), Need::Nothing),
        SessionMcpStatus::NotLoaded => (("Not loaded", Tag::secondary()), Need::Nothing),
        SessionMcpStatus::InvalidConfig => (("Invalid config", Tag::danger()), Need::Fix),
        SessionMcpStatus::Failed => (("Failed", Tag::danger()), Need::Fix),
        SessionMcpStatus::TimedOut => (("Timed out", Tag::danger()), Need::Fix),
        // A status a newer daemon sends: its detail says what it means.
        _ => (("Unknown", Tag::secondary()), Need::Nothing),
    }
}

/// One server: its name, tool count and status, and for one that failed,
/// why: the daemon's error, or from a daemon that sends none, the start of
/// its lines on the server. All the lines show on hover. Labelled
/// "name: status" for UI tests, and the why by what it says.
fn render_server(ix: usize, server: &SessionMcpServer, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let ((label, tag), need) = need(server.status);
    let tools = match server.tool_count {
        0 => None,
        1 => Some("1 tool".to_owned()),
        count => Some(format!("{count} tools")),
    };
    let detail = SharedString::from(server.detail.clone());
    v_flex()
        .id(SharedString::from(format!("mcp-server-{ix}")))
        .test_support()
        .aria_label(format!("{}: {label}", server.identity))
        .tooltip({
            let detail = detail.clone();
            move |window, cx| Tooltip::new(detail.clone()).build(window, cx)
        })
        .gap_0p5()
        .child(
            h_flex()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family(theme.mono_font_family.clone())
                        .child(server.identity.clone()),
                )
                .children(tools.map(|tools| {
                    div()
                        .flex_shrink_0()
                        .text_color(theme.muted_foreground)
                        .child(tools)
                }))
                .child(tag.xsmall().child(label)),
        )
        .children((need == Need::Fix).then(|| {
            // The detail is for showing, not parsing: without the error, it
            // says why wherever the daemon put it (rho puts it second).
            let why = server.error.clone().map_or(detail, SharedString::from);
            div()
                .id(SharedString::from(format!("mcp-server-{ix}-why")))
                .test_support()
                .aria_label(why.clone())
                .text_color(theme.muted_foreground)
                .line_clamp(2)
                .child(why)
        }))
        .into_any_element()
}

impl Render for McpStatus {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let Some(summary) = self.summary() else {
            // Hidden, so the footer keeps no gap for it.
            return div().hidden().into_any_element();
        };
        let color = if self.failed > 0 {
            cx.theme().danger
        } else {
            cx.theme().muted_foreground
        };
        let (servers, loading) = (Rc::clone(&self.servers), self.tool_sources_loading);
        Popover::new("mcp-status")
            .anchor(Anchor::BottomRight)
            .trigger(
                Button::new("mcp-status-button")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(Lucide::Plug).text_color(color))
                    .tooltip(summary),
            )
            .content(move |_, _, cx| {
                let muted = cx.theme().muted_foreground;
                v_flex()
                    .id("mcp-status-servers")
                    .test_support()
                    .w(rems(22.))
                    .max_h(rems(20.))
                    .overflow_y_scroll()
                    .gap_2()
                    .text_xs()
                    .child(div().font_semibold().text_color(muted).child("MCP servers"))
                    .children(
                        servers
                            .iter()
                            .enumerate()
                            .map(|(ix, server)| render_server(ix, server, cx)),
                    )
                    // Tool sources other than MCP servers load too.
                    .children(
                        loading.then(|| div().text_color(muted).child("Loading tool sources…")),
                    )
            })
            .into_any_element()
    }
}
