# AGENTS.md

rho-ui is the desktop app for rho. See README.md for the crate layout.

## Skills

Repo skills live in `.agents/skills`:

- `rust-best-practices`: load it before writing or changing any Rust here. Follow it.
- `rust-review`: use it to review Rust changes before you hand them back.
- `rust-port`: for porting code from another language to Rust.

These three come from [BeautifulRust](https://github.com/morganlinton/BeautifulRust) (MIT, commit `edd3e86`). Update them by copying the skill folders again; don't edit them in place.

For UI work in `crates/rho-ui`, follow GPUI Kit's own guides: https://gpui-kit.com/docs/coding-guides.md and https://gpui-kit.com/docs/design-guides.md. Never invent a GPUI or GPUI Kit API. Check the source of the exact version in `Cargo.lock`.

Use GPUI Kit's components (`gpui_kit::component`, `gpui_kit::base`); don't build an element Kit already has. `gpui_kit` also re-exports raw GPUI: use that only for layout, or with a comment saying Kit lacks the element.

## Rules

- `rho-ui-core` has no GUI code and no async runtime. The UI drives it on GPUI's executors, and a future daemon will reuse it.
- Lints are set once in the root `Cargo.toml` (`[workspace.lints]`, from BeautifulRust's `lints.toml`) and `clippy.toml`. Every crate opts in with `[lints] workspace = true`. Fix a lint rather than silencing it. When one is wrong for an item, use `#[expect(clippy::name, reason = "...")]` on that item.
- Nothing here is published to crates.io (`publish = false`).
- Check UI changes with the headless UI tests in `crates/rho-ui/src/ui_tests.rs`, and look at the screenshots in `target/ui-shots/`. Don't drive the user's live desktop with `wtype` or screenshots of their screen: they may be using it.
- A UI test finds elements by `ElementId` (GPUI Kit controls register themselves; custom elements need `.test_support()`). Assert both what the user sees and the thread state.

## Before you hand back

```bash
cargo fmt --all
cargo clippy -j 12 --all-targets -- -D warnings
cargo test -j 12
```

Send build and test output to a temp file and read only the parts you need.
