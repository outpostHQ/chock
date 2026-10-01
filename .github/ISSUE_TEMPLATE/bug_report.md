---
name: Bug report
about: A gate reported something wrong, or chock itself misbehaved
labels: bug
---

**What happened**

**What you expected**

**The gate and its output**

```
chock run <gate> --json
```

Paste the JSON. If the gate tripped, `chock explain <gate>` names the findings.

**Your machine**

```
chock --version
chock doctor
```

`doctor` output matters: most surprising results are a tool at a version the project does not pin.

**A false positive?**

Say which rule fired and why the code is correct. A rule that fires on correct code is a defect —
the whole design rests on a gate people do not switch off.
