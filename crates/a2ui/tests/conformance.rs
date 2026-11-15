//! Runs the vendored upstream conformance suites against the crate.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use a2ui::{resolve_path, DataModel, Pointer, WatchId};
use serde_json::{Map, Value};

/// Cases the harness cannot express in Rust, with the reason. A case here is
/// not run.
const SKIPPED: &[(&str, &str)] = &[];

fn load_suite(file: &str) -> Vec<Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("vendor/a2ui/conformance/core")
        .join(file);
    let text =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let suite: Value = serde_yaml_ng::from_str(&text)
        .unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()));
    match suite {
        Value::Array(cases) => cases,
        other => panic!("{} is not a list of cases: {other}", path.display()),
    }
}

fn case_name(case: &Value) -> &str {
    case["name"].as_str().expect("every case has a name")
}

fn assert_no_failures(suite: &str, failures: &[String]) {
    assert!(
        failures.is_empty(),
        "{suite}: {} failing:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

fn is_skipped(name: &str) -> bool {
    SKIPPED.iter().any(|(skipped, _)| *skipped == name)
}

#[test]
fn data_context_suite() {
    let mut failures = Vec::new();
    for case in load_suite("data_context.yaml") {
        let name = case_name(&case);
        if is_skipped(name) {
            continue;
        }
        assert_eq!(case["action"], "resolve_path", "{name}: unknown action");
        let args = &case["args"];
        let got = resolve_path(
            args["path"].as_str().expect("path argument"),
            args["contextPath"].as_str(),
        );
        if case["expect"] != got.as_str() {
            failures.push(format!("{name}: got {got:?}, want {}", case["expect"]));
        }
    }
    assert_no_failures("data_context", &failures);
}

#[test]
fn data_model_suite() {
    let mut failures = Vec::new();
    for case in load_suite("data_model.yaml") {
        let name = case_name(&case);
        if is_skipped(name) {
            continue;
        }
        if let Err(reason) = run_data_model_case(&case) {
            failures.push(format!("{name}: {reason}"));
        }
    }
    assert_no_failures("data_model", &failures);
}

/// Runs one case, stopping at its first failing step.
fn run_data_model_case(case: &Value) -> Result<(), String> {
    let initial = case
        .get("initial")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let mut model = DataModel::new(initial);

    let mut watched: HashMap<WatchId, Pointer> = HashMap::new();
    for path in case["watch"].as_array().into_iter().flatten() {
        let pointer = parse(path.as_str().ok_or("watch path is not a string")?)?;
        watched.insert(model.watch(&pointer), pointer);
    }

    for (n, step) in case["steps"]
        .as_array()
        .ok_or("no steps")?
        .iter()
        .enumerate()
    {
        run_step(&mut model, &watched, step).map_err(|reason| format!("step {n}: {reason}"))?;
    }
    Ok(())
}

fn parse(path: &str) -> Result<Pointer, String> {
    Pointer::parse(path).map_err(|e| e.to_string())
}

fn run_step(
    model: &mut DataModel,
    watched: &HashMap<WatchId, Pointer>,
    step: &Value,
) -> Result<(), String> {
    let op = step["op"].as_str().ok_or("step has no op")?;
    let path = step["path"].as_str().unwrap_or("/");
    let expects_error = step.get("expect_error").is_some();

    let fired = match op {
        "get" => {
            return check_get(model, step, path);
        }
        "set" | "delete" => {
            let value = if op == "set" {
                step["value"].clone()
            } else {
                Value::Null
            };
            let before = model.clone();
            let outcome = parse(path)
                .and_then(|pointer| model.set(&pointer, value).map_err(|e| e.to_string()));
            match (outcome, expects_error) {
                (Ok(fired), false) => fired,
                (Err(_), true) => {
                    return if before.get(&Pointer::root()) == model.get(&Pointer::root()) {
                        Ok(())
                    } else {
                        Err("model changed by a failed write".to_owned())
                    };
                }
                (Ok(_), true) => return Err(format!("{op} {path} succeeded, want an error")),
                (Err(e), false) => return Err(format!("{op} {path} failed: {e}")),
            }
        }
        "dispose" => {
            model.dispose();
            Vec::new()
        }
        // The suite hands `set` an object and then mutates it. Rust's `set`
        // takes the value by move, so no caller can reach committed state.
        "mutate_input" => Vec::new(),
        other => return Err(format!("unknown op {other:?}")),
    };

    if let Some(expected) = step.get("expect_notified") {
        check_notified(watched, &fired, expected)?;
    }
    if let Some(expected) = step.get("expect_values") {
        for (path, want) in expected.as_object().ok_or("expect_values is not a map")? {
            let got = model.get(&parse(path)?);
            if got != Some(want) {
                return Err(format!("{path} holds {got:?}, want {want}"));
            }
        }
    }
    Ok(())
}

fn check_get(model: &DataModel, step: &Value, path: &str) -> Result<(), String> {
    let pointer = match (parse(path), step.get("expect_error")) {
        (Ok(pointer), None) => pointer,
        (Err(_), Some(_)) => return Ok(()),
        (Ok(_), Some(_)) => return Err(format!("get {path} parsed, want an error")),
        (Err(e), None) => return Err(format!("get {path}: {e}")),
    };
    let got = model.get(&pointer);
    if step["expect_absent"] == true {
        // The suite counts a list slot left null by padding or removal as no
        // value. A key removed from an object must be gone.
        let in_list = pointer
            .parent()
            .is_some_and(|parent| model.get(&parent).is_some_and(Value::is_array));
        return match got {
            None => Ok(()),
            Some(Value::Null) if in_list => Ok(()),
            Some(value) => Err(format!("get {path} is {value}, want absent")),
        };
    }
    if let Some(kind) = step["expect_type"].as_str() {
        return match (kind, got) {
            ("list", Some(Value::Array(_))) | ("object", Some(Value::Object(_))) => Ok(()),
            _ => Err(format!("get {path} is {got:?}, want a {kind}")),
        };
    }
    let want = step.get("expect").ok_or("get step expects nothing")?;
    if got == Some(want) {
        Ok(())
    } else {
        Err(format!("get {path} is {got:?}, want {want}"))
    }
}

fn check_notified(
    watched: &HashMap<WatchId, Pointer>,
    fired: &[WatchId],
    expected: &Value,
) -> Result<(), String> {
    let mut got: Vec<String> = fired
        .iter()
        .map(|id| {
            watched
                .get(id)
                .map_or_else(|| format!("{id:?}"), ToString::to_string)
        })
        .collect();
    let mut want = expected
        .as_array()
        .ok_or("expect_notified is not a list")?
        .iter()
        .map(|path| parse(path.as_str().ok_or("path is not a string")?).map(|p| p.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    got.sort();
    want.sort();
    if got == want {
        Ok(())
    } else {
        Err(format!("notified {got:?}, want {want:?}"))
    }
}
