# In a browser

rho-ui also runs in a browser tab. `rho-ui serve` runs on the machine you want to reach and does there what a browser can't: open the daemon's socket, run programs, and start shells. The page works as the desktop app on that machine would. It shows that machine's sessions and your remote machines', the same views, terminal included, and the same settings and state, shared with the desktop app.

## Start it

A [release](./getting-started#from-a-release) for Linux or macOS has the web app inside it:

```bash
rho-ui serve [--port PORT] [--bind ADDRESS]
```

On macOS, that `rho-ui` is in the app: `/Applications/Rho.app/Contents/MacOS/rho-ui serve`.

From a checkout, build the web app first:

```bash
cargo xtask web
cargo run -p rho-ui -- serve [--port PORT] [--bind ADDRESS]
```

`cargo xtask dist` builds one `rho-ui` with the web app inside it, as a release is, so `rho-ui serve` needs nothing beside it.

The server listens on `127.0.0.1:8765` unless told otherwise. Open the URL it prints. The URL holds a token, and the server only talks to a page that has it.

## From another machine

Whoever reaches `rho-ui serve` can run anything as its user, which is why it listens on localhost. To use it from another machine, tunnel to it:

```bash
ssh -L 8765:127.0.0.1:8765 HOST
```

Then open the printed URL on your machine.

## What differs from the desktop

- `rho-ui serve` runs on Linux and macOS, not Windows.
- The composer has no attach button.
- On a Linux server without a desktop, `rho-ui` still needs the desktop's `libxcb`, `libxkbcommon` and `libxkbcommon-x11` libraries installed to start: `sudo apt install libxcb1 libxkbcommon0 libxkbcommon-x11-0` on Debian or Ubuntu, `sudo dnf install libxcb libxkbcommon libxkbcommon-x11` on Fedora.
