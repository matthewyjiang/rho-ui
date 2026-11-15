# a2ui

rho's A2UI v1.0 core. It has no GUI code and depends on no rho crate. It is tested against the conformance suites Google publishes with A2UI, vendored unmodified under `vendor/a2ui`.

So far it holds:

- `Pointer` (a normalized JSON Pointer), `resolve_path`, and `DataModel` with pull-based watchers.
- `Expr`, a dynamic value parsed from its wire form (`Expr::from_json`) or from a `formatString` template (`parse_template`).
- `DataContext`, which evaluates an `Expr` against a `DataModel` in a scope, with the A2UI type coercions.
- `Functions::basic()`, the basic catalog's 14 functions. `regex` uses the A2UI regex dialect and its safety check. The formatters format as `en-US` does.
- `Catalog`, a loaded v1.0 catalog. `Catalog::from_json` checks a catalog document against the v1.0 rules: UAX #31 names, the reserved `Surface` component, and `$ref` targets only inside the catalog or `common_types.json`. `Catalog::validation_schema` builds the bundled JSON Schema, and `CatalogValidator` checks component instances and the function calls in them against it. The published `common_types.json`, `agent_to_renderer.json` and basic catalog are compiled into the crate, and the validator resolves only those, so loading and validation never read a file or the network. A catalog that declares no protocol version, or one other than 1.0, fails to load.

The message processor comes later; until then a call's `catalogId` is parsed but not resolved.

## Vendored files

`vendor/a2ui/FILES` lists the upstream paths, `vendor/a2ui/UPSTREAM` the commit they come from. To refresh them, or after adding a path to `FILES`, run this against a checkout of [google/A2UI](https://github.com/google/A2UI):

```sh
crates/a2ui/scripts/vendor-a2ui.sh <upstream-checkout>
```

Never edit a vendored file by hand.

## Conformance

`tests/conformance.rs` runs each vendored suite as one test.

| Suite | Status |
| --- | --- |
| `core/data_model.yaml` | all 52 cases pass |
| `core/data_context.yaml` | all 11 cases pass |
| `core/expressions.yaml` | all 92 `parse_expression_template` cases pass; the 6 `validate` cases wait for the message processor |
| `core/functions.yaml` | all 93 `evaluate_function` cases without a surface or catalog pass; the 18 with one wait for catalogs and the message processor, and the 5 `validate` cases for the message processor |
| `core/catalog.yaml` | all 47 v1.0 cases pass; 26 pre-1.0 cases are left out |
| `core/common_types.yaml` | all 12 v1.0 cases pass; 10 pre-1.0 cases are left out |
| `core/agent_to_renderer.yaml` | the v1.0 case passes; 3 pre-1.0 cases are left out |
