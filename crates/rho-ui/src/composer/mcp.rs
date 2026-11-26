//! What the composer asks the session's MCP servers: the prompts and
//! resources they offer, a resource to attach, and values for a prompt's
//! argument. Only a composer of an open session asks
//! ([`ComposerState::set_mcp`]); a draft's has no servers yet.
//!
//! The catalog is read again whenever a new `@` mention starts, or the
//! prompt starts with an MCP prompt's command not asked for yet, so what a
//! server added since shows. Values are asked for as an argument's value is
//! typed: only the latest request counts, and the menu keeps the last
//! answer for the argument until the next one comes.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::{Context, SharedString, Task};
use rho_ui_core::{
    mcp_command, ArgumentAsk, Daemon, SessionId, SessionMcpCatalog, SessionMcpPrompt,
    SessionMcpResource, MAX_RESOURCE_TEXT_BYTES,
};

use super::{ComposerState, Typing};
use crate::attachments::Preview;

/// Where the composer's MCP requests go: the session's daemon, as its view
/// has it now, and the session.
#[derive(Clone)]
pub(crate) struct McpLink {
    pub(crate) daemon: Daemon,
    pub(crate) session: SessionId,
}

/// The composer's MCP requests and what they answered.
#[derive(Default)]
pub(super) struct Mcp {
    link: Option<McpLink>,
    /// What the session's servers offered when last asked.
    catalog: Option<SessionMcpCatalog>,
    /// What the catalog was last asked for, so the same mention or command
    /// doesn't ask again.
    catalog_for: Option<CatalogFor>,
    /// The daemon has no catalog (`MethodNotFound`), so the link asks no
    /// more until a reconnect brings another.
    no_catalog: bool,
    /// The catalog request in flight. Dropping it cancels it.
    catalog_task: Option<Task<()>>,
    /// The last values asked for, and the last answer, which may be for an
    /// earlier ask of the same argument.
    values_for: Option<ArgumentAsk>,
    values: Option<(ArgumentAsk, Vec<String>)>,
    /// The values request in flight. Dropping it cancels it.
    values_task: Option<Task<()>>,
}

/// What the catalog is read for.
#[derive(Clone, Debug, PartialEq, Eq)]
enum CatalogFor {
    /// The `@` mention that starts at this byte.
    Mention(usize),
    /// The MCP prompt command that starts the prompt.
    Command(String),
}

/// A resource being read to attach: shown as a card, holding the send back
/// until it is attached or fails.
pub(super) struct Read {
    pub(super) id: u64,
    pub(super) preview: Preview,
    /// The read; removing the card drops it, which cancels it.
    _task: Task<()>,
}

impl Mcp {
    /// The prompts the servers offered when last asked.
    pub(super) fn prompts(&self) -> &[SessionMcpPrompt] {
        self.catalog
            .as_ref()
            .map_or(&[], |catalog| catalog.prompts.as_slice())
    }

    /// The resources the servers offered when last asked.
    pub(super) fn resources(&self) -> &[SessionMcpResource] {
        self.catalog
            .as_ref()
            .map_or(&[], |catalog| catalog.resources.as_slice())
    }

    /// The values the server last suggested for the argument `ask` asks
    /// about. An answer for what was typed before is kept while the next
    /// comes, with only the values that start with what is typed now, so
    /// none taken overwrites the value with one it no longer means.
    pub(super) fn values<'a>(&'a self, ask: &'a ArgumentAsk) -> impl Iterator<Item = &'a String> {
        let answer = self.values.as_ref().filter(|(answered, _)| {
            (&answered.server, &answered.prompt, &answered.argument)
                == (&ask.server, &ask.prompt, &ask.argument)
        });
        let (answered, values) = match answer {
            Some((answered, values)) => (Some(answered), values.as_slice()),
            None => (None, [].as_slice()),
        };
        let stale = answered.is_some_and(|answered| answered.value != ask.value);
        let typed = ask.value.to_lowercase();
        values
            .iter()
            .filter(move |value| !stale || value.to_lowercase().starts_with(&typed))
    }
}

impl Mcp {
    /// Forgets the values last suggested, once one is taken: those were
    /// for what was typed before it.
    pub(super) fn forget_values(&mut self) {
        self.values = None;
    }
}

impl ComposerState {
    /// Sends the composer's MCP requests to `link`, the session's daemon as
    /// it is now; see [`McpLink`]. The view sets it again when a reconnect
    /// brings another daemon, which is asked about the catalog afresh.
    pub(crate) fn set_mcp(&mut self, link: McpLink) {
        self.mcp.link = Some(link);
        self.mcp.no_catalog = false;
        self.mcp.catalog_for = None;
    }

    /// Asks the session's servers what follows from `typing`, the word at
    /// the cursor of `text`, newly typed: the catalog for a new mention or
    /// prompt command, and values for an argument's value.
    pub(super) fn ask_mcp(
        &mut self,
        text: &str,
        typing: Option<&Typing>,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(link) = self.mcp.link.clone() else {
            return;
        };
        let catalog_for = match typing {
            Some(Typing::Mention(mention)) => Some(CatalogFor::Mention(mention.start())),
            _ => mcp_command(text).map(|command| CatalogFor::Command(command.to_owned())),
        };
        // Each mention or command asks once, even one where another was.
        let asked = std::mem::replace(&mut self.mcp.catalog_for, catalog_for.clone());
        if catalog_for.is_some() && catalog_for != asked && !self.mcp.no_catalog {
            let link = link.clone();
            self.mcp.catalog_task = Some(cx.spawn(async move |this, cx| {
                let catalog = link.daemon.mcp_catalog(&link.session).await;
                let _ = this.update(cx, |this, cx| {
                    this.mcp.catalog_task = None;
                    match catalog {
                        Ok(Some(catalog)) => {
                            this.mcp.catalog = Some(catalog);
                            this.refresh_menu(cx);
                        }
                        Ok(None) => this.mcp.no_catalog = true,
                        // Still loading, say: the next key asks again, and
                        // what was offered before stays, so nothing redraws.
                        Err(_) => this.mcp.catalog_for = None,
                    }
                });
            }));
        }
        if let Some(Typing::Value(value)) = typing {
            let ask = value.ask().clone();
            if self.mcp.values_for.as_ref() != Some(&ask) {
                self.mcp.values_for = Some(ask.clone());
                self.mcp.values_task = Some(cx.spawn(async move |this, cx| {
                    let values = link.daemon.complete_mcp_argument(&link.session, &ask).await;
                    let _ = this.update(cx, |this, cx| {
                        this.mcp.values = Some((ask, values));
                        this.mcp.values_task = None;
                        this.refresh_menu(cx);
                    });
                }));
            }
        }
    }

    /// Reads `resource`, a concrete one, to attach once it arrives; until
    /// then it shows as a card that holds the send back. A failed read says
    /// why above the composer.
    pub(super) fn read_resource(
        &mut self,
        resource: &SessionMcpResource,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(link) = self.mcp.link.clone() else {
            return;
        };
        self.next_id += 1;
        let id = self.next_id;
        let (server, uri) = (resource.server.clone(), resource.uri.clone());
        let name: SharedString = resource.name.clone().into();
        let task = cx.spawn(async move |this, cx| {
            let read = link
                .daemon
                .read_mcp_resource(&link.session, &server, &uri)
                .await;
            let _ = this.update(cx, |this, cx| {
                // The attachment, or why it couldn't come, replaces the card.
                // Had the card been removed, its task would have been dropped.
                this.reads.retain(|read| read.id != id);
                let added = read.map_err(|error| error.to_string()).and_then(|read| {
                    let cut = read.cut;
                    let added = this.add(read.attachment, None)?;
                    if cut {
                        let item = this.attachments.iter_mut().find(|item| item.id == added);
                        if let Some(item) = item {
                            item.preview = item.preview.clone().cut(MAX_RESOURCE_TEXT_BYTES);
                        }
                    }
                    Ok(())
                });
                if let Err(why) = added {
                    this.refusal = Some(format!("Couldn't attach {uri}: {why}.").into());
                }
                cx.notify();
            });
        });
        self.reads.push(Read {
            id,
            preview: Preview::File {
                name,
                detail: "Reading…".into(),
                tooltip: resource.uri.clone().into(),
                icon: Lucide::Plug,
            },
            _task: task,
        });
        cx.notify();
    }
}
