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

## Syncing settings

**Settings → Machines → Sync** copies rho's settings, model aliases and favorites, provider profiles, your hooks file and your plugins from one machine to another, once. It doesn't keep them in step afterwards.

- **What can't be copied.** A plugin syncs only when it came from git, since the target fetches it again with its own `git` and credentials, on the same branch and folder. A plugin copied from a folder or linked to one names a folder on the source machine, so it can't be copied: install it on the target from git, or link a folder there. A setting the target's rho doesn't have, or a value it doesn't take, such as a choice an older rho lacks, can't be copied either. Project plugins aren't synced.
- **Hooks come without their scripts.** The hooks file is copied whole, but the programs its hooks run aren't: put them on the target first. If a hook names a program by a full path the target doesn't have, the target refuses the file. Replacing the file also drops the target's own hooks.
- **The order it writes in.** Provider profiles go first, then model aliases and favorites, then rho's settings, then the hooks file, so nothing names a provider the target doesn't have yet. Each of these is written all at once. If one changed on the target since the page read it, the sync stops there: what was written before it stays, and Revert undoes it, but nothing after it is written, plugins included. Check what differs now and sync again. If the target refuses one for what it holds, its rows show why with Retry, and the rest still go. Plugins come last, each on its own, so one that fails to install shows Retry and the rest still land. Revert writes these parts in the reverse order, plugins still last.
- **Rows that start unchecked.** Something only the target has would be removed from it, so it syncs only if you check it. So does a setting that rho marks as belonging to one machine, such as the shell.
- **Older rho.** A target whose rho can't write provider profiles, models or the hooks file shows those rows as can't-copy. Update its rho to copy them.
- **A setting at its default** on the source resets it on the target, so the target follows its own default, unless that default differs.
- **Revert** puts back what the target had before the last sync to it, for 7 days after it. It first reads the target again. Anything that changed there since the sync is left as it is now, unless you check it. A plugin replaced since by a copy of a folder can't be reverted, since Revert couldn't bring it back.
