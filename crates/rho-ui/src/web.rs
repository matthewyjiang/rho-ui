//! rho-ui in a browser: the page rho-ui-serve serves loads this build,
//! which reaches the server's machine through the server, as the desktop
//! app reaches a remote machine over ssh (`rho_ui_core::server`). That
//! machine is the one rho-ui calls this machine.
//!
//! A browser has no fonts GPUI can read, so the page's fonts come from the
//! server, at `/fonts/` (`scripts/build-web.sh` puts them there), before
//! the window opens. The state database lives in memory, and the
//! settings aren't saved: nothing is remembered past the page.

use std::borrow::Cow;

use gpui_kit::AsyncApp;
use js_sys::Uint8Array;
use rho_ui_core::{DaemonTarget, RhoInstall, ServerTarget};
use wasm_bindgen::JsCast as _;
use wasm_bindgen_futures::JsFuture;

use crate::{
    app::LaunchConfig, machines::Reach, open_app, right_panel::TerminalShell, store::StateDb,
    Assets,
};

/// The fonts the page loads, from the server's `/fonts/`: the default UI
/// and code fonts (`settings.rs`), in the styles rho-ui draws.
const FONTS: [&str; 7] = [
    "NotoSans-Regular.ttf",
    "NotoSans-Bold.ttf",
    "NotoSans-Italic.ttf",
    "NotoSans-BoldItalic.ttf",
    "JetBrainsMono-Regular.ttf",
    "JetBrainsMono-Bold.ttf",
    "JetBrainsMono-Italic.ttf",
];

/// Starts the web build: loads the fonts, then opens the window on the
/// page's canvas, with the server's machine as this machine.
pub(crate) fn main() {
    gpui_kit::platform::web_init();
    let window = web_sys::window().expect("rho-ui runs in a browser window");
    let origin = window.location().origin().expect("the page has an origin");
    let server = ServerTarget::new(origin.clone(), token(&window));
    let launch = LaunchConfig {
        workspace: None,
        local: Reach {
            daemon: DaemonTarget::Server(server.clone()),
            rho: Some(RhoInstall::Server(server)),
        },
        shell: TerminalShell::from_env(),
    };
    gpui_kit::platform::single_threaded_web()
        .with_assets(Assets::new(&origin))
        .run(move |cx| {
            cx.spawn(async move |cx: &mut AsyncApp| {
                let fonts = load_fonts().await;
                cx.update(|cx| {
                    if let Err(error) = cx.text_system().add_fonts(fonts) {
                        log::error!("rho-ui: the fonts didn't load: {error}");
                    }
                    // A browser has no folder to keep it in.
                    open_app(launch, &StateDb::in_memory(), cx);
                });
            })
            .detach();
        });
}

/// The key the tab keeps the server's token under.
const TOKEN_KEY: &str = "rho-ui-token";

/// The server's token: from the address's `#token=`, as rho-ui-serve
/// prints it, which this then keeps for the tab and takes out of the
/// address, so a reload still has it and the address bar doesn't show it;
/// else the one the tab kept. Empty if neither, which the server refuses,
/// and the page says it can't reach the machine.
fn token(window: &web_sys::Window) -> String {
    let storage = window.session_storage().ok().flatten();
    let fragment = window.location().hash().unwrap_or_default();
    if let Some(token) = fragment
        .strip_prefix("#token=")
        .filter(|token| !token.is_empty())
    {
        if let Some(storage) = &storage {
            let _ = storage.set_item(TOKEN_KEY, token);
        }
        if let Ok(history) = window.history() {
            let _ = history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some("/"));
        }
        return token.to_owned();
    }
    storage
        .and_then(|storage| storage.get_item(TOKEN_KEY).ok().flatten())
        .unwrap_or_else(|| {
            log::error!("rho-ui: no token: open the URL rho-ui-serve printed");
            String::new()
        })
}

/// [`FONTS`], each as the server sent it; one that didn't come is left out,
/// and said in the browser's console.
async fn load_fonts() -> Vec<Cow<'static, [u8]>> {
    let mut fonts = Vec::new();
    for font in FONTS {
        match fetch(&format!("/fonts/{font}")).await {
            Ok(bytes) => fonts.push(Cow::Owned(bytes)),
            Err(why) => log::error!("rho-ui: couldn't load the font {font}: {why}"),
        }
    }
    fonts
}

/// The body of `url`, from the page's server.
async fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let window = web_sys::window().ok_or("no browser window")?;
    let response = JsFuture::from(window.fetch_with_str(url))
        .await
        .map_err(|error| format!("{error:?}"))?;
    let response: web_sys::Response = response.dyn_into().map_err(|_| "no response".to_owned())?;
    if !response.ok() {
        return Err(format!("HTTP {}", response.status()));
    }
    let body = response
        .array_buffer()
        .map_err(|error| format!("{error:?}"))?;
    let body = JsFuture::from(body)
        .await
        .map_err(|error| format!("{error:?}"))?;
    Ok(Uint8Array::new(&body).to_vec())
}
