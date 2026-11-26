//! What a session's MCP servers are doing and offer: the daemon's session
//! connected them, so it reports each one's status and tools, lists the
//! prompts and resources they offer, reads a resource, and asks for values
//! of a prompt's argument, each without waiting for the session's turn.

use base64::Engine as _;
use rho_protocol::{
    methods, ErrorCode, SessionMcpCatalog, SessionMcpCatalogParams, SessionMcpCompleteParams,
    SessionMcpInventory, SessionMcpParams, SessionMcpResourceContent, SessionMcpResourceParams,
};

use super::{Daemon, DaemonError, SessionId};
use crate::{ArgumentAsk, AttachedFile, Attachment, Image};

/// The most text one resource brings to a prompt, in bytes: about the
/// 200,000 characters of a text file sent whole
/// ([`crate::MAX_ATTACHED_TEXT_CHARS`]), counted in the bytes the daemon
/// cuts a resource's text by.
pub const MAX_RESOURCE_TEXT_BYTES: usize = 200 << 10;

/// A resource read to go with a prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceAttachment {
    /// A lone image as itself; anything else as one text file under the
    /// resource's URI, each body's text in turn, a binary one described.
    pub attachment: Attachment,
    /// Its text came to more than [`MAX_RESOURCE_TEXT_BYTES`], so it holds
    /// that much, and a last line saying it was cut.
    pub cut: bool,
}

impl Daemon {
    /// The MCP servers session `id` was configured with, each with its
    /// status, as they are now. Nothing pushes changes to it; ask again to
    /// refresh.
    ///
    /// # Errors
    ///
    /// Fails if the daemon has not loaded the session, is older than the
    /// inventory (`MethodNotFound`), or the connection is gone.
    pub async fn mcp_servers(&self, id: &SessionId) -> Result<SessionMcpInventory, DaemonError> {
        let params = SessionMcpParams {
            session: id.to_wire(),
        };
        Ok(self.client.request::<methods::SessionMcp>(&params).await?)
    }

    /// The prompts and resources session `id`'s MCP servers offer now: what
    /// the composer completes an `@` mention and a prompt's arguments from;
    /// `None` from a daemon older than the catalog, which will never have
    /// one. Ask again to see what a server added or removed.
    ///
    /// # Errors
    ///
    /// Fails if the daemon has not loaded the session, or the connection is
    /// gone.
    pub async fn mcp_catalog(
        &self,
        id: &SessionId,
    ) -> Result<Option<SessionMcpCatalog>, DaemonError> {
        let params = SessionMcpCatalogParams {
            session: id.to_wire(),
        };
        match self
            .client
            .request::<methods::SessionMcpCatalogRead>(&params)
            .await
        {
            Ok(catalog) => Ok(Some(catalog)),
            Err(error) if error.error_code() == Some(ErrorCode::MethodNotFound) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Reads resource `uri`, a concrete one, from session `id`'s MCP server
    /// `server`, to attach to a prompt.
    ///
    /// # Errors
    ///
    /// Fails if the session hasn't connected `server`, the server refuses
    /// or fails the read, the daemon is older than resource reads
    /// (`MethodNotFound`), or the connection is gone; the daemon's message
    /// says which.
    pub async fn read_mcp_resource(
        &self,
        id: &SessionId,
        server: &str,
        uri: &str,
    ) -> Result<ResourceAttachment, DaemonError> {
        let params = SessionMcpResourceParams {
            session: id.to_wire(),
            server: server.to_owned(),
            uri: uri.to_owned(),
            // A usize fits in a u64 on every target rho-ui builds for.
            max_text_bytes: u64::try_from(MAX_RESOURCE_TEXT_BYTES).ok(),
        };
        let response = self
            .client
            .request::<methods::SessionMcpResourceRead>(&params)
            .await?;
        Ok(resource_attachment(uri, &response.contents))
    }

    /// The values session `id`'s MCP server suggests for what `ask` says is
    /// typed of a prompt's argument, best first. None when it has none, or
    /// can't be asked: a suggestion is only help.
    pub async fn complete_mcp_argument(&self, id: &SessionId, ask: &ArgumentAsk) -> Vec<String> {
        let params = SessionMcpCompleteParams {
            session: id.to_wire(),
            server: ask.server.clone(),
            prompt: ask.prompt.clone(),
            argument: ask.argument.clone(),
            value: ask.value.clone(),
        };
        self.client
            .request::<methods::SessionMcpComplete>(&params)
            .await
            .map(|response| response.values)
            .unwrap_or_default()
    }
}

/// The one attachment the bodies a read of `uri` returned make: a lone
/// image of a format rho takes as itself, so the model sees it; anything
/// else as one text file, so a resource of several bodies arrives whole.
fn resource_attachment(uri: &str, contents: &[SessionMcpResourceContent]) -> ResourceAttachment {
    if let [SessionMcpResourceContent::Blob {
        mime_type: Some(mime_type),
        blob,
        ..
    }] = contents
    {
        if let Some(image) = Image::from_wire(mime_type, blob) {
            return ResourceAttachment {
                attachment: Attachment::Image(image),
                cut: false,
            };
        }
    }
    let mut text = contents
        .iter()
        .map(body_text)
        .collect::<Vec<_>>()
        .join("\n\n");
    let cut = text.len() > MAX_RESOURCE_TEXT_BYTES;
    if cut {
        text.truncate(text.floor_char_boundary(MAX_RESOURCE_TEXT_BYTES));
        let kib = MAX_RESOURCE_TEXT_BYTES >> 10;
        text = format!("{text}\n\n[rho-ui sent the first {kib} KiB of this resource]");
    }
    ResourceAttachment {
        attachment: Attachment::File(AttachedFile::resource(uri.to_owned(), text)),
        cut,
    }
}

/// A body as text: text as itself, and a binary one, which a text file
/// can't carry, as a line saying what it is.
fn body_text(body: &SessionMcpResourceContent) -> String {
    match body {
        SessionMcpResourceContent::Text { text, .. } => text.clone(),
        SessionMcpResourceContent::Blob {
            mime_type, blob, ..
        } => {
            let kind = mime_type.as_deref().unwrap_or("application/octet-stream");
            match base64::engine::general_purpose::STANDARD.decode(blob) {
                Ok(bytes) => format!("[binary resource: {kind}, {} bytes]", bytes.len()),
                Err(_) => format!("[binary resource: {kind}, not valid base64]"),
            }
        }
        // `Unknown`, or a kind a newer daemon sends.
        _ => "[a kind of resource rho-ui can't show]".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(text: &str) -> SessionMcpResourceContent {
        SessionMcpResourceContent::Text {
            uri: "docs://a".to_owned(),
            mime_type: Some("text/markdown".to_owned()),
            text: text.to_owned(),
        }
    }

    fn blob(mime_type: &str, bytes: &[u8]) -> SessionMcpResourceContent {
        SessionMcpResourceContent::Blob {
            uri: "docs://a".to_owned(),
            mime_type: Some(mime_type.to_owned()),
            blob: base64::engine::general_purpose::STANDARD.encode(bytes),
        }
    }

    /// The text a read sends as a file, and whether it was cut.
    fn sent(contents: &[SessionMcpResourceContent]) -> (Option<String>, bool) {
        let read = resource_attachment("docs://a", contents);
        let text = match &read.attachment {
            Attachment::File(file) => {
                assert_eq!(file.uri(), "docs://a", "it goes under its URI");
                file.text().map(str::to_owned)
            }
            Attachment::Image(_) => None,
        };
        (text, read.cut)
    }

    /// A lone image goes as an image; every other read goes whole as one
    /// text file, binary bodies described, cut at the limit with a line
    /// saying so.
    #[test]
    fn a_read_goes_as_one_attachment() {
        let png = b"\x89PNG\r\n\x1a\nfake";
        let read = resource_attachment("docs://a", &[blob("image/png", png)]);
        assert!(
            matches!(&read.attachment, Attachment::Image(image) if image.bytes() == png),
            "{read:?}"
        );

        let cases = [
            (vec![text("# Intro")], "# Intro"),
            (
                vec![text("one"), blob("image/png", png), text("two")],
                "one\n\n[binary resource: image/png, 12 bytes]\n\ntwo",
            ),
            (
                vec![blob("application/pdf", b"%PDF")],
                "[binary resource: application/pdf, 4 bytes]",
            ),
        ];
        for (contents, want) in cases {
            assert_eq!(sent(&contents), (Some(want.to_owned()), false), "{want:?}");
        }

        // The daemon may send more than the limit; it is cut at a
        // character.
        let long = "é".repeat(MAX_RESOURCE_TEXT_BYTES);
        let (text, cut) = sent(&[self::text(&long)]);
        let text = text.expect("text");
        assert!(cut);
        let (kept, note) = text.split_once("\n\n").expect("a note after the text");
        assert_eq!(kept.len(), MAX_RESOURCE_TEXT_BYTES, "a whole number of é");
        assert_eq!(note, "[rho-ui sent the first 200 KiB of this resource]");
    }
}
