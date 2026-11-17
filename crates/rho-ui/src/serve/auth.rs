//! Who may use the server: a page that holds its token.
//!
//! The server prints a URL with the token in its fragment, `/#token=...`,
//! which a browser never sends anywhere: the page reads it, keeps it for
//! the tab, and offers it with each WebSocket it opens, as a subprotocol
//! ([`wire::token_protocol`]), since a browser's WebSocket takes no header
//! of its own. A cookie would do too, but a browser sends a cookie to
//! every port on its host, so the token would reach any other server
//! there. The page's files hold no secret and need no token.
//!
//! A WebSocket must also come from a page this server served, as its
//! `Origin` says: with the token alone, a page another server on this
//! machine serves, which can't read this page's storage, couldn't connect
//! anyway, but this keeps it so even if the token leaked to one.

use std::{fmt::Write as _, sync::Arc};

use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use rho_ui_core::server::wire;

/// The secret a page must hold: 32 random bytes, in hex.
#[derive(Debug)]
pub(crate) struct Token(String);

impl Token {
    /// A new token, from the operating system's random source.
    ///
    /// # Panics
    ///
    /// If the operating system has no random source to read.
    pub(crate) fn new() -> Self {
        let mut bytes = [0_u8; 32];
        getrandom::fill(&mut bytes).expect("the OS has a random source");
        let hex = bytes.iter().fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        });
        Self(hex)
    }

    /// The token, for the URL the server prints.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    /// The subprotocol a WebSocket offers the token as, which the server
    /// names back when it accepts.
    pub(crate) fn protocol(&self) -> String {
        wire::token_protocol(&self.0)
    }

    /// Whether `offered` is the token's subprotocol, in time that doesn't
    /// depend on where they differ.
    fn matches(&self, offered: &str) -> bool {
        let ours = self.protocol();
        let (ours, theirs) = (ours.as_bytes(), offered.as_bytes());
        ours.len() == theirs.len()
            && ours
                .iter()
                .zip(theirs)
                .fold(0, |differ, (a, b)| differ | (a ^ b))
                == 0
    }
}

impl Default for Token {
    fn default() -> Self {
        Self::new()
    }
}

/// Lets a WebSocket through if it offers the token and comes from a page
/// of this server's; any other request is for the page's files.
///
/// # Panics
///
/// Never: an offer that matched the token is a header value already.
pub(crate) async fn check(
    State(token): State<Arc<Token>>,
    request: Request,
    next: Next,
) -> Response {
    let headers = request.headers();
    if !is_websocket(headers) {
        return next.run(request).await;
    }
    let Some(offered) = offered_protocols(headers).find(|offered| token.matches(offered)) else {
        return (
            StatusCode::UNAUTHORIZED,
            "rho-ui serve: open the URL it printed when it started, with its token",
        )
            .into_response();
    };
    if !same_origin(headers) {
        return (
            StatusCode::FORBIDDEN,
            "rho-ui serve: a WebSocket from another site",
        )
            .into_response();
    }
    let accepted = HeaderValue::from_str(offered).expect("the token's protocol is a header value");
    let mut response = next.run(request).await;
    // Named back, as a client that offered subprotocols refuses an answer
    // without one; every endpoint's answer gets it here.
    if response.status() == StatusCode::SWITCHING_PROTOCOLS {
        response
            .headers_mut()
            .insert(header::SEC_WEBSOCKET_PROTOCOL, accepted);
    }
    response
}

/// The subprotocols a WebSocket offers.
fn offered_protocols(headers: &HeaderMap) -> impl Iterator<Item = &str> {
    headers
        .get_all(header::SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|protocols| protocols.split(','))
        .map(str::trim)
}

fn is_websocket(headers: &HeaderMap) -> bool {
    headers
        .get(header::UPGRADE)
        .is_some_and(|upgrade| upgrade.as_bytes().eq_ignore_ascii_case(b"websocket"))
}

/// Whether the request's `Origin` is the server it was sent to, as a page
/// this server served sends; one with no `Origin` is no browser's.
fn same_origin(headers: &HeaderMap) -> bool {
    let header = |name| headers.get(name).and_then(|value| value.to_str().ok());
    match (header(header::ORIGIN), header(header::HOST)) {
        (Some(origin), Some(host)) => origin.strip_prefix("http://") == Some(host),
        _ => false,
    }
}
