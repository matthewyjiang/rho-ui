# a2ui

rho's A2UI v1.0 core. It has no GUI code and depends on no rho crate. It is tested against the conformance suites Google publishes with A2UI, vendored unmodified under `vendor/a2ui`.

So far it holds `Pointer` (a normalized JSON Pointer), `resolve_path`, and `DataModel` with pull-based watchers.

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
