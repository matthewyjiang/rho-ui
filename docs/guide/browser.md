# In a browser

rho-ui also runs in a browser tab. `rho-ui serve` runs on the machine you want to reach and does there what a browser can't: open the daemon's socket, run programs, and start shells. The page works as the desktop app on that machine would. It shows that machine's sessions and your remote machines', the same views, terminal included, and the same settings and state, shared with the desktop app.

## Start it

From a checkout, build the web app and start the server:

```bash
cargo xtask web
cargo run -p rho-ui -- serve [--port PORT] [--bind ADDRESS]
```

`cargo xtask dist` builds one `rho-ui` with the web app inside it, so `rho-ui serve` needs nothing beside it.

The server listens on `127.0.0.1:8765` unless told otherwise. Open the URL it prints. The URL holds a token, and the server only talks to a page that has it.

## From another machine

Whoever reaches `rho-ui serve` can run anything as its user, which is why it listens on localhost. To use it from another machine, tunnel to it:

```bash
ssh -L 8765:127.0.0.1:8765 HOST
```

Then open the printed URL on your machine.

## What differs from the desktop

- `rho-ui serve` runs on Linux and macOS, not Windows.
- There is no desktop folder chooser. Adding a project browses from `~/` in the search, as on a remote machine.
- The composer has no attach button.
- On a headless server, the release binary still needs the desktop's `libxcb` and `libxkbcommon` libraries installed to start.
