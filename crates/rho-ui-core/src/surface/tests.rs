use serde_json::{json, Value};

use super::*;
use crate::fake_daemon::surface::{form, gallery, FORM_SURFACE, GALLERY_SURFACE};

const CATALOG: &str = "https://a2ui.org/specification/v1_0/catalogs/basic/catalog.json";

fn create() -> Value {
    json!({"version": "1.0", "createSurface": {"surfaceId": "s", "catalogId": CATALOG}})
}

fn components(components: &Value) -> Value {
    json!({"version": "1.0", "updateComponents": {"surfaceId": "s", "components": components}})
}

fn text(text: &str) -> Value {
    components(&json!([{"id": "root", "component": "Text", "text": text}]))
}

fn placed(call: &str, surface: &str, messages: Vec<Value>) -> PlacedSurface {
    PlacedSurface {
        shown: SurfaceRef {
            call: call.to_owned(),
            surface: surface.to_owned(),
        },
        messages,
    }
}

/// A store shown `messages`, the call `call`'s, on surface "s".
fn store_of(messages: Vec<Value>) -> SurfaceStore {
    let mut store = SurfaceStore::default();
    store.show(placed("call", "s", messages));
    store
}

/// A surface is drawn under the latest call that showed it, and not at
/// all once deleted, unless to say the processor refused a message of
/// it. An action the daemon refuses makes it stale only while the call it
/// was sent from still draws it, and a call that shows it again makes it
/// current.
#[test]
fn a_surface_is_drawn_under_the_latest_call_that_showed_it() {
    let mut store = SurfaceStore::default();
    let delete = json!({"version": "1.0", "deleteSurface": {"surfaceId": "s"}});
    // (call, its messages, a refused action's call, drawn under, stale, refuses)
    let steps = [
        (
            "first",
            vec![create(), text("one")],
            None,
            Some("first"),
            false,
            false,
        ),
        (
            "second",
            vec![text("two")],
            Some("first"),
            Some("second"),
            false,
            false,
        ),
        (
            "third",
            vec![text("three")],
            Some("third"),
            Some("third"),
            true,
            false,
        ),
        (
            "fourth",
            vec![text("four")],
            None,
            Some("fourth"),
            false,
            false,
        ),
        ("fifth", vec![delete], None, None, false, false),
        (
            "sixth",
            vec![text("gone")],
            None,
            Some("sixth"),
            false,
            true,
        ),
    ];
    for (call, messages, refused_from, drawn_under, stale, refuses) in steps {
        store.show(placed(call, "s", messages));
        if let Some(from) = refused_from {
            let action = SurfaceAction {
                shown: SurfaceRef {
                    call: from.to_owned(),
                    surface: "s".to_owned(),
                },
                action: json!({}),
                data_model: None,
            };
            store.mark_stale(&action, "no such surface".to_owned());
        }
        assert_eq!(store.drawn_under("s"), drawn_under, "{call}");
        assert_eq!(store.stale("s").is_some(), stale, "{call}");
        assert_eq!(!store.refused("s").is_empty(), refuses, "{call}");
    }
}

/// Every component of the basic catalog reads into a node of its own: one
/// read as a placeholder would draw as one. A template draws its
/// component once per item, each in its own scope.
#[test]
fn the_gallery_reads_into_every_component() {
    let mut store = SurfaceStore::default();
    store.show(placed("call", GALLERY_SURFACE, gallery("AAAA")));
    let tree = store.tree(GALLERY_SURFACE).expect("the gallery has a root");
    let all: Vec<&Node> = tree.root.descendants().collect();

    let placeholders: Vec<_> = all
        .iter()
        .filter(|node| matches!(node, Node::Placeholder(_)))
        .collect();
    assert_eq!(placeholders, Vec::<&&Node>::new());

    let people: Vec<(Option<&str>, &str)> = all
        .iter()
        .filter_map(|node| match node {
            Node::Text(text) if text.target.component() == "person_name" => {
                Some((text.target.item(), text.text.as_str()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        people,
        [(Some("/people/0"), "Ada"), (Some("/people/1"), "Grace")]
    );
    let kinds: Vec<FieldKind> = all
        .iter()
        .filter_map(|node| match node {
            Node::Field(field) => Some(field.kind),
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        [
            FieldKind::Short,
            FieldKind::Number,
            FieldKind::Obscured,
            FieldKind::Long,
            FieldKind::Date,
            FieldKind::Time,
            FieldKind::DateTime,
        ]
    );
    let has = |wanted: fn(&Node) -> bool| all.iter().any(|node| wanted(node));
    assert!(has(|node| matches!(node, Node::Modal(Modal {
        trigger: Label { text, icon: Some(icon) },
        content: Some(_),
        ..
    }) if text == "Open a modal" && icon == "info")));
    assert!(has(
        |node| matches!(node, Node::Tabs(tabs) if tabs.tabs.len() == 2)
    ));
    assert!(has(|node| matches!(node, Node::Divider(Axis::Vertical))));
    assert!(has(|node| matches!(
        node,
        Node::Media(Media {
            kind: MediaKind::Video,
            ..
        })
    )));
    assert!(has(|node| matches!(
        node,
        Node::Media(Media {
            kind: MediaKind::Audio,
            ..
        })
    )));
    assert!(has(|node| matches!(
        node,
        Node::Media(Media {
            size: ImageSize::Avatar,
            fit: ImageFit::Cover,
            ..
        })
    )));
}

/// An input a template repeats writes to its own item: its binding is
/// the template's relative path resolved in the item's scope.
#[test]
fn a_templated_input_is_bound_to_its_item() {
    let store = store_of(vec![
        create(),
        components(&json!([
            {"id": "root", "component": "List",
                "children": {"componentId": "done", "path": "/todos"}},
            {"id": "done", "component": "CheckBox", "label": "Done",
                "value": {"@path": "done"}},
        ])),
        json!({"version": "1.0", "updateDataModel": {"surfaceId": "s",
            "value": {"todos": [{"done": false}, {"done": true}]}}}),
    ]);
    let tree = store.tree("s").expect("the list has a root");
    let bound: Vec<(String, bool)> = tree
        .root
        .descendants()
        .filter_map(|node| match node {
            Node::CheckBox(checkbox) => Some((
                checkbox.input.binding.as_ref()?.to_string(),
                checkbox.checked,
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        bound,
        [
            ("/todos/0/done".to_owned(), false),
            ("/todos/1/done".to_owned(), true)
        ]
    );
}

/// A template over a list longer than a surface draws stops at one
/// placeholder where it is cut off.
#[test]
fn a_surface_too_large_is_cut_off_once() {
    let items: Vec<Value> = (0..5_000).map(|item| json!({"n": item})).collect();
    let store = store_of(vec![
        create(),
        components(&json!([
            {"id": "root", "component": "List",
                "children": {"componentId": "item", "path": "/items"}},
            {"id": "item", "component": "Text", "text": {"@path": "n"}},
        ])),
        json!({"version": "1.0", "updateDataModel": {"surfaceId": "s",
            "value": {"items": &items}}}),
    ]);
    let tree = store.tree("s").expect("the list has a root");
    let Node::Stack(list) = &tree.root else {
        panic!("a list is a stack: {:?}", tree.root);
    };
    let cut: Vec<usize> = list
        .children
        .iter()
        .enumerate()
        .filter(|(_, child)| child.node == Node::Placeholder(Placeholder::Truncated))
        .map(|(ix, _)| ix)
        .collect();
    assert_eq!(cut, [list.children.len() - 1]);
    assert!(list.children.len() < items.len(), "{}", list.children.len());
}

/// A button whose check fails has no action to run. Once the user's edit
/// passes the check, pressing it gives the event to send, its context
/// read from the data model as edited, with the whole model attached for
/// a surface created with `sendDataModel`.
#[test]
fn a_button_sends_what_the_user_entered_once_its_checks_pass() {
    let mut store = SurfaceStore::default();
    store.show(placed("call", FORM_SURFACE, form()));
    let deploy = |store: &SurfaceStore| {
        let tree = store.tree(FORM_SURFACE).expect("the form has a root");
        let button = tree.root.descendants().find_map(|node| match node {
            Node::Button(button) if button.target.component() == "deploy" => Some(button.clone()),
            _ => None,
        });
        button.expect("the form has a Deploy button")
    };
    let blocked = deploy(&store);
    assert_eq!(blocked.action, None);
    assert_eq!(
        blocked.failures,
        [Failure {
            message: "Name the service to deploy".to_owned(),
            severity: Severity::Error,
        }]
    );

    let name = Pointer::parse("/name").unwrap();
    store.write(FORM_SURFACE, &name, json!("api"));
    let button = deploy(&store);
    assert_eq!(button.failures, []);
    let action = button.action.expect("the check passes");
    let acted = store.act(FORM_SURFACE, &button.target, &action);
    let sent = acted.send.expect("an event is sent to the session");
    assert_eq!(sent.surface(), FORM_SURFACE);
    assert_eq!(sent.action["action"]["name"], "deploy");
    assert_eq!(sent.action["action"]["sourceComponentId"], "deploy");
    assert_eq!(
        sent.action["action"]["context"],
        json!({"name": "api", "env": ["staging"]})
    );
    let model = sent
        .data_model
        .as_ref()
        .map(|model| &model["surfaces"][FORM_SURFACE]["name"]);
    assert_eq!(model, Some(&json!("api")));
    assert_eq!((acted.open, acted.error), (Vec::new(), None));
}

/// A date field reads only its kind of ISO 8601 moment, and writes a
/// moment back the way it reads one. A number field keeps text that is
/// not a finite number out of the data model.
#[test]
fn fields_take_only_values_of_their_kind() {
    let moments = [
        (FieldKind::Date, "2026-01-31", Some("2026-01-31")),
        (FieldKind::Date, "2026-1-31", None),
        (FieldKind::Date, "2026-02-30", None),
        (FieldKind::Time, "09:30", Some("09:30")),
        (FieldKind::Time, "09:30:15", Some("09:30:15")),
        (FieldKind::Time, "9:30", None),
        (
            FieldKind::DateTime,
            "2026-01-31T09:30",
            Some("2026-01-31T09:30"),
        ),
        (
            FieldKind::DateTime,
            "2026-01-31T09:30:00",
            Some("2026-01-31T09:30"),
        ),
        (FieldKind::DateTime, "2026-01-31", None),
    ];
    for (kind, text, iso) in moments {
        assert_eq!(
            kind.moment(text).map(Moment::to_iso).as_deref(),
            iso,
            "{kind:?} {text}"
        );
    }
    let numbers = [
        ("12.5", None),
        ("", None),
        ("12,5", Some(Problem::NotANumber)),
        ("1e999", Some(Problem::NotANumber)),
    ];
    for (text, problem) in numbers {
        assert_eq!(FieldKind::Number.problem(text), problem, "{text:?}");
    }
}
