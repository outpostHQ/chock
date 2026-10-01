//! The text `chock --help` prints, and every refused command beneath its reason.

pub(crate) const USAGE: &str = "\
▰ chock — quality gates for Rust: new code must be clean, and existing debt can only go down.

  chock run [GATE...]     run the gates; no name means every one this project enabled
  chock run --fast        only the gates that answer without a compile
  chock gates             what gates exist, what each measures, which are on
  chock enable GATE...    switch gates on for this project
  chock disable GATE... [--reason WHY]  switch them off; the config records why
  chock stage GATE... STAGE  run them at commit, push, ci or manual (only `chock run`)
  chock explain GATE      the findings from the last run, without running again
  chock explain           all the debt the baseline holds, largest first, with each fix
  chock baseline [GATE...] accept today's numbers as the record, raising it where they grew
  chock baseline --lower GATE...  record a gain on a gate that measures differently by machine
  chock doctor            does this machine hold what this project pins?
  chock survey            what every gate can measure here; no config, no baseline, writes nothing
  chock message FILE      check one commit message; what the commit-msg hook runs
  chock slop [PATH]       comment blocks longer than the limit
  chock edited PATH...    every rule one file's own text answers; what the editor hook runs
  chock init --global     install the pinned tools, once per machine
  chock init --local      write this project's justfile, pins and config
  chock init              both; run it again after you install a newer chock

  --json                  machine-readable output, for an agent or a CI job

Exit codes: 0 the gate passed, 1 the gate tripped, 2 the gate could not run.
";
