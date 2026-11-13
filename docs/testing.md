# Testing

```bash
cargo test
```

- `rho-ui-core` has a scripted daemon for tests (`fake_daemon`, behind the `test-support` feature): `rho-protocol`'s real server side on a real Unix socket, with no model or network. `tests/fixtures/bash_turn.jsonl` is a recorded turn from a real daemon.
- `rho-ui` UI tests (`crates/rho-ui/src/ui_tests.rs`) open the real app in an offscreen window, click and type through GPUI Kit's test helpers, and talk to the same fake daemon. They need a working Vulkan driver; Mesa lavapipe works without a GPU. Each run saves screenshots to `target/ui-shots/` for review.

To check rho-ui against a real daemon and model, install rho 3, sign it in, and run the `live_daemon` test. It starts the daemon, runs a turn through an approval, and saves screenshots. A new rho home starts in `bypass`, which asks for no approvals, so set `permission_mode = "supervised"` under `[behavior]` in its `config.toml`:

```bash
RHO_UI_LIVE_RHO=/path/to/rho cargo test -p rho-ui live_daemon -- --ignored --nocapture
```

Set `RHO_UI_LIVE_ATTACH` to the ID of a session whose turn waits on an approval, such as one you started in rho's terminal UI, and the test also opens that session, answers the approval, and steers the turn. Give that turn a command that runs for several seconds, such as `sleep 8`, so the steer arrives while it runs. Use a `RHO_HOME` of its own to keep the run away from your sessions.

`live_background` checks background work the same way: a turn starts two background processes and ends while they run, one is stopped from the list, and the other exits by itself; each time the daemon's own turn tells the model, and the test sends a message during one of those turns. It takes a few minutes:

```bash
RHO_UI_LIVE_RHO=/path/to/rho cargo test -p rho-ui live_background -- --ignored --nocapture
```
