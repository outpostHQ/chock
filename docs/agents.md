# Calling chock

```sh
chock run --json            every gate this project has on
chock run --fast --json     only the gates that need no compiler; seconds
chock run GATE... --json    only the gates you name
chock gates --json          what exists; read this, never hard-code a list
chock explain GATE          the last run's findings, without a new run
chock edited PATH...        what the commit would refuse in these files; no build
chock baseline GATE...      record where this tree is; read the rule below first
```

Exit codes: `0` every gate passed, `1` a gate tripped, `2` a gate could not run.

## `cannot_run` is not a pass

| verdict | what happened | what to do |
|---|---|---|
| `passed` | measured, no worse than the baseline | nothing |
| `tripped` | measured, worse than the baseline | fix the findings |
| `cannot_run` | nothing was measured | read `cannot_run_reason` and fix that first; run `fix` where the gate has one |

A `cannot_run` means a tool was missing, a file would not parse, or a command died. It says nothing
about the code. Reading it as success is the one mistake that makes chock worthless: the build stays
green while nothing is measured.

## Never move a baseline to make a gate pass

`chock baseline` records the current state as the new normal, and the ratchet stops protecting
anything. Raising a record is a deliberate commit, where a reviewer can see which debt was accepted
and why.

The record goes down by itself. A run that measures less says `lowered the record for …` on stderr
and leaves `.chock/baseline.json` modified: commit it with your change, or CI fails it.

## What a report gives you

Each entry in `gates` is one gate. `verdict` is its result, and `rerun` is the command for that one
gate: run it, not the whole set. A gate that tripped adds `fix`, how to repair that kind of
finding, where chock has advice. A gate that could not run because its tool is missing adds `fix`
too: the command that installs the tool.

Each entry in `findings` has `file`, relative to the project root, and `message`. Where the gate
has them it adds `line`, `item` (the function, lint or measure), and `measured` and `baseline`, the
number now and the number on record.

The findings are what got worse than the record, not all the debt. `measured` and `baseline` on the
gate count that debt, and `chock explain --json` lists every record, largest first.
