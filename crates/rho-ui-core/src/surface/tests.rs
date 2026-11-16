use serde_json::{json, Value};

use super::*;
use crate::fake_daemon::surface::{form, gallery, FORM_SURFACE, GALLERY_SURFACE};

const CATALOG: &str = "https://a2ui.org/specification/v1_0/catalogs/basic/catalog.json";

fn text(text: &str) -> Value {
    json!({"version": "1.0", "updateComponents": {
        "surfaceId": "s",
        "components": [{"id": "root", "component": "Text", "text": text}],
    }})
}

/// Every node of `node`, itself first, in drawing order.
fn nodes(node: &Node) -> Vec<&Node> {
    let mut all = vec![node];
    match node {
        Node::Row { children, .. } | Node::Column { children, .. } => {
            all.extend(children.iter().flat_map(|child| nodes(&child.node)));
        }
        Node::Card(child) => all.extend(nodes(child)),
        Node::Tabs { tabs, .. } => {
            all.extend(
                tabs.iter()
                    .filter_map(|(_, body)| body.as_ref())
                    .flat_map(nodes),
            );
        }
        Node::Modal { content, .. } => all.extend(content.iter().flat_map(|node| nodes(node))),
        _ => {}
    }
    all
}

/// A surface is drawn once, under the latest call that showed it, and not
/// at all once deleted; a call's repeated updates apply once; a refused
/// surface is current again when a call shows it again.
#[test]
fn a_surface_is_drawn_under_the_latest_call_that_showed_it() {
    let mut store = SurfaceStore::default();
    let create = json!({"version": "1.0", "createSurface": {
        "surfaceId": "s", "catalogId": CATALOG,
    }});
    let delete = json!({"version": "1.0", "deleteSurface": {"surfaceId": "s"}});
    let steps = [
        ("first", vec![create, text("one")], true, Some("first")),
        ("first", vec![text("repeat")], false, Some("first")),
        ("second", vec![text("two")], true, Some("second")),
        ("third", vec![delete], true, None),
    ];
    for (call, messages, applies, drawn_under) in steps {
        if call == "second" {
            store.mark_stale("s");
        }
        assert_eq!(store.apply(call, "s", &messages), applies, "{call}");
        assert_eq!(store.drawn_under("s"), drawn_under, "{call}");
        assert!(!store.is_stale("s"), "{call}");
    }
}

/// Every component of the basic catalog reads into a node of its own: one
/// read as a placeholder would draw as its name. A template draws its
/// component once per item, each in its own scope.
#[test]
fn the_gallery_reads_into_every_component() {
    let mut store = SurfaceStore::default();
    assert!(store.apply("call", GALLERY_SURFACE, &gallery("AAAA")));
    let tree = store.tree(GALLERY_SURFACE).expect("the gallery has a root");
    let all = nodes(&tree.root);

    let placeholders: Vec<_> = all
        .iter()
        .filter(|node| matches!(node, Node::Placeholder(_)))
        .collect();
    assert_eq!(placeholders, Vec::<&&Node>::new());

    let people: Vec<(String, &str)> = all
        .iter()
        .filter_map(|node| match node {
            Node::Text { target, text, .. } if target.component() == "person_name" => {
                Some((target.key(), text.as_str()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        people,
        [
            ("person_name@/people/0".to_owned(), "Ada"),
            ("person_name@/people/1".to_owned(), "Grace"),
        ]
    );
    let has = |wanted: fn(&Node) -> bool| all.iter().any(|node| wanted(node));
    assert!(has(
        |node| matches!(node, Node::Modal { label, icon: Some(icon), content: Some(_), .. }
        if label == "Open a modal" && icon == "info")
    ));
    assert!(has(
        |node| matches!(node, Node::Tabs { tabs, .. } if tabs.len() == 2)
    ));
    assert!(has(|node| matches!(node, Node::Divider { vertical: true })));
    assert!(has(|node| matches!(
        node,
        Node::Media {
            kind: MediaKind::Video,
            ..
        }
    )));
    assert!(has(|node| matches!(
        node,
        Node::Media {
            kind: MediaKind::Audio,
            ..
        }
    )));
    assert!(has(|node| matches!(
        node,
        Node::Media {
            size: ImageSize::Avatar,
            fit: ImageFit::Cover,
            ..
        }
    )));
}

/// Each bound input says where its value is kept, and what it takes: an
/// input without its binding could not be edited.
#[test]
fn inputs_keep_their_bindings_and_kinds() {
    let mut store = SurfaceStore::default();
    assert!(store.apply("call", GALLERY_SURFACE, &gallery("AAAA")));
    let tree = store.tree(GALLERY_SURFACE).expect("the gallery has a root");
    let all = nodes(&tree.root);

    let bound: Vec<(String, String)> = all
        .iter()
        .filter_map(|node| match node {
            Node::CheckBox {
                target, binding, ..
            }
            | Node::Field {
                target, binding, ..
            }
            | Node::Choices {
                target, binding, ..
            }
            | Node::Slider {
                target, binding, ..
            } => Some((
                target.key(),
                binding
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            )),
            _ => None,
        })
        .collect();
    let expected = [
        ("short", "/short"),
        ("number", "/number"),
        ("secret", "/secret"),
        ("long", "/long"),
        ("date", "/date"),
        ("time", "/time"),
        ("moment", "/moment"),
        ("agree", "/agree"),
        ("one", "/one"),
        ("many", "/many"),
        ("chips", "/chips"),
        ("volume", "/volume"),
    ]
    .map(|(target, binding)| (target.to_owned(), binding.to_owned()));
    assert_eq!(bound, expected);

    let kinds: Vec<FieldKind> = all
        .iter()
        .filter_map(|node| match node {
            Node::Field { kind, .. } => Some(*kind),
            _ => None,
        })
        .collect();
    let date_time = |date, time| FieldKind::DateTime { date, time };
    assert_eq!(
        kinds,
        [
            FieldKind::Short,
            FieldKind::Number,
            FieldKind::Obscured,
            FieldKind::Long,
            date_time(true, false),
            date_time(false, true),
            date_time(true, true),
        ]
    );
}

/// A button whose check fails has no action to run. Once the user's edit
/// passes the check, pressing it gives the event to send, its context
/// read from the data model as edited, with the whole model attached for
/// a surface created with `sendDataModel`.
#[test]
fn a_button_sends_what_the_user_entered_once_its_checks_pass() {
    let mut store = SurfaceStore::default();
    assert!(store.apply("call", FORM_SURFACE, &form()));
    let deploy = |store: &SurfaceStore| {
        let tree = store.tree(FORM_SURFACE).expect("the form has a root");
        nodes(&tree.root)
            .into_iter()
            .find_map(|node| match node {
                Node::Button {
                    target,
                    action,
                    failures,
                    ..
                } if target.component() == "deploy" => {
                    Some((target.clone(), action.clone(), failures.clone()))
                }
                _ => None,
            })
            .expect("the form has a Deploy button")
    };
    let (_, action, failures) = deploy(&store);
    assert_eq!(action, None);
    assert_eq!(
        failures,
        [Failure {
            message: "Name the service to deploy".to_owned(),
            blocks: true
        }]
    );

    let name = Pointer::parse("/name").unwrap();
    store.write(FORM_SURFACE, &name, json!("api"));
    let (target, action, failures) = deploy(&store);
    assert_eq!(failures, []);
    let acted = store.act(FORM_SURFACE, &target, &action.expect("the check passes"));
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

/// A date field keeps text that is not its kind of ISO 8601 moment out of
/// the data model, and writes a moment back the way it reads one.
#[test]
fn date_fields_take_iso_moments_of_their_kind() {
    let date_time = |date, time| FieldKind::DateTime { date, time };
    let cases = [
        (date_time(true, false), "2026-01-31", Some("2026-01-31")),
        (date_time(true, false), "2026-1-31", None),
        (date_time(true, false), "2026-02-30", None),
        (date_time(false, true), "09:30", Some("09:30")),
        (date_time(false, true), "09:30:15", Some("09:30:15")),
        (date_time(false, true), "9:30", None),
        (
            date_time(true, true),
            "2026-01-31T09:30",
            Some("2026-01-31T09:30"),
        ),
        (
            date_time(true, true),
            "2026-01-31T09:30:00",
            Some("2026-01-31T09:30"),
        ),
        (date_time(true, true), "2026-01-31", None),
        (
            date_time(false, false),
            "2026-01-31T09:30",
            Some("2026-01-31T09:30"),
        ),
        (FieldKind::Number, "1e3", None),
    ];
    for (kind, text, iso) in cases {
        assert_eq!(
            kind.moment(text).map(Moment::to_iso).as_deref(),
            iso,
            "{kind:?} {text}"
        );
    }
    let problems = [
        (FieldKind::Number, "12.5", false),
        (FieldKind::Number, "12,5", true),
        (FieldKind::Number, "", false),
        (FieldKind::Short, "anything", false),
        (date_time(true, false), "tomorrow", true),
        (date_time(true, false), "", false),
    ];
    for (kind, text, refused) in problems {
        assert_eq!(kind.problem(text).is_some(), refused, "{kind:?} {text:?}");
    }
}
