---
layout: home

hero:
  name: rho-ui
  text: The desktop app for rho
  tagline: Run rho's sessions in a window. They live in the rho daemon, so they keep going after you close it, and sessions from rho's terminal UI show up here too.
  image:
    src: /icon.svg
    alt: rho-ui
  actions:
    - theme: brand
      text: Get started
      link: /guide/getting-started
    - theme: alt
      text: GitHub
      link: https://github.com/matthewyjiang/rho-ui

features:
  - title: Sessions that outlive the window
    details: The rho daemon runs every turn. Quit rho-ui mid-turn and the work carries on; open it again and the session is where it got to.
  - title: Every machine in one list
    details: Add a remote machine by its SSH host. Its sessions sit beside this machine's, its terminals open there, and rho-ui starts its daemon for you.
  - title: Tools beside the chat
    details: A terminal, the session's files, its uncommitted diff, and its pull request with checks, threads and merge, in a panel next to the transcript.
  - title: Answers without leaving the keyboard
    details: Approvals, plan reviews and the agent's questions come up above the composer, and Enter answers them.
  - title: Worktrees per session
    details: Start a session in a fresh git worktree so parallel sessions in one repository never write the same files.
  - title: In a browser too
    details: "<code>rho-ui serve</code> runs the same app in a browser tab, with the same sessions, settings and terminal."
---

![A session in rho-ui: a turn's tool calls folded into one row, then the reply](/shots/work-steps.webp)
