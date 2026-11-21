# Remote machines

rho-ui shows the sessions of several machines side by side: this one, and any you add by SSH host. It reaches each over ssh, starts rho's daemon there if none is running, and runs that machine's sessions in its own login environment. Terminals, files, diffs and pull requests all come from the session's own machine.

## Adding a machine

Open **Settings → Machines** and add the machine by its SSH host, such as a `Host` in `~/.ssh/config`. Before you do, run `ssh HOST` once in a terminal. It should sign in without asking anything: rho-ui can't type a password or passphrase, and can't ask you whether to trust a host key.

On the same page you can name each machine, set the path of its `rho` if an ssh command's `PATH` doesn't find it, and set the folder repositories get cloned into.

With more than one machine, each session row names its machine, and the button in the project list's footer picks the machine a new session runs on. A project on another machine offers **Clone to** that machine.

![Sessions from two machines, and one that can't be reached](/shots/machines-listed.webp)

A machine that can't be reached shows at the bottom of the sidebar with the reason and **Retry**. The others carry on.

## How rho-ui signs in

rho-ui has its own ssh client, the same on Linux, macOS and Windows. It follows your ssh config as `ssh -G` reads it: host name, user and port, key files and certificates, and `known_hosts`. It signs in with:

1. Your key agent's keys and certificates, the ones your config names first. That's `IdentityAgent`, else `SSH_AUTH_SOCK`, else OpenSSH's agent on Windows.
2. Key files without a passphrase, each with its certificate first.

A key file with a passphrase is skipped, and the error names it so you can add it to your agent with `ssh-add`. Hosts reached through `ProxyJump` or `ProxyCommand` aren't supported yet. Nor are hosts that `known_hosts` vouches for only with an `@cert-authority` line: add the host's own key.

With 1Password's agent you approve the key once per machine, and once more for its terminals. rho-ui keeps each connection for 10 minutes after it last used the machine, so a reconnect doesn't ask again.

## What a remote machine needs

- **rho 3.1 or later**, whose `rho daemon status --json` names its socket. rho-ui offers to install rho on a machine without it.
- **sshd must allow forwarding to a Unix socket.** `AllowStreamLocalForwarding no` or `DisableForwarding yes` in `sshd_config` blocks rho-ui.
- **A login shell that takes `-l -c`.** bash, zsh, fish and sh do; csh and tcsh may not.
- **On systemd with `KillUserProcesses=yes`**, the daemon dies with rho-ui's ssh connection. Run `loginctl enable-linger` there.
- **`RHO_HOME`**, if you move it, set where a non-login shell reads it, such as `~/.zshenv`.

## When the connection drops

Open sessions stay open while their machine's connection comes and goes.

- **The daemon restarts in place**, as it does to upgrade: each open session says it is reconnecting, then carries on. What you typed stays in the composer, and a message you sent as it went is sent again once it is back. It runs once.
- **The daemon stops, or the link dies**, as when a laptop sleeps: each session says the connection was lost and keeps what it showed. **Retry** in the sidebar picks them all up again. rho-ui never restarts a daemon by itself here, because it can't tell a restart from `rho daemon stop`.
