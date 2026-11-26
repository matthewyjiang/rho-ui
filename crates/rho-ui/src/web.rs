//! rho-ui in a browser: the page `rho-ui serve` serves loads this build,
//! which reaches the server's machine through the server, as the desktop
//! app reaches a remote machine over ssh (`rho_ui_core::server`). That
//! machine is the one rho-ui calls this machine. The remote machines the
//! settings list, the server reaches over ssh for the page.
//!
//! A browser has no fonts GPUI can read, so the page's fonts come from the
//! server, at `/fonts/` (`cargo xtask web` puts them there), before
//! the window opens, and so do the desktop app's settings and state
//! database, as the server shares them: the page keeps a copy, and sends
//! its changes back.

use std::{borrow::Cow, rc::Rc};

use gpui_kit::AsyncApp;
use js_sys::Uint8Array;
use rho_ui_core::{server::wire::StateWrite, DaemonTarget, RhoInstall, ServerTarget};
use wasm_bindgen::JsCast as _;
use wasm_bindgen_futures::JsFuture;

use crate::{
    app::LaunchConfig,
    attention::Notifier,
    machines::{Reach, RemoteMachine},
    open_app,
    right_panel::TerminalShell,
    settings::Settings,
    store::StateDb,
    Assets,
};

/// The fonts the page loads, from the server's `/fonts/`: the default UI
/// and code fonts (`settings.rs`), in the styles rho-ui draws.
const FONTS: [&str; 8] = [
    "NotoSans-Regular.ttf",
    "NotoSans-Bold.ttf",
    "NotoSans-Italic.ttf",
    "NotoSans-BoldItalic.ttf",
    "JetBrainsMonoNerdFont-Regular.ttf",
    "JetBrainsMonoNerdFont-Bold.ttf",
    "JetBrainsMonoNerdFont-Italic.ttf",
    "JetBrainsMonoNerdFont-BoldItalic.ttf",
];

/// The fonts GPUI's SVG renderer asks the assets for, in place of the
/// system's, which a browser has none of: for a diagram's text. They are
/// the page's own regular faces (`FONTS`), which a drawing names by family.
const SVG_FONTS: [(&str, &str); 2] = [
    (
        "fonts/ibm-plex-sans/IBMPlexSans-Regular.ttf",
        "NotoSans-Regular.ttf",
    ),
    (
        "fonts/lilex/Lilex-Regular.ttf",
        "JetBrainsMonoNerdFont-Regular.ttf",
    ),
];

thread_local! {
    /// The page's fonts, by file name, once loaded: before the window
    /// opens, so before any diagram is drawn.
    static LOADED: std::cell::RefCell<std::collections::HashMap<&'static str, Cow<'static, [u8]>>> =
        std::cell::RefCell::default();
}

/// The font GPUI's SVG renderer asks for at `path`, as [`SVG_FONTS`] says;
/// `None` for any other path.
pub(crate) fn svg_font(path: &str) -> Option<Cow<'static, [u8]>> {
    let (_, file) = SVG_FONTS.iter().find(|(asked, _)| *asked == path)?;
    LOADED.with_borrow(|loaded| loaded.get(file).cloned())
}

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
            rho: Some(RhoInstall::Server(server.clone())),
        },
        remote: {
            let server = server.clone();
            Rc::new(move |remote: &RemoteMachine| remote.server_reach(&server))
        },
        shell: TerminalShell::from_env(),
        rho_home: None,
        notifier: Notifier::System,
    };
    gpui_kit::platform::single_threaded_web()
        .with_assets(Assets::new(&origin))
        .run(move |cx| {
            cx.spawn(async move |cx: &mut AsyncApp| {
                let fonts = load_fonts().await;
                let (db, settings) = shared_state(&server).await;
                cx.update(|cx| {
                    if let Err(error) = cx.text_system().add_fonts(fonts) {
                        log::error!("rho-ui: the fonts didn't load: {error}");
                    }
                    open_app(launch, &db, settings, cx);
                });
            })
            .detach();
        });
}

/// The desktop app's state database and settings on the server's machine,
/// which the page then writes back to. If they can't be had, ones in
/// memory, which the page forgets, and the console says why.
async fn shared_state(server: &ServerTarget) -> (StateDb, Settings) {
    let shared = match server.open_state().await {
        Ok(shared) => shared,
        Err(error) => {
            log::error!("rho-ui: {error}; nothing will be remembered");
            return (StateDb::in_memory(), Settings::default());
        }
    };
    let writer = shared.writer.clone();
    let forward = move |sql| writer.write(StateWrite::Sql { sql });
    let db = StateDb::from_image(&shared.database, forward).unwrap_or_else(|error| {
        log::error!("rho-ui: the state database didn't load: {error}; nothing will be remembered");
        StateDb::in_memory()
    });
    let settings = Settings::shared(shared.settings, shared.writer);
    // The page holds its writer while open, so this ends only if the
    // connection went.
    let done = shared.done;
    wasm_bindgen_futures::spawn_local(async move {
        done.wait().await;
        log::error!(
            "rho-ui: the connection to rho-ui serve went; changes from now on aren't saved"
        );
    });
    (db, settings)
}

/// The key the tab keeps the server's token under.
const TOKEN_KEY: &str = "rho-ui-token";

/// The server's token: from the address's `#token=`, as `rho-ui serve`
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
            log::error!("rho-ui: no token: open the URL `rho-ui serve` printed");
            String::new()
        })
}

/// [`FONTS`], each as the server sent it; one that didn't come is left out,
/// and said in the browser's console.
async fn load_fonts() -> Vec<Cow<'static, [u8]>> {
    let mut fonts = Vec::new();
    for font in FONTS {
        match fetch(&format!("/fonts/{font}")).await {
            Ok(bytes) => {
                if SVG_FONTS.iter().any(|(_, file)| *file == font) {
                    LOADED.with_borrow_mut(|loaded| loaded.insert(font, Cow::Owned(bytes.clone())));
                }
                fonts.push(Cow::Owned(bytes));
            }
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
