//! Runs the vendored upstream conformance suites against the crate.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use a2ui::{
    parse_template, resolve_path, DataContext, DataModel, Expr, ExpressionError, FunctionError,
    Functions, Pointer, WatchId,
};
use serde_json::{json, Map, Value};

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

/// The cases `deferred` does not claim, after checking that it claims exactly
/// `want` of them, so that a vendor refresh that moves cases is noticed.
fn in_scope(
    suite: &str,
    cases: Vec<Value>,
    deferred: impl Fn(&Value) -> bool,
    want: usize,
) -> Vec<Value> {
    let (later, now): (Vec<Value>, Vec<Value>) = cases.into_iter().partition(|c| deferred(c));
    assert_eq!(later.len(), want, "{suite}: deferred case count changed");
    now
}

/// A case's error expectation, under either spelling the suites use.
fn expected_error(case: &Value) -> Option<&Value> {
    case.get("expect_error").or_else(|| case.get("expectError"))
}

#[test]
fn expressions_suite() {
    // `validate` cases render a surface, which needs the message processor.
    let cases = in_scope(
        "expressions",
        load_suite("expressions.yaml"),
        |case| case["action"] == "validate",
        6,
    );
    let mut failures = Vec::new();
    for case in cases {
        let name = case_name(&case);
        if is_skipped(name) {
            continue;
        }
        assert_eq!(
            case["action"], "parse_expression_template",
            "{name}: unknown action"
        );
        let input = case["input"].as_str().expect("input is a string");
        let outcome =
            parse_template(input).map(|parts| Value::Array(parts.iter().map(part_json).collect()));
        let verdict = match (outcome, expected_error(&case)) {
            (Ok(got), None) if got == case["expect"] => Ok(()),
            (Ok(got), None) => Err(format!("got {got}, want {}", case["expect"])),
            (Ok(got), Some(_)) => Err(format!("parsed to {got}, want an error")),
            (Err(e), None) => Err(format!("failed: {e}")),
            (Err(e), Some(want)) => check_parse_error(&e, want),
        };
        if let Err(reason) = verdict {
            failures.push(format!("{name}: {reason}"));
        }
    }
    assert_no_failures("expressions", &failures);
}

/// A template part in the suite's notation.
fn part_json(part: &Expr) -> Value {
    match part {
        Expr::Literal(value) => value.clone(),
        Expr::Path(path) => json!({ "path": path }),
        Expr::Call(call) => json!({
            "call": call.name,
            "args": call.args.iter().map(|(k, v)| (k.clone(), part_json(v))).collect::<Map<_, _>>(),
            "returnType": "any",
        }),
        Expr::List(_) | Expr::Object(_) => panic!("templates have no containers: {part:?}"),
    }
}

/// Checks a parse error against the suite's `ParseError` category and the
/// message phrase that names its cause.
fn check_parse_error(error: &ExpressionError, want: &Value) -> Result<(), String> {
    if want["category"] != "ParseError" {
        return Err(format!("unknown error category {}", want["category"]));
    }
    let phrase = parse_error_phrase(error);
    if want["message"].as_str() == phrase {
        Ok(())
    } else {
        Err(format!(
            "failed with {error:?} ({phrase:?}), want {}",
            want["message"]
        ))
    }
}

/// The phrase the suite's `expect_error.message` uses for each cause it
/// tests, so a case is matched by cause rather than by wording.
fn parse_error_phrase(error: &ExpressionError) -> Option<&'static str> {
    use ExpressionError::*;
    match error {
        UnexpectedCharacters { .. } => Some("Unexpected characters"),
        MissingArgumentColon { .. } => Some("after argument name"),
        UnclosedCall { .. } => Some("after function arguments"),
        UnclosedInterpolation => Some("Unclosed interpolation"),
        InvalidNumber { .. } => Some("Invalid number literal"),
        NumberOutOfRange { .. } => Some("out of range"),
        TooDeep => Some("Max recursion depth"),
        UnclosedString
        | InvalidPathEscape { .. }
        | TildeInFunctionName { .. }
        | InvalidSystemName
        | SystemNameWithoutCall { .. }
        | TemplateTooLong { .. }
        | TooManyParts
        | UnknownDirective { .. }
        | MalformedBinding
        | MalformedCall
        | InvalidPath(_)
        | UnknownFunction { .. }
        | IndexOutsideCollection
        | EvaluationTooDeep
        | Function { .. } => None,
    }
}

#[test]
fn functions_suite() {
    // `validate` cases render a surface, which needs the message processor.
    // A case naming a surface or a catalog goes through catalog resolution.
    let deferred = |case: &Value| {
        case["action"] == "validate"
            || ["surface", "catalogId", "catalogPaths", "catalog"]
                .iter()
                .any(|key| case.get(key).is_some())
    };
    let cases = in_scope("functions", load_suite("functions.yaml"), deferred, 23);
    let functions = Functions::basic();
    let mut failures = Vec::new();
    for case in cases {
        let name = case_name(&case);
        if is_skipped(name) {
            continue;
        }
        assert_eq!(
            case["action"], "evaluate_function",
            "{name}: unknown action"
        );
        if let Err(reason) = run_function_case(&case, &functions) {
            failures.push(format!("{name}: {reason}"));
        }
    }
    assert_no_failures("functions", &failures);
}

/// Calls the case's function with its arguments, at the root of its data.
fn run_function_case(case: &Value, functions: &Functions) -> Result<(), String> {
    let model = DataModel::new(case.get("dataModel").cloned().unwrap_or_else(|| json!({})));
    let call = json!({"@call": case["function"], "args": case.get("args").cloned().unwrap_or_else(|| json!({}))});
    let outcome =
        Expr::from_json(&call).and_then(|call| DataContext::new(&model, functions).evaluate(&call));
    match (outcome, expected_error(case)) {
        (Ok(got), None) => check_function_result(case, &got),
        (Ok(got), Some(_)) => Err(format!("returned {got}, want an error")),
        (Err(e), None) => Err(format!("failed: {e}")),
        (Err(e), Some(want)) => check_function_error(&e, want),
    }
}

/// Compares a result with `expect`.
///
/// Validators return a `ValidationResult`. A v1.0 case states the members it
/// checks, such as `{valid: false}`, and only those are compared. A case for
/// an earlier version, written when validators returned a boolean, states
/// that boolean, which is the result's `valid`.
fn check_function_result(case: &Value, got: &Value) -> Result<(), String> {
    let want = &case["expect"];
    let version = case
        .get("protocolVersion")
        .or_else(|| case.pointer("/catalog/protocolVersion"));
    let validity = got.get("valid").filter(|valid| valid.is_boolean());
    let matches = match (version.and_then(Value::as_str), want, validity) {
        (Some("1.0"), Value::Object(members), Some(_)) if members.contains_key("valid") => members
            .iter()
            .all(|(key, value)| got.get(key) == Some(value)),
        (Some("1.0"), _, _) => got == want,
        (_, Value::Bool(_), Some(valid)) => valid == want,
        _ => got == want,
    };
    if matches {
        Ok(())
    } else {
        Err(format!("got {got}, want {want}"))
    }
}

/// Checks an error against the suite's `ExpressionError` category and, when
/// the case names one, the regex dialect's diagnostic token.
fn check_function_error(error: &ExpressionError, want: &Value) -> Result<(), String> {
    if want["category"] != "ExpressionError" {
        return Err(format!(
            "failed with {error:?}, want a {}",
            want["category"]
        ));
    }
    let Some(message) = want.get("message") else {
        return Ok(());
    };
    let code = match error {
        ExpressionError::Function {
            source: FunctionError::Regex(regex),
            ..
        } => regex.code(),
        _ => None,
    };
    if message.as_str() == code {
        Ok(())
    } else {
        Err(format!("failed with {error:?}, want {message}"))
    }
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
