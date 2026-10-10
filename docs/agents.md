# Calling chock

```sh
chock run --json            every gate this project has on
chock run --fast --json     only the gates that need no compiler; seconds
chock run GATE... --json    only the gates you name
chock gates --json          what exists; read this, never hard-code a list
chock explain GATE          the last run's findings, without a new run
chock edited PATH...        what the commit would refuse in these files; no build
chock run --no-cache        judge every gate again; no kept verdict answers
chock baseline GATE...      raise a record to where this tree is; read the rule below first
chock lean [DIR] --json     every line this tree could lose, file by file; no record
chock oracle --old A --new B --corpus FILE --json
                            two builds over the same scenarios; each answer compared
chock sweep REV [PATH...]   proof that only comments changed since REV; exit 1 where code did
chock moved REV PATH...     proof that code only moved between these files since REV
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

A gate with no record gets its first one from `chock run`, which says `wrote the first record for
…`. No step needs `chock baseline` for that.

A gate entry with `"recalled": true` is a verdict that an earlier run took on the same files,
tools, config and record. It is as true as a new one.

## What a report gives you

Each entry in `gates` is one gate. `verdict` is its result, and `rerun` is the command for that one
gate: run it, not the whole set. A gate that tripped adds `fix`, how to repair that kind of
finding, where chock has advice. A gate that could not run because its tool is missing adds `fix`
too: the command that installs the tool.

Each entry in `findings` has `file`, relative to the project root, and `message`. Where the gate
has them it adds `line`, `item` (the function, lint or measure), and `measured` and `baseline`, the
number now and the number on record. It can add `places`, the other places the finding is about, each
with `role`, `file` and `line`, and `fix`, the change that clears that one finding.

A finding with `grade` set to `candidate` is a lead: the gate cannot settle it from the source, and
it never trips the gate. Judge each one before you change the code.

The findings are what got worse than the record, not all the debt. `measured` and `baseline` on the
gate count that debt, and `chock explain --json` lists every record, largest first.

## Cutting code that is already there

`chock lean --json` ranks the files by `removable_lines`. Each place has a `kind`, a `fix` and
`evidence`. `exact` is a fact that the source settles. `estimate` is a shape that a person must
judge: a trait with one implementation can be a test seam.

1. Run `chock lean --json` and take the files at the top.
2. Propose one change for each place, and wait until a person approves it.
3. Keep a build of the program from before the change. Apply the change and build again.
4. Run `chock oracle --old OLD --new NEW --corpus FILE --json`.
5. Run `chock run --json`.

The oracle exits `0` when each scenario answered the same, and `1` when one did not: the report
names the step, the field and the first line that differs. It exits `2` when nothing was compared,
which is not a pass. A difference is a fact about the change. Fix the change, or let a person
accept the difference in the `--allow` file with a reason.
