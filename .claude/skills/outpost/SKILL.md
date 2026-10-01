---
name: outpost
description: Outpost, this repository's version control: graph-first search, callers, impact, exact multi-file edits, history.
---

## This repository is versioned with Outpost

_Managed by `outpost setup`; rewritten on upgrade._

Ask the graph first: `outpost search <name>` finds a definition, `outpost search callers <name>`
what calls it, `outpost graph impact <name>` what a change reaches, `outpost show <file> --lines
a:b` a range. The shell's `grep`, `cat` and `sed` are routed through Outpost. Exact edits in any
files, all or none: `outpost edit --plan -` with `{"edits":[{"path":"src/a.rs","find":"old",
"source":"new"}]}` on stdin; a rename, `outpost rewrite -p old -r new --write <paths>`. History
mirrors git: `outpost status`, `outpost diff --stat`, `outpost log`, `outpost commit -m`.
Playbooks, when needed: `outpost skills show <name>` for outpost-agents, outpost-basics, outpost-codebase-health, outpost-data-workflows, outpost-merge-conflicts, outpost-search, outpost-sync.
