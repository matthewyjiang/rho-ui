# AGENTS.md

rho-ui is the desktop app for rho. See docs/architecture.md for the crate layout, docs/features.md for behavior, and docs/testing.md for tests.

## Skills

Repo skills live in `.agents/skills`:

- `rust-best-practices`: load it before writing or changing any Rust here. Follow it.
- `rust-review`: use it to review Rust changes before you hand them back.
- `rust-port`: for porting code from another language to Rust.
- `rho-ui-test-selection`: load it before adding, growing, reviewing, or deleting tests. Every test needs a failure mode, one owner layer, and a gap. Ported from rho's `rho-test-selection`; edit it here.

The three Rust skills come from [BeautifulRust](https://github.com/morganlinton/BeautifulRust) (MIT, commit `edd3e86`). Update them by copying the skill folders again; don't edit them in place.

For UI work in `crates/rho-ui`, follow GPUI Kit's own guides: https://gpui-kit.com/docs/coding-guides.md and https://gpui-kit.com/docs/design-guides.md. Never invent a GPUI or GPUI Kit API. Check the source of the exact version in `Cargo.lock`.

Use GPUI Kit's components (`gpui_kit::component`, `gpui_kit::base`); don't build an element Kit already has. `gpui_kit` also re-exports raw GPUI: use that only for layout, or with a comment saying Kit lacks the element.

## Rules

- `rho-ui-core` has no GUI code. It talks to the rho daemon through `rho-protocol` and nothing else of rho's: never depend on `rho-sdk`. Its one Tokio runtime is for the daemon socket only, because `rho-protocol`'s client needs it. Keep everything else off it: the public API is plain futures that the UI awaits on GPUI's executors.
- `rho-protocol` comes from crates.io, released with rho. Don't work around something it lacks: say what is missing, so it can be added in rho.
- Lints are set once in the root `Cargo.toml` (`[workspace.lints]`, from BeautifulRust's `lints.toml`) and `clippy.toml`. Every crate opts in with `[lints] workspace = true`. Fix a lint rather than silencing it. When one is wrong for an item, use `#[expect(clippy::name, reason = "...")]` on that item.
- Nothing here is published to crates.io (`publish = false`).
- rho-ui has no release yet. Change its files, formats and settings freely: write no migrations and keep no compatibility code for old ones.
- rho-ui keeps the settings the user chooses in a text file (`settings.rs`, in the config directory), and everything it remembers between runs in one SQLite database (`store::StateDb`, schema in `store.rs`). New state gets a table there, or a `store::Key` in its `kv` table; never a file of its own. After any schema change, bump `SCHEMA_VERSION`: a database of another version starts over empty.
- Don't commit probes: measurement tests, timing scripts, one-off checks. Run them locally, put the numbers they give in the comment or commit message that needs them, and delete them before you hand back.
- Check UI changes with the headless UI tests in `crates/rho-ui/src/ui_tests.rs`, and look at the screenshots in `target/ui-shots/`. Don't drive the user's live desktop with `wtype` or screenshots of their screen: they may be using it.
- A UI test finds elements by `ElementId` (GPUI Kit controls register themselves; custom elements need `.test_support()`). Assert both what the user sees and the session state.

## Before you hand back

```bash
cargo fmt --all
cargo clippy -j 12 --all-targets -- -D warnings
cargo test -j 12
```

Send build and test output to a temp file and read only the parts you need.
