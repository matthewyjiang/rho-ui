//! The web build's files: `index.html` at `/`, and the JS, wasm and fonts
//! it loads.

use std::path::{Path, PathBuf};

use axum::{
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, MethodRouter},
};

/// Serves the files in `dist`, `index.html` for `/`. A path with a `..`,
/// or any segment that names no plain file name, gets 404, so nothing
/// outside `dist` is served.
pub(crate) fn serve(dist: PathBuf) -> MethodRouter {
    get(move |uri: Uri| {
        let dist = dist.clone();
        async move { file(&dist, uri.path()).await }
    })
}

async fn file(dist: &Path, path: &str) -> Response {
    let relative = match path.trim_start_matches('/') {
        "" => "index.html",
        relative => relative,
    };
    let plain = relative
        .split('/')
        .all(|segment| !segment.is_empty() && segment != "." && segment != "..");
    if !plain {
        return StatusCode::NOT_FOUND.into_response();
    }
    match tokio::fs::read(dist.join(relative)).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, content_type(relative))], bytes).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// The type a browser needs to use the file: wasm must be
/// `application/wasm` to compile as it streams in.
fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, extension)| extension) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    }
}
