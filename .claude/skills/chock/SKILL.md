---
name: chock
description: Run this project's quality gates with `chock` and read the answer as JSON — `chock run --json` for every enforced gate or `chock run <gate>... --json` for named ones, `chock gates --json` for which gates exist and which are enforced, `chock explain <gate>` for the last run's findings without paying for the gates again, `chock baseline` only when a person asks to move a record (never to make a gate pass), `chock doctor` for whether this machine holds the pinned tool versions, `chock slop [PATH]` for over-long comment blocks. Use it before calling a change finished, after a gate trips and you need the file and line, when asked whether the tests, lints, docs, dependencies, spelling, complexity or coverage are clean, and instead of inventing a quality check of your own.
---

# chock entry points

`chock` is the whole interface. Every rule lives in the binary where it has unit tests; this file
names entry points and carries none of its own, so deleting it leaves every gate still running.

## Run the gates

    chock run --json              every enforced gate
    chock run <gate>... --json    only the named ones, in the order given

Exit codes, for the run and for each gate in it: `0` the gate ran and passed, `1` it ran and
tripped, `2` it could not run. A `2` is never a pass; `cannot_run_reason` says what stopped it.

## Ask what gates exist

    chock gates --json

Each row carries `gate`, `about`, `group`, `ratchet`, `stage`, `enabled` and `rerun`. Read it
rather than assume a list — the registry is the source of truth, and it changes.

## Read the last run again

    chock explain <gate>

The findings from the run already in `.chock/last-run.json`, without running anything again. With
no gate named it lists all the debt on record: a run reports what got worse, not what is held.

## What a finding carries

`file` and `message` always; `line`, `item`, `measured` and `baseline` where the gate has them.
Open the file, at the line when there is one. `places` names the other places the finding is
about, such as the copy of a duplicated body, and `fix` the change that clears it, where the gate
can name one. A finding with `grade: candidate` is a lead to judge: it never trips the gate.

Every gate report carries `rerun`: the narrowest command that runs that one gate again. A gate that
tripped adds `fix` where chock has advice for that kind of finding.

## The rest

    chock baseline [GATE...]      record what this tree measures now; only when a person asks,
                                  never to make a gate pass (see .chock/agents.md)
    chock doctor --json           does this machine hold the versions tool-versions.env pins
    chock slop [PATH] --json      comment blocks longer than the limit, in any tree
