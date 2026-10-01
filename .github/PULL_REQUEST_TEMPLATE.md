**What this changes**

**Checks**

- [ ] `chock run` is green
- [ ] `chock doctor` matches `tool-versions.env`
- [ ] Comment blocks are at most two lines, and say why rather than what
- [ ] Tests assert content, not length, and are named as sentences

**If this adds or changes a gate**

- [ ] The rule is separated from the filesystem and the subprocess, so it is testable as a value
- [ ] `Err` means "could not run" and nothing else
- [ ] Run over at least one real repository, with the findings and the false-positive rate below

**If this moves a baseline**

Say which debt was accepted and why. A branch should not normally move `.chock/baseline.json`.
