<!-- Partly managed by `outpost skills install`. -->

<!-- outpost:skills:start -->
## This repository is versioned with Outpost

_Managed by `outpost setup`; rewritten on upgrade._

Ask the graph first: `outpost search <name>` finds a definition, `outpost search callers <name>`
what calls it, `outpost graph impact <name>` what a change reaches, `outpost show <file> --lines
a:b` a range. The shell's `grep`, `cat` and `sed` are routed through Outpost. Exact edits in any
files, all or none: `outpost edit --plan -` with `{"edits":[{"path":"src/a.rs","find":"old",
"source":"new"}]}` on stdin; a rename, `outpost rewrite -p old -r new --write <paths>`. History
mirrors git: `outpost status`, `outpost diff --stat`, `outpost log`, `outpost commit -m`.
Playbooks, when needed: `outpost skills show <name>` for outpost-agents, outpost-basics, outpost-codebase-health, outpost-data-workflows, outpost-merge-conflicts, outpost-search, outpost-sync.
<!-- outpost:skills:end -->

Quality gates: read .chock/agents.md before running or reading chock.

## Order of work (owner, 2026-10-23)

For each feature and each bug, in this order:
1. Write the contract and the proof: the rule, its invariants, and a mutant that breaks the rule.
2. Write the smallest function that can run.
3. Run the real feature at once on real data, and read its output.
4. If the feature fails, find the cause, fix it, and run the same small feature check again.
   Never run the full suite to find out if a feature works.
5. When the feature works, write the tests. Then run the tests, proofs and lints for the
   changed code.
6. Run the required checks once, before the push. Passing tests never prove that a feature works.
