# Remote machines

Add a machine in **Settings → Machines** by its SSH host, such as a `Host` in `~/.ssh/config`. rho-ui reaches it over ssh, starts its rho daemon if none runs, and shows its sessions beside this machine's. Its sessions, terminals, files and git all run there, in that machine's login environment.

![Sessions from two machines, and one that can't be reached](/shots/machines-listed.webp)

## Before you add one

**Run `ssh HOST` once in a terminal first.** It should sign in without asking anything, because rho-ui can't ask:

- **No passwords or passphrases.** rho-ui signs in with your key agent's keys and certificates, then with key files that have no passphrase. A key file with a passphrase is skipped, and the error names it: add it to your agent with `ssh-add`.
- **No host-key prompts.** The host's key must already be in `known_hosts`, which rho-ui never writes to. A host vouched for only by an `@cert-authority` line is refused, so add the host's own key.
- **No jump hosts yet.** Hosts reached through `ProxyJump` or `ProxyCommand` aren't supported.

rho-ui has its own ssh client, the same on every OS. It follows your ssh config as `ssh -G` reads it: host name, user, port, `IdentityFile`, `CertificateFile`, `IdentitiesOnly`, `IdentityAgent`, the `known_hosts` options and `RevokedHostKeys`. On Windows it uses OpenSSH's agent; Pageant isn't supported.

## What the machine needs

- **rho 3.1 or later**, whose `rho daemon status --json` names its socket. rho-ui offers to install rho where it's missing.
- **Unix socket forwarding in sshd.** `AllowStreamLocalForwarding no` or `DisableForwarding yes` in `sshd_config` blocks rho-ui.
- **A login shell that takes `-l -c`.** bash, zsh, fish and sh do; csh and tcsh may not.
- **Lingering under systemd.** Where `KillUserProcesses=yes`, the daemon dies with rho-ui's ssh connection. Run `loginctl enable-linger` there.
- **`RHO_HOME` where ssh sees it.** If you move rho's home, set `RHO_HOME` in a file a non-login shell reads, such as `~/.zshenv`.
- **Its own git credentials.** Cloning a project to a machine runs git there without prompts, so a private repository needs that machine's own key.

If `rho` isn't on the `PATH` of an ssh command there, set the machine's rho path in **Settings → Machines**.

## Connections

rho-ui keeps two ssh connections to each machine: one for its daemon and reads, one for terminals. Each stays open for 10 minutes after rho-ui last used it, so a reconnect doesn't ask your agent again. With 1Password's agent, you approve the key once for each.

A dead link, as when a laptop sleeps, shows within about 45 seconds. Open sessions stay open while their machine's connection comes and goes:

- **When the daemon restarts in place**, as it does to upgrade, open sessions reconnect by themselves. What you typed stays, and a message sent as it went is sent again once it's back. It runs once.
- **When the daemon stops or the link dies**, open sessions keep what they showed and wait for **Retry** in the sidebar. rho-ui never restarts a daemon by itself here, because it can't tell a restart from `rho daemon stop`.
