<img src="assets/icon.svg" width="64" height="64" alt="">

# rho-ui

The desktop app for [rho](https://github.com/matthewyjiang/rho), built with [GPUI Kit](https://gpui-kit.com).

rho-ui runs `rho acp` as a child process and talks to it over the [Agent Client Protocol](https://agentclientprotocol.com) (ACP), version 2. rho stays a separate binary. Any agent that speaks ACP v2 works too, and features ACP lacks get fixed in rho upstream first.

## Run

```bash
cargo run -p rho-ui -- [DIR] [-- AGENT COMMAND...]
```

`DIR` is where new threads start; it defaults to the current directory. The agent command defaults to `rho acp`. To pick a model for the whole process:

```bash
cargo run -p rho-ui -- ~/code/project -- rho --provider cliproxyapi --model anthropic/claude-haiku-5-5 acp
```

On Linux you need a Wayland or X11 session with a working Vulkan driver. See the GPUI Kit [installation guide](https://gpui-kit.com/docs/installation/) for the system packages.

## Layout

```mermaid
flowchart TD
    UI["rho-ui: GPUI Kit views"] --> Core["rho-ui-core: agent process, ACP client, thread state"]
    Core -->|"stdio, ACP v2"| Rho["rho acp"]
    Rho --> Store["~/.rho/sessions"]
```

| Crate | Owns |
| --- | --- |
| `rho-ui-core` | Starting the agent, ACP requests, the event stream, and `Thread`, which turns `session/update` notifications into transcript rows. It has no GUI code and no async runtime: the caller drives the connection on its own executor, so a future daemon can reuse it unchanged. |
| `rho-ui` | Windows, views, and input. |

One `rho acp` process serves every thread; rho keeps sessions in a map, and a new session in a warm process takes about 20 ms. Sessions live in `~/.rho/sessions`, so they show up in the TUI and other ACP hosts as well.

Neither crate is published to crates.io.

## Test

```bash
cargo test
```

- `rho-ui-core` tests run `crates/rho-ui-core/tests/fake_agent.py`, a scripted ACP agent, so they need `python3` but no model or network. `tests/fixtures/bash_turn.jsonl` is a recorded turn from a real `rho acp`.
- `rho-ui` UI tests (`crates/rho-ui/src/ui_tests.rs`) open the real app in an offscreen window, click and type through GPUI Kit's test helpers, and talk to the same fake agent. They need a working Vulkan driver; Mesa lavapipe works without a GPU. Each run saves screenshots to `target/ui-shots/` for review.

`Cargo.lock` follows GPUI Kit 0.9.0's own lock file; its `test-support` feature does not resolve against newer versions of some shared crates.
