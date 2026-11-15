# a2ui

rho's A2UI v1.0 core. It has no GUI code and depends on no rho crate. It is tested against the conformance suites Google publishes with A2UI, vendored unmodified under `vendor/a2ui`.

So far it holds:

- `Pointer` (a normalized JSON Pointer), `resolve_path`, and `DataModel` with pull-based watchers.
- `Expr`, a dynamic value parsed from its wire form (`Expr::from_json`) or from a `formatString` template (`parse_template`).
- `DataContext`, which evaluates an `Expr` against a `DataModel` in a scope, with the A2UI type coercions.
- `Functions::basic()`, the basic catalog's 14 functions. `regex` uses the A2UI regex dialect and its safety check. The formatters format as `en-US` does.
- `Catalog`, a loaded v1.0 catalog. `Catalog::from_json` checks a catalog document against the v1.0 rules: UAX #31 names, the reserved `Surface` component, and `$ref` targets only inside the catalog or `common_types.json`, where a local `#/$defs/X` the catalog does not define names the common type `X`, as validation bundles it. `Catalog::validation_schema` builds the bundled JSON Schema, and `CatalogValidator` checks component instances and the function calls in them against it. The published `common_types.json`, `agent_to_renderer.json` and basic catalog are compiled into the crate, and the validator resolves only those, so loading and validation never read a file or the network. A catalog that declares no protocol version, or one other than 1.0, fails to load.

- The wire messages: `AgentMessage`, parsed from what an agent sends (`parse_payload` takes one message, a list, or a `{"messages": [...]}` wrapper), and `RendererMessage`, written back to the wire with `to_json`. An `error` message carries its protocol `ErrorCode` and the JSON Pointer of what failed.
- `MessageProcessor`, the renderer side. It holds the supported catalogs, each with the `Functions` that implement it, and the surfaces. `process` applies a payload and returns `Processed`: the `Change`s a view acts on (surfaces created and deleted, components added, updated and removed by id, data writes with the `WatchId`s they fired) and the messages to send back. Every message is checked against the surface it joins before anything changes, so a rejected message changes nothing and yields its `error`. Catalogs resolve per component and per call, with no fallback: the catalog the item names, else the surface default, else `CATALOG_NOT_FOUND`. With `Validation::Check` it also checks schemas, composition (`allowedParents`, `allowedChildren`) and topology (root, dangling references, cycles, depth, orphans), by the rules of a `ValidationConfig`.
- `Surface` and `Component`: a component keeps its envelope (`@catalogId`, `@accessibility`, `@checks`, `@metadata`, and the bare v1.0 transition forms) apart from its properties, which are read once, by the catalog's schema, into child references, actions and `Expr`s.
- `MessageProcessor::resolve`, which evaluates a component's properties in a `Scope` (the root, or one item of a template's list) into a `ResolvedComponent`: values, `ChildRef`s with the scope to resolve each child in, actions, and the outcome of `@checks`. A binding that fails resolves to null and is reported in `errors`, as `EXPRESSION_ERROR`, or `CATALOG_ERROR` when its call's catalog cannot be resolved.
- `MessageProcessor::dispatch_action`, which turns a component's `Action` into an outbound `action` message, with the surface's data model when it was created with `sendDataModel`, or runs a local function call. A function that requires user activation runs only with `UserActivation::Present`.

The processor runs no function for the agent: it answers a well-formed `callRendererFunction` with a `rendererFunctionResponse` error, and ignores `agentFunctionResponse`, since it calls no agent function.

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
| `core/expressions.yaml` | all 92 `parse_expression_template` and 6 `validate` cases pass |
| `core/functions.yaml` | all 93 `evaluate_function` cases without a surface, the 17 v1.0 ones on a surface, and the 5 `validate` cases pass; 1 pre-1.0 case on a surface is left out |
| `core/catalog.yaml` | all 47 v1.0 cases pass; 26 pre-1.0 cases are left out |
| `core/common_types.yaml` | all 12 v1.0 cases pass; 10 pre-1.0 cases are left out |
| `core/agent_to_renderer.yaml` | the v1.0 case passes; 3 pre-1.0 cases are left out |
| `core/message_processor_v1_0.yaml` | 81 v1.0 cases pass; 1 pre-1.0 case is left out; 2 that ask for a v0.9 data model are skipped |
| `core/validator_v1_0.yaml` | all 49 cases pass |
| `core/error_responses.yaml` | all 23 v1.0 cases pass; 1 pre-1.0 case is left out |
| `core/data_deletion.yaml` | all 7 cases pass |
| `core/data_model_pointers.yaml` | all 7 cases pass |
| `core/reserved_keys.yaml` | all 10 cases pass |
| `core/composition_constraints.yaml` | all 10 cases pass |
| `core/index_function.yaml` | all 4 cases pass |
| `core/validation_result.yaml` | all 6 cases pass |
| `core/actions.yaml` | all 11 v1.0 cases pass; 7 cases without a version, which use v0.9 bindings, are left out |
| `core/multi_catalog.yaml` | all 8 cases pass |

A case's version is its own `protocolVersion`, else its catalogs', else the version in its suite's file name, else 0.9, the spec's default for a catalog that declares none. The harness builds each case's catalogs as Google's reference harnesses do: a catalog of another version is left out, since a v1.0 processor cannot hold one, and `basic`, and any catalog id a `createSurface` names that the case does not define, are the basic catalog under that id.
