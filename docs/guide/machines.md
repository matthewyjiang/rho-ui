# Remote machines

rho-ui shows the sessions of several machines side by side: this one, and any you add by SSH host. It reaches each over ssh, starts rho's daemon there if none is running, and runs that machine's sessions in its own login environment. Terminals, files, diffs and pull requests all come from the session's own machine.

## Adding a machine

Open **Settings → Machines** and add the machine by its SSH host, such as a `Host` in `~/.ssh/config`. Before you do, run `ssh HOST` once in a terminal. It should sign in without asking anything: rho-ui can't type a password or passphrase, and can't ask you whether to trust a host key.

On the same page you can name each machine, set the path of its `rho` if an ssh command's `PATH` doesn't find it, and set the folder repositories get cloned into.

With more than one machine, each session row names its machine, and the button in the project list's footer picks the machine a new session runs on. A project on another machine offers **Clone to** that machine.

![Sessions from two machines, and one that can't be reached](/shots/machines-listed.webp)

A machine that can't be reached shows at the bottom of the sidebar with the reason and **Retry**. The others carry on.

::: details In detail

The sidebar lists every machine's sessions in one list, newest first, and with more than one machine each active row names its machine's label; a snoozed or settled row leaves it to the card its hover shows. The filter beside the search hides machines, or keeps to the projects you choose. A machine rho-ui can't reach shows at the bottom of the sidebar with ssh's or rho's reason and a Retry button; the others work on. A link that dies, as when the laptop sleeps or changes networks, shows there within about 45 seconds, when the ssh connection's keepalives give up. A login shell that hasn't printed its environment within 30 seconds fails the connection too.

A project is one repository, however many machines hold a checkout of it and whatever branch each is on. With more than one machine, the button in the project list's footer chooses the machine the session runs on: the list shows each project's checkout there, and a project with no checkout there shows where it is, with Clone to that machine: choosing it clones the repository from its origin remote into the machine's clone folder and starts the session in the clone. A project outside git, which has nothing to clone, shows dimmed, and choosing it looks for the same path on the chosen machine. Right-clicking either removes it. Each machine's clone folder is set on the Machines page; empty means `$RHO_HOME/projects` on that machine, and a relative path is from its home. git never prompts during a clone: a repository that needs a password, or a host key not yet known, fails with git's message, so the machine needs its own key for a private repository. Its ssh runs with `BatchMode`, unless on this machine you chose another ssh with `GIT_SSH` or git's `core.sshCommand`, such as PuTTY's `plink`, which rho-ui leaves as it is. A folder that already holds a clone of the same URL is taken as it is, once it has a commit; one a clone cut short left is refused as incomplete. Switching machines moves the draft to its project's checkout on the new one, else to that machine's home folder; a machine that isn't connected can't be chosen. Projects match by repository name without its owner, or by folder name outside git, so forks and same-named folders count as one. A typed path on a remote machine is browsed over the ssh connection to its daemon, once typing pauses; if its folders can't be read, the list says why and offers nothing to add. A file that would go by its path can't be attached to a message for a remote machine, and the refusal says why it would, such as its size over a limit: send text files and images there instead. A remote workspace's project name and branch come from running `git` there, over the ssh connection to its daemon, while it is connected; without `git` on that machine's `PATH` they show the folder's name and no branch. If its repositories can't be read, the sidebar's footer says why.

:::

## How rho-ui signs in

rho-ui has its own ssh client, the same on Linux, macOS and Windows. It follows your ssh config as `ssh -G` reads it: host name, user and port, key files and certificates, and `known_hosts`. It signs in with:

1. Your key agent's keys and certificates, the ones your config names first. That's `IdentityAgent`, else `SSH_AUTH_SOCK`, else OpenSSH's agent on Windows.
2. Key files without a passphrase, each with its certificate first.

A key file with a passphrase is skipped, and the error names it so you can add it to your agent with `ssh-add`. Hosts reached through `ProxyJump` or `ProxyCommand` aren't supported yet. Nor are hosts that `known_hosts` vouches for only with an `@cert-authority` line: add the host's own key.

With 1Password's agent you approve the key once per machine, and once more for its terminals. rho-ui keeps each connection for 10 minutes after it last used the machine, so a reconnect doesn't ask again.

::: details In detail

rho-ui shows the sessions of several machines side by side: this one, and any remote machine you add on the Machines page of Settings by its SSH host, such as a host in `~/.ssh/config`. rho-ui reaches a remote machine over ssh, starts its daemon there if none runs, and runs its sessions with that machine's login environment. It does so with an ssh client of its own, the same on Linux, macOS and Windows, which follows your ssh config as `ssh -G` reads it: the host's name, user and port, your key files and certificates (`IdentityFile`, `CertificateFile`, and `IdentitiesOnly`), your key agent (`IdentityAgent`, else `SSH_AUTH_SOCK`; on Windows, else OpenSSH's agent; Pageant isn't used), and your `known_hosts` files (`UserKnownHostsFile`, `GlobalKnownHostsFile`, `HostKeyAlias`) and `RevokedHostKeys`. A host you reach through `ProxyJump` or `ProxyCommand` can't be reached yet, and says so. rho-ui can't ask for a password or a passphrase: it signs in with your key agent's keys and certificates, those of your configured files first, then with key files that have no passphrase, each with its certificate (`<file>-cert.pub` beside it, or a `CertificateFile`) before the bare key, so user certificates, such as Teleport's, Vault's or step-ca's, sign in from the agent or from files; an RSA certificate signs in only from the agent. A key file that has a passphrase is skipped, and the error names it, to add to your agent with `ssh-add`; so is a key file rho-ui can't sign with, such as a FIDO (`sk-`) key, and one whose name uses `%i`. Nor can it ask whether to trust a host: the host's key must already be in `known_hosts`, which rho-ui never changes, so run `ssh HOST` once in a terminal to check and accept it. A key that differs from the one listed stops the connection, and the error names the file and line, as does one your `RevokedHostKeys` file lists; a `RevokedHostKeys` that isn't there or that rho-ui can't read (it reads a text list of keys, not a KRL) stops every connection, as OpenSSH does. rho-ui reads that file as a text list of keys; one it can't read, such as a binary KRL, stops the connection and says so. rho-ui doesn't check host certificates yet: a host your `known_hosts` vouches for only with an `@cert-authority` line is refused, and the error says so; add the host's own key to reach it. With 1Password's agent you approve the key once per machine, and once more for its terminals: rho-ui keeps those two ssh connections while it uses the machine, and each for 10 minutes after, so a retry or reconnect asks nothing. A machine you remove in Settings lets its connection go 10 minutes later. A remote terminal keeps the terminals' connection open while it runs. A remote machine whose `rho` isn't on the `PATH` of an ssh command gets its rho path set on the same page, as an absolute or `~/` path. Each machine also has a clone folder there, where the project list clones repositories. Each machine has a name, set there too (this machine's is `local` until you change it; a remote one shows its host until named).

:::

## What a remote machine needs

- **rho 3.1 or later**, whose `rho daemon status --json` names its socket. rho-ui offers to install rho on a machine without it.
- **sshd must allow forwarding to a Unix socket.** `AllowStreamLocalForwarding no` or `DisableForwarding yes` in `sshd_config` blocks rho-ui.
- **A login shell that takes `-l -c`.** bash, zsh, fish and sh do; csh and tcsh may not.
- **On systemd with `KillUserProcesses=yes`**, the daemon dies with rho-ui's ssh connection. Run `loginctl enable-linger` there.
- **`RHO_HOME`**, if you move it, set where a non-login shell reads it, such as `~/.zshenv`.

::: details In detail

A remote machine needs a few things of its server:

- sshd must allow forwarding to a Unix socket, since rho-ui reaches the daemon through one, as `ssh -W` does. `AllowStreamLocalForwarding no` or `DisableForwarding yes` in `sshd_config` refuses it.
- rho there must report its daemon's socket in `rho daemon status --json`, as rho 3.1 does: rho-ui connects to the socket it names.
- The login shell must take `-l -c`. bash, zsh, fish and sh do; csh and tcsh may not.
- rho-ui starts the daemon from its ssh connection. Where systemd-logind has `KillUserProcesses=yes`, the daemon dies with that connection: when rho-ui quits, the link drops, or 10 minutes after rho-ui last used the machine; run `loginctl enable-linger` there, or start the daemon some other way.
- The daemon's home comes from `RHO_HOME` as an ssh command sees it, not as a login shell does. Set it in a file a non-login shell reads, such as `~/.zshenv`, if you move it.

:::

## When the connection drops

Open sessions stay open while their machine's connection comes and goes.

- **The daemon restarts in place**, as it does to upgrade: each open session says it is reconnecting, then carries on. What you typed stays in the composer, and a message you sent as it went is sent again once it is back. It runs once.
- **The daemon stops, or the link dies**, as when a laptop sleeps: each session says the connection was lost and keeps what it showed. **Retry** in the sidebar picks them all up again. rho-ui never restarts a daemon by itself here, because it can't tell a restart from `rho daemon stop`.

::: details In detail

Open sessions stay open while their machine's connection comes and goes. A session the daemon stops sending, because rho-ui fell behind, picks up where it left off, keeping its scroll position and what you unfolded. When the daemon restarts in place, as it does to upgrade, each open session says it is reconnecting, and carries on once the daemon is back; what you typed stays in the composer, and a message you sent as the daemon went goes again once it is back, and runs once. A connection that ends any other way, as when the daemon was stopped or the link died, leaves each open session saying so, with what it showed and what you typed: Retry in the sidebar follows them again. A message you sent as it went goes back in the composer, as does one you stop before the daemon is back. rho-ui never reconnects by itself then, unless it expects a restart because an upgrade waits, as it can't tell a restart from `rho daemon stop`. A session that is gone once the daemon is back, as one another client deleted, or a new one whose first message the restart cut off, which rho saved nowhere, keeps what it showed and says why, and the message goes back in the composer.

:::
