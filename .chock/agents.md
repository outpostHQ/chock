# Calling chock

```sh
chock run --json            every check this project has on
chock run --fast --json     only the checks that need no compiler; seconds
chock run <name>... --json  only the ones you name
chock gates --json          what exists; read this, never hard-code a list
chock explain <name>        the last run's findings, without running it again
chock edited <path>...      what the commit would refuse in these files; no build
chock baseline <name>...    record where this tree is; read the rule below first
```

Exit codes: `0` passed, `1` failed, `2` could not run.

## `cannot_run` is not a pass

| verdict | what happened | what to do |
|---|---|---|
| `passed` | measured, no worse than the baseline | nothing |
| `tripped` | measured, worse than the baseline | fix the findings |
| `cannot_run` | nothing was measured | read `cannot_run_reason` and fix that first |

A `cannot_run` means a tool was missing, a file would not parse, or a command died. It says nothing
about the code. Reading it as success is the one mistake that makes chock worthless: the build stays
green while nothing is checked.

## Never move a baseline to turn a check green

`chock baseline` records the current state as the new normal, and the ratchet stops protecting
anything. Raising a record is a deliberate commit, where a reviewer can see which debt was accepted
and why.

The record goes down by itself. A run that measures less says `lowered the record for …` on stderr
and leaves `.chock/baseline.json` modified: commit it with your change, or CI fails it.

## What a finding gives you

`file` and `line` are where to go, relative to the project root. `item` is the function, lint or
measure. `measured` and `baseline` are the number now and the number on record. `rerun` is the
command for that one check: run it, not the whole set.
