---
name: Gate or feature proposal
about: A check chock should make, or something it should do
labels: enhancement
---

**What should it catch**

**Is there a number?**

A ratchet needs a number that only goes down. A pass/fail gate needs a verdict with no legitimate
counter-case. If it is neither, say what it is instead.

**Does a tool already do this?**

chock orchestrates rather than reimplements. If a pinned binary can answer, wrapping it is usually
the right shape.

**False positives**

What correct code might this fire on? If you have run the idea over a real repository, the measured
rate is the most useful thing you can put in this issue.
