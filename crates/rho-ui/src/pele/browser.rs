//! pele in a browser, as the page's own JavaScript: [`start`] loads it
//! into the page once, and [`draw`] calls it. A browser can't stop a
//! script that runs away, as `QuickJS`'s limits do off the browser, so a
//! diagram that took pele that long would hang the page; pele draws each
//! kind rho's prompt allows in milliseconds.

use std::{
    cell::{OnceCell, RefCell},
    collections::HashMap,
    sync::Arc,
};

use js_sys::{Function, Reflect};
use wasm_bindgen::{JsCast, JsValue};

use super::{narrow, Drawing, KEPT, PELE};

thread_local! {
    /// `pele.render` once pele has loaded, or `None` if it didn't; unset
    /// until it first loads.
    static RENDER: OnceCell<Option<Function>> = const { OnceCell::new() };
    /// Drawings already made, by source, as the engine keeps them.
    static DRAWN: RefCell<HashMap<String, Option<Arc<Drawing>>>> = RefCell::new(HashMap::new());
}

/// Loads pele into the page, so the first diagram doesn't wait for it.
/// [`draw`] loads it too.
pub(crate) fn start() {
    render();
}

/// `pele.render`, loading pele first if need be. pele's build is strict
/// code, whose `var pele` an `eval` would keep to itself, so it runs as
/// the body of a function that returns it.
fn render() -> Option<Function> {
    RENDER.with(|render| {
        render
            .get_or_init(|| {
                let load = Function::new_no_args(&format!("{PELE}\nreturn pele;"));
                let loaded = load
                    .call0(&JsValue::NULL)
                    .and_then(|pele| Reflect::get(&pele, &"render".into()))
                    .and_then(JsCast::dyn_into::<Function>);
                loaded
                    .inspect_err(|error| {
                        log::error!(
                            "rho-ui: pele didn't load, so diagrams show as code: {error:?}"
                        );
                    })
                    .ok()
            })
            .clone()
    })
}

/// Draws Mermaid `source`; `None` when pele can't.
pub(crate) fn draw(source: &str) -> Option<Arc<Drawing>> {
    if let Some(drawing) = DRAWN.with_borrow(|drawn| drawn.get(source).cloned()) {
        return drawing;
    }
    let drawing = render().and_then(|render| {
        let drawn = render.call1(&JsValue::NULL, &source.into()).ok()?;
        let get = |key: &str| Reflect::get(&drawn, &key.into()).ok();
        let size = |key| get(key)?.as_f64().map(narrow);
        Some(Arc::new(Drawing {
            svg: get("svg")?.as_string()?,
            width: size("width")?,
            height: size("height")?,
        }))
    });
    DRAWN.with_borrow_mut(|drawn| {
        if drawn.len() == KEPT {
            drawn.clear();
        }
        drawn.insert(source.to_owned(), drawing.clone());
    });
    drawing
}
