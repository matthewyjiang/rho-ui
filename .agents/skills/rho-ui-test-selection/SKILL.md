---
name: rho-ui-test-selection
description: Choose, write, review, and delete rho-ui tests under the suite quality gate. Use when adding tests, expanding a suite, reviewing test-only changes, deleting low-value or flaky tests, deciding unit vs core vs headless UI coverage, or enforcing failure-mode and owner-layer rules.
compatibility: Applies to the rho-ui workspace (rho-ui-core, rho-ui) test layout. Ported from rho's rho-test-selection skill.
---

# rho-ui test selection

Every test must earn its place. Prefer deleting or merging weak tests over growing the suite.

Load this skill before adding a `#[test]`, before large test edits, and when reviewing test growth. `docs/testing.md` says how the suites run.

## Gate before you add a test

Answer all three:

1. **Failure mode** - What user-visible or contract bug does this catch if it fails?
2. **Owner layer** - Which single layer owns that failure mode (see below)?
3. **Gap** - What existing test does *not* already cover it?

If you cannot name a distinct failure mode and one owner layer, do not add the test. Extending an existing test with one assertion counts too: it needs the same three answers.

Put a short header on non-obvious new tests, in the doc comment the suite already uses:

```rust
/// A held question stops its countdown once, however many keys arrive
/// before the daemon answers.
#[test]
fn a_timed_question_holds_on_the_first_key() { ... }
```

## One failure mode, one owner layer

Pick the cheapest layer that still fails for the right reason. Do not cover the same behavior at more than one layer.

| Owner layer | Use for | Home |
| --- | --- | --- |
| Pure unit | Parsers, completion and prompt logic, timeline merging, time and size formatting, kinetic and layout math, store and settings round trips | `mod tests` in the file, or a sibling `*_tests.rs` with `#[path]` once it grows (see `rho-ui-core/src/session_tests.rs`) |
| Core contract | What `rho-ui-core` makes of the daemon: session state from protocol events, requests it sends, reconnects, subagents | `rho-ui-core` tests against `fake_daemon` (`tests/daemon.rs`, `session_tests.rs`) |
| Interactive UX | What the user sees and what the session does after clicks, keys, paste, resize, and window changes | headless UI tests in `crates/rho-ui/src/ui_tests.rs` |
| Process or real daemon | ssh launch, a real rho daemon and model | `rho-ui-core/tests/ssh.rs`; the `#[ignore]` live tests, run by hand |

Defaults:

- Interactive behavior defaults to a **headless UI test**. Find elements by `ElementId` and assert both what the user sees and the session state.
- Session logic that needs no window belongs in `rho-ui-core`, not in a UI test.
- Do not add a unit test, a core test, and a UI test for the same change.

## Keep vs reject

### Prefer (Tier A)

- Invariants that can regress silently: cancel, reconnect, resume, replay, tool call pairing, approval and question answers, ordering of streamed entries
- Protocol contracts with the daemon that break against a real rho
- UI flows: new session, prompt, approval, steer, interrupt, sidebar shelves, machine add and remove, settings that change behavior
- Pure parsers and policy as **table-driven** unit tests

### Keep thin (Tier B)

- Store schema and settings parsing: one round-trip or invalid-input test, cases in a table
- One structured test per rule, with cases in a table
- Platform or path edges

### Default reject (Tier C)

- Happy-path tests whose only failure mode is "someone deleted the feature"
- Another case of the same branch with different literals as a new function
- Rendered chrome: labels, badges, tooltips, copy, theme colors, pixel positions and alignment. Review those in the screenshots in `target/ui-shots/` instead
- `Default` / serde default checks that only restate derives
- Field-by-field load/save tests for each setting when one table would do
- Negative tests whose only purpose is to document removed behavior
- Static constants
- Private helper wiring already covered by a higher-layer test
- Duplicate coverage of the same branch at unit + core + UI
- Wall-clock sleeps used to synchronize, known flakes, and timing races (see Determinism)

## Determinism

Tests must be deterministic under parallel `cargo test` and a loaded machine.

### Reject

- `std::thread::sleep` or a timer used so "the other task can finish"
- Polling with fixed delays instead of an explicit condition
- Tests that fail under load or need retries or longer waits to pass
- Parking known flakes with `#[ignore]` without a rewrite or delete plan

### Allow

- Waiting on an explicit signal: a channel, an `Atomic*`, or a condition the code under test owns
- A timeout as a **failure bound** around that signal, not as the sync mechanism
- The UI harness's `wait_until` on an observable condition, with its named timeout
- The fake daemon's scripted replies and its counters (`holds()` and the like) instead of guessing when the daemon got a request
- Fake or manual clocks when the product logic is time-based

### If a test is flaky

1. Rewrite to event-driven or pure inputs when the failure mode still matters.
2. Delete when a deterministic test already covers it, or rewrite cost exceeds value.
3. Do not keep it by lengthening waits or ignoring it.

## How to write tests you keep

- Prefer one UI test that walks a real flow over several that each check one widget.
- One test function per **rule**; put sibling inputs in a table.
- Assert decisions, enums, session state, and whole values. Avoid `.contains("...")` on prose.
- String asserts are allowed only for wire-format contracts and escaping.
- Do not lock copy, tooltips, or labels behind string asserts. Review copy in the change. Test what it does instead: the element exists, the click lands, the session changes.
- Don't commit probes: measurement tests, timing scripts, one-off checks (see `AGENTS.md`).
- Avoid mutating the process environment; pass values in.
- When a change adds tests, remove or merge weaker tests nearby so the suite's weight stays flat or falls.

## Deleting and reviewing tests

When shrinking the suite or reviewing test growth, delete or demand changes for:

- Tier C tests and assertions without a distinct failure mode
- UI tests for logic that `rho-ui-core` or a unit test owns
- Near-duplicate functions instead of a table
- Copy, chrome, and pixel locks
- Sleep-synced or known-flaky tests
- Test growth without deleting or tightening anything nearby

Prefer one strong owner-layer test over the same branch covered three ways.
