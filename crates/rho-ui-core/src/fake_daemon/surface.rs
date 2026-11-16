//! The surfaces the fake daemon's `ui` tool shows, as the A2UI v1.0
//! messages a real daemon compiles a model's Express block to.

use serde_json::{json, Value};

/// The basic catalog's ID.
const CATALOG: &str = "https://a2ui.org/specification/v1_0/catalogs/basic/catalog.json";

/// The ID of the surface the prompt "ui" shows.
pub const FORM_SURFACE: &str = "deploy";

/// The ID of the surface the prompt "gallery" shows.
pub const GALLERY_SURFACE: &str = "gallery";

/// The name of the event the fake daemon refuses, as a session does that
/// no longer has the surface.
pub const REFUSED_EVENT: &str = "refuse";

fn create(surface: &str) -> Value {
    json!({"version": "1.0", "createSurface": {
        "surfaceId": surface,
        "catalogId": CATALOG,
        "sendDataModel": true,
    }})
}

fn components(surface: &str, components: &Value) -> Value {
    json!({"version": "1.0", "updateComponents": {
        "surfaceId": surface,
        "components": components,
    }})
}

fn data(surface: &str, value: &Value) -> Value {
    json!({"version": "1.0", "updateDataModel": {"surfaceId": surface, "value": value}})
}

fn text(id: &str, text: &str) -> Value {
    json!({"id": id, "component": "Text", "text": text})
}

fn button(id: &str, label: &str, variant: &str, event: &str) -> [Value; 2] {
    let label_id = format!("{id}_label");
    [
        json!({"id": id, "component": "Button", "child": label_id, "variant": variant,
            "action": {"event": {"name": event}}}),
        text(&label_id, label),
    ]
}

/// The surface of the prompt "ui": a form that deploys a service. Its
/// Deploy button is disabled until the name is filled in, and sends the
/// event `deploy` with the name and the environment; Cancel sends
/// `cancel`, and Stale sends [`REFUSED_EVENT`].
pub fn form() -> Vec<Value> {
    let required = json!([{
        "condition": {"@call": "required", "args": {"value": {"@path": "/name"}}},
        "message": "Name the service to deploy",
    }]);
    let mut all = vec![
        json!({"id": "root", "component": "Card", "child": "form"}),
        json!({"id": "form", "component": "Column", "children":
            ["title", "hint", "name", "notes", "env", "notify", "replicas", "when", "actions"]}),
        text("title", "### Deploy a service"),
        json!({"id": "hint", "component": "Text", "variant": "caption",
            "text": "Rho waits for you to send this form."}),
        json!({"id": "name", "component": "TextField", "label": "Service",
            "placeholder": "api", "value": {"@path": "/name"}, "@checks": required}),
        json!({"id": "notes", "component": "TextField", "label": "Release notes",
            "variant": "longText", "value": {"@path": "/notes"}}),
        json!({"id": "env", "component": "ChoicePicker", "label": "Environment",
        "value": {"@path": "/env"}, "options": [
            {"label": "Staging", "value": "staging"},
            {"label": "Production", "value": "production"},
        ]}),
        json!({"id": "notify", "component": "CheckBox", "label": "Notify the channel",
            "value": {"@path": "/notify"}}),
        json!({"id": "replicas", "component": "Slider", "label": "Replicas",
            "min": 1, "max": 10, "value": {"@path": "/replicas"}}),
        json!({"id": "when", "component": "DateTimeInput", "label": "Not before",
            "enableDate": true, "value": {"@path": "/when"}}),
        json!({"id": "actions", "component": "Row", "justify": "end",
            "children": ["stale", "cancel", "deploy"]}),
        json!({"id": "deploy", "component": "Button", "child": "deploy_label",
        "variant": "primary", "@checks": required,
        "action": {"event": {"name": "deploy", "context": {
            "name": {"@path": "/name"},
            "env": {"@path": "/env"},
        }}}}),
        text("deploy_label", "Deploy"),
    ];
    all.extend(button("cancel", "Cancel", "default", "cancel"));
    all.extend(button("stale", "Stale", "borderless", REFUSED_EVENT));
    vec![
        create(FORM_SURFACE),
        components(FORM_SURFACE, &Value::Array(all)),
        data(
            FORM_SURFACE,
            &json!({"name": "", "notes": "", "env": ["staging"], "notify": true,
                "replicas": 3, "when": "2026-01-31"}),
        ),
    ]
}

/// The surface of the prompt "gallery": every component of the basic
/// catalog, all 18, with each input in each of its forms.
pub fn gallery(picture: &str) -> Vec<Value> {
    let picture = format!("data:image/png;base64,{picture}");
    let mut all = vec![
        json!({"id": "root", "component": "Column", "children":
            ["heading", "caption", "divider", "tabs"]}),
        text("heading", "## Component gallery"),
        json!({"id": "caption", "component": "Text", "variant": "caption",
            "text": "Every component of the A2UI basic catalog."}),
        json!({"id": "divider", "component": "Divider"}),
        json!({"id": "tabs", "component": "Tabs", "tabs": [
            {"title": "Inputs", "child": "inputs"},
            {"title": "Media", "child": "media"},
        ]}),
        json!({"id": "inputs", "component": "Card", "child": "inputs_column"}),
        json!({"id": "inputs_column", "component": "Column", "children": [
            "short", "number", "secret", "long", "date", "time", "moment", "agree",
            "one", "many", "chips", "volume", "buttons", "people",
        ]}),
    ];
    all.extend(gallery_inputs());
    all.extend(gallery_media(&picture));
    all.extend(button("primary", "Primary", "primary", "primary"));
    all.extend(button("plain", "Default", "default", "plain"));
    vec![
        create(GALLERY_SURFACE),
        components(GALLERY_SURFACE, &Value::Array(all)),
        data(
            GALLERY_SURFACE,
            &json!({
                "short": "", "number": "42", "secret": "hunter2",
                "long": "Line one\nLine two", "date": "2026-01-31", "time": "09:30",
                "moment": "2026-01-31T09:30", "agree": true, "one": ["m"],
                "many": ["tests", "docs"], "chips": ["rust"], "volume": 40,
                "people": [{"name": "Ada"}, {"name": "Grace"}],
            }),
        ),
    ]
}

/// The gallery's inputs, buttons, modal and templated list.
fn gallery_inputs() -> Vec<Value> {
    vec![
        json!({"id": "short", "component": "TextField", "label": "Short text",
        "placeholder": "Type here", "value": {"@path": "/short"}, "@checks": [{
            "condition": {"@call": "required", "args": {"value": {"@path": "/short"}}},
            "message": "Short text is required",
        }]}),
        json!({"id": "number", "component": "TextField", "label": "Number",
            "variant": "number", "value": {"@path": "/number"}}),
        json!({"id": "secret", "component": "TextField", "label": "Obscured",
            "variant": "obscured", "value": {"@path": "/secret"}}),
        json!({"id": "long", "component": "TextField", "label": "Long text",
            "variant": "longText", "value": {"@path": "/long"}}),
        json!({"id": "date", "component": "DateTimeInput", "label": "Date",
            "enableDate": true, "value": {"@path": "/date"}}),
        json!({"id": "time", "component": "DateTimeInput", "label": "Time",
            "enableTime": true, "value": {"@path": "/time"}}),
        json!({"id": "moment", "component": "DateTimeInput", "label": "Date and time",
            "enableDate": true, "enableTime": true, "value": {"@path": "/moment"}}),
        json!({"id": "agree", "component": "CheckBox", "label": "A checkbox",
            "value": {"@path": "/agree"}}),
        json!({"id": "one", "component": "ChoicePicker", "label": "Choose one",
        "value": {"@path": "/one"}, "options": [
            {"label": "Small", "value": "s"},
            {"label": "Medium", "value": "m"},
            {"label": "Large", "value": "l"},
        ]}),
        json!({"id": "many", "component": "ChoicePicker", "label": "Choose several",
        "variant": "multipleSelection", "value": {"@path": "/many"}, "options": [
            {"label": "Tests", "value": "tests"},
            {"label": "Docs", "value": "docs"},
            {"label": "Changelog", "value": "changelog"},
        ]}),
        json!({"id": "chips", "component": "ChoicePicker", "label": "Chips",
        "variant": "multipleSelection", "displayStyle": "chips",
        "value": {"@path": "/chips"}, "options": [
            {"label": "Rust", "value": "rust"},
            {"label": "Go", "value": "go"},
            {"label": "Zig", "value": "zig"},
        ]}),
        json!({"id": "volume", "component": "Slider", "label": "Slider",
            "min": 0, "max": 100, "value": {"@path": "/volume"}}),
        json!({"id": "buttons", "component": "Row", "align": "center",
            "children": ["primary", "plain", "link", "blocked", "modal"]}),
        json!({"id": "link", "component": "Button", "child": "link_label",
            "variant": "borderless", "action": {"functionCall": {
                "@call": "openUrl", "args": {"url": "https://a2ui.org"}}}}),
        text("link_label", "Borderless"),
        json!({"id": "blocked", "component": "Button", "child": "blocked_label",
        "action": {"event": {"name": "blocked"}}, "@checks": [{
            "condition": {"@call": "required", "args": {"value": {"@path": "/short"}}},
            "message": "Fill in the short text first",
        }]}),
        text("blocked_label", "Disabled"),
        json!({"id": "modal", "component": "Modal", "trigger": "modal_trigger",
            "content": "modal_content"}),
        json!({"id": "modal_trigger", "component": "Row",
            "children": ["modal_icon", "modal_label"]}),
        json!({"id": "modal_icon", "component": "Icon", "name": "info"}),
        text("modal_label", "Open a modal"),
        text("modal_content", "A modal's content, shown in a dialog."),
        json!({"id": "people", "component": "List",
            "children": {"componentId": "person", "path": "/people"}}),
        json!({"id": "person", "component": "Row", "align": "center",
            "children": ["person_icon", "person_name"]}),
        json!({"id": "person_icon", "component": "Icon", "name": "person"}),
        json!({"id": "person_name", "component": "Text", "text": {"@path": "name"}}),
    ]
}

/// The gallery's images, icons, vertical divider, video and audio.
fn gallery_media(picture: &str) -> Vec<Value> {
    vec![
        json!({"id": "media", "component": "Column", "children":
            ["picture_row", "video", "audio"]}),
        json!({"id": "picture_row", "component": "Row", "align": "center",
            "children": ["picture", "avatar", "star", "unknown_icon", "rule", "picture_note"]}),
        json!({"id": "picture", "component": "Image", "url": picture,
            "description": "A test chart", "fit": "contain", "variant": "smallFeature"}),
        json!({"id": "avatar", "component": "Image", "url": picture,
            "description": "An avatar", "fit": "cover", "variant": "avatar"}),
        json!({"id": "star", "component": "Icon", "name": "star"}),
        json!({"id": "unknown_icon", "component": "Icon", "name": "noSuchIcon"}),
        json!({"id": "rule", "component": "Divider", "axis": "vertical"}),
        json!({"id": "picture_note", "component": "Text", "variant": "caption",
            "text": "An image, an avatar, two icons and a vertical divider."}),
        json!({"id": "video", "component": "Video",
            "url": "https://example.com/demo.mp4"}),
        json!({"id": "audio", "component": "AudioPlayer",
            "url": "https://example.com/intro.mp3", "description": "Introduction"}),
    ]
}
