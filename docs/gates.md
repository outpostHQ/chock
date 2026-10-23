# The gates

`chock gates` prints this list from the binary: 69 gates, 39 on by default and 30 opt-in.
`chock gates --json` gives the same list to a program.

A gate is one of two kinds.

- A **pass/fail gate** trips on any problem.
- A **ratchet** keeps a number for each file or function in `.chock/baseline.json`. It holds new
  code to zero and trips when a recorded count grows. When a count shrinks, the next commit lowers
  the record. A count that the record does not hold trips when it is above zero.

[On by default](#on-by-default) · [Opt-in](#opt-in) · [Outpost gates](#outpost-gates) ·
[Proofs and models](#proofs-and-models) · [What a gate needs](#what-a-gate-needs) · [Records by machine and system](#records-by-machine-and-system) ·
[How `crap` counts](#how-crap-counts) · [What a gate leaves out](#what-a-gate-leaves-out)

## On by default

| gate | ratchet | trips when |
|---|:-:|---|
| `test` | | a test fails, or the crate has none, or the suite changes a file the repository holds; the run lists every failing test |
| `lint` | | rustfmt would change a file, or clippy warns; both run, and the run lists both |
| `doc` | | the documentation of a package builds with a warning; the run lists every such package |
| `deps` | | `cargo deny` finds an advisory, a disallowed licence or an unapproved source |
| `msrv` | | the crate stops building on the oldest Rust it declares |
| `crap` | | a function's complexity-times-uncoverage score goes up, or a new function scores over 30 |
| `commits` | | an unpushed commit's subject or a body line is too wide, its body repeats the diff, or a tool is credited as author |
| `profile` | | the release profile skips a free win or silences a compiler check |
| `toolchain` | | a `rust-toolchain` file names a channel that moves, such as `stable` or `nightly`, not a release or a dated nightly |
| `hygiene` | | the repository tracks a secret, a credential or build output |
| `wiring` | | a gate passes on this tree but is switched off, and is not opt-in |
| `modcheck` | ✓ | a `mod` names no file, or more files are reached by no `mod` |
| `manifest` | ✓ | more dependency declarations are unpinned |
| `placement` | ✓ | a path dependency is outside the repository or written as an absolute path, or a dependency is declared for every build when only tests use it |
| `features` | ✓ | more features are unused or undeclared |
| `sort` | ✓ | dependency tables fall further out of order |
| `dupdeps` | ✓ | one more crate is compiled twice, at two versions or feature sets |
| `unused` | ✓ | one more declared dependency is referred to nowhere |
| `supply` | ✓ | one more dependency runs code at build time |
| `typos` | ✓ | one more misspelling in code, comments or docs; a hex id of 7 characters or more, such as a commit id, is no word |
| `source` | ✓ | one more lint is allowed crate-wide or suppressed without a reason |
| `slop` | ✓ | one more comment block runs past two lines |
| `bigfiles` | ✓ | a file over 1000 production lines grows, or another one crosses 1000 |
| `splits` | ✓ | a file gains a part of 100 lines or more that only one private item uses |
| `lean` | ✓ | a file gains lines that one function, one table, one call or one loop over test cases would remove: code repeated in one shape, tests included, or a function that only passes its parameters on |
| `complexity` | ✓ | a function's cognitive complexity rises |
| `nesting` | ✓ | a function nests deeper, past four levels |
| `codeslop` | ✓ | one of clippy's code-shape lints fires more often, or fires for the first time |
| `duplication` | ✓ | one more function body is a copy, or close enough to merge |
| `coverage` | ✓ | a file gains lines no test executes; the run names every line of that file no test ran |
| `binsize` | ✓ | a release binary grows by more than 1% or 256 KiB, whichever is larger |
| `unsafety` | ✓ | a file gains an `unsafe` block, function, trait, impl or `extern` |
| `admits` | ✓ | a proof gains a step that nothing checks: `sorry`, `admit`, `native_decide` or `axiom` in Lean code; `assume(`, `admit(`, `assume_specification` or `verifier::external` in a Verus file; `kani::assume`; or a TLA+ `ASSUME`, `ASSUMPTION` or `AXIOM` that names no constant. `.chock/escapes.json` keeps one with its reason. Lake's `.lake` and `lakefile.lean` do not count, and a tree with no proof passes |
| `citations` | ✓ | a doc comment names one more path the repository does not have |
| `assertions` | ✓ | a test gains an assertion about *how many* instead of *what* |
| `testlint` | ✓ | a file gains a test that asserts nothing, cannot fail, returns before it checks, is skipped with no reason or has a name that states no claim |
| `claims` | ✓ | a document gains a citation of a line past the end of its file, or names as absent a thing that the tree holds |
| `pinned` | ✓ | a document's `test:<file>#<name>` pin names a test or a table row that the file no longer has, or a pin on record leaves the documents |
| `links` | ✓ | a `code:` comment in a TLA+ model or a Verus proof gains a link to an item the code does not have, or the code it links changes after the model last did |

## Opt-in

`chock enable GATE` switches one on. `chock disable GATE --reason "WHY"` switches a gate off and
records the reason under `left_off` in the config.

| gate | ratchet | trips when |
|---|:-:|---|
| `mutest` | ✓ | a mutation changes a decision and no test notices, one more time per file and operator; a run whose tests leave a file of the repository changed gives no verdict |
| `duplicates` | ✓ | the same job is written twice in different shapes, once more |
| `lines` | ✓ | the files the repository tracks hold more lines in total, blank lines included; the records under `.chock` do not count |
| `commands` | ✓ | a check you declared fails, or counts more than it held |
| `commands-build` | ✓ | the same, for your declared checks that need a compiler |
| `phrases` | ✓ | source gains a phrase your config forbids |
| `seams` | ✓ | a file or package uses a path or crate that your `seams` entry keeps to other places |
| `owners` | ✓ | a value that a call listed under `owners` reads, such as an environment variable, is read in one more file |
| `calls` | ✓ | a function makes a call listed under `calls` without the call it must come after or with |
| `dead` | | a private function nothing in the crate names |
| `acl` | | a dependency reaches an API its `cackle.toml` entry does not grant |
| `idempotent` | | the suite passes once and fails the second time |
| `proof` | ✓ | a Kani harness does not verify, a mutant does not make its harness fail, or a harness on record is gone |
| `laws` | ✓ | the text of a law changes or goes: a TLA+ theorem, a Verus `requires` or `ensures`, or a Kani harness's attributes and checks |
| `miri` | | a test fails under Miri; the run lists every failing test |
| `tla` | | TLC finds a fault in a model, a bad variant of a model passes, Apalache cannot prove the inductive invariant, or TLAPS does not prove a theorem |
| `verus` | | Verus does not verify a proof file, or verifies a mutant, a patch that puts a fault in one function |
| `unused-deep` | | a real (nightly) compile shows an unused dependency |
| `fmt` | | rustfmt would change a file; needs no compiler, so a commit can run it |
| `shellcheck` | ✓ | shellcheck has one more thing to say about a shell script the repository tracks, per file and check |
| `history` | | any commit holds a credential, not only the current tree |
| `unread` | | never; reports regions Outpost could not parse |
| `bsize` | | never; reports where a binary's bytes went |

Mutation testing is the slowest gate, about six minutes on chock itself. Every test of every
mutation runs in its own process, so one mutation that aborts cannot end the run.

## Outpost gates

These gates are opt-in too. They read one `outpost check`, which resolves references across the
whole tree.

| gate | ratchet | trips when one more… |
|---|:-:|---|
| `padding` | ✓ | stretch of shipped code is longer than the tree's own density explains |
| `boundaries` | ✓ | definition has callers spread past its directory, or directory holds more than one concern |
| `scan` | ✓ | known defect pattern appears, or untrusted input reaches a sink |
| `hazards` | ✓ | spot of syntax is about to be wrong, such as a value spliced into a command |
| `unreferenced` | ✓ | piece of shipped code is reached by nothing, or only by tests |
| `lenses` | ✓ | hazard lens stops reporting, or lens with no record reports a hazard |
| `measures` | ✓ | Outpost measure grows, stops arriving, or arrives above zero with no record |

## Proofs and models

Five gates check proofs: `tla` the TLA+ models, `verus` the Verus proofs, `proof` Kani harnesses,
`links` their ties to the code, and `laws` the text of each law. Only `links` is on by default; it
finds nothing with no model. `admits`, on by default too, counts the proof steps that nothing
checks.

```
   proofs/tla/M.tla ──── tla ────► TLC: M.cfg passes, and each bad variant fails
          │                        Apalache: IndInv holds in every state
          │                        TLAPS: each theorem is proved, and no proof is left out
          │
   proofs/verus/*.rs ─── verus ──► Verus: each proof verifies, and each mutant fails
          │
          ├───────────── links ──► each `code:` comment names one item of the code,
          │                        and the item did not change after the model's last commit
          │
          └───────────── laws ───► each theorem, contract and harness keeps its text on record
```

To start, switch the gates on, pin the tools, fetch them, then run the gates:

```sh
chock enable tla verus     # `links` is on already
chock init --local         # adds the TLC, Apalache and Verus pins to tool-versions.env
chock init --downloads     # fetches the pinned files of the gates that are on
chock run tla verus links
```

### `tla`

The gate reads each directory of `tla.dirs`, `proofs/tla` when left out. For a model `M.tla`, TLC
checks each cfg file whose name starts with `M`:

| cfg | TLC must |
|---|---|
| `M.cfg` | pass: each invariant and property of the model holds |
| `M_live.cfg` | pass: each liveness property holds |
| `M_reach.cfg` | fail. Its invariant says that a state the model guards is never reached, so the failure shows that TLC reaches it. Each model needs one. |
| any other `M_*.cfg` | fail with a violation: it is a bad variant of the model |

A line `\* expect: pass`, `\* expect: deadlock` or `\* expect: violation` in a cfg sets what TLC
must do with it. Each invariant of `M.cfg` must fail in at least one bad variant, except
`IndInv` and the invariants whose names start with `Type`. A conjunction is covered when each of
its parts fails in a variant.

Apalache checks the inductive invariant `IndInv` of each model whose `M.cfg` lists it. It proves
three things: `Init` makes `IndInv` true, one step keeps it true, and `IndInv` implies each
invariant. `tla.inductive` names the models and their invariants yourself, and `{}` proves none.
`tla.length` adds a bounded Apalache run of that many steps from `Init`. `tla.smt_encoding` sets
the SMT encoding of Apalache for a model, such as `{"OpHeads": "arrays"}`: `oopsla19`, `arrays`
or `funArrays`; the gate refuses another name.

`chock run tla --model=M,...` checks only the cfgs of the named models, and proves only the
theorems of their modules. A name with no cfg and no theorem stops the run with an error.

TLAPS proves each module that states a `THEOREM`, `LEMMA`, `PROPOSITION` or `COROLLARY`. The gate
trips on an obligation that no backend of tlapm proves, on an `OMITTED` proof, and on a step with
no proof, and it names the file and the line of each. `BY OBVIOUS` is a proof, because a backend
must prove it. chock pins tlapm as `TLAPM` in `tool-versions.env`, for Linux x86_64 and macOS
arm64. Where the pin has no file for the machine, or the project has no pin, the gate gives a note
and proves no theorem. `tla.timeout_s` and `tla.timeouts`, by the module's name, limit the proof
of each module, and `tla.workers` sets the threads of tlapm.

TLC needs Java 11 or later, and Apalache needs Java 21 or later. The gate uses
`$JAVA_HOME/bin/java`, else the `java` on `PATH`. TLC writes its states, and tlapm its cache,
under `target/test-scratch/chock/tla`.

### `verus`

The gate runs Verus on each file of `verus.files`, `proofs/verus/*.rs` when left out, and each file
must verify. A crate's `lib.rs` verifies its modules with it.

A mutant is a patch in `proofs/verus/mutants` that puts a fault in one function. Verus must fail
on each mutant. A mutant that verifies shows that the proof does not say what the function must
do.

```diff
--- ../../crates/k/src/cas.rs
+++ cas::put
@@ -2,3 +2,3 @@
 a
-b
+B
 c
```

Line 1 names the file, as a path from the directory that holds `mutants/`. Line 2 names the
function: `module::function`, or a bare name for a function in the crate root. The hunks follow.
chock patches a copy under `target/test-scratch/chock/verus`, never your file, and Verus checks
only the named function. A mutant that stops Verus before it verifies, such as one that does not
compile, refutes nothing, and the gate reports it.

Verus runs on the Rust toolchain that its release names. The `VERUS_SETUP` line of the pin
installs it, so `chock init --downloads` adds it. When that toolchain is not installed, the gate
names the `rustup install` command to run.

### `links`

A link comment ties a model to the code that it describes:

```
\* code: src/run/lock.rs::once
\* and Lock::release
Next == ...
```

In a Rust file under a `proofs` directory that holds a `verus!` block, the comment starts with
`// code:`. A link comment goes on over the comment lines after it, up to a line of code, an empty
comment or another label, such as `bad:`.

| you write | it names |
|---|---|
| `lock.rs::once` | the item `once` in the one source file whose path ends in `lock.rs` |
| `Lock::release` | the one item with that path in the code |
| `lock.rs` | the whole file |
| `code: none; why` | no code, on purpose; the text after `;` says why |

The gate counts a stale link for each of these:

- a comment that names no code: `unlinked`
- a link to no item, or to more than one item
- an item that changed after the model's last commit. Read the model against the code, change the
  model where it is wrong, then commit the model. Only a git repository shows this.

`links` is a ratchet: its first run records the stale links that the tree has.

### `laws`

`laws` pins the text of each law, so a weaker law cannot pass as the same one. A law is one of
these:

| law | its text |
|---|---|
| a `THEOREM`, `LEMMA`, `PROPOSITION` or `COROLLARY` at column 0 of a `.tla` file | the statement before its proof, and each definition at column 0 that the statement reaches |
| a function with `requires` or `ensures` in a Rust file with a `verus!` block | its signature, and each `spec fn` that the signature reaches |
| a function with `#[kani::proof]` or `#[kani::proof_for_contract]` | its `kani::` attributes, and each `assert!`, `kani::assert` and `kani::assume` in it |

chock takes out the comments and the blank lines and hashes the text. In TLA+, the indent of a line
stays, because a conjunction list reads by its indent. The key of a law is its file, its name and
its hash, such as `proofs/tla/M.tla#THEOREM Safe@9f…`. A change to a proof, or to the code that a
harness runs, keeps the key.

A law that changes gets a new key, so the gate trips twice: the new key is not on record, and the
old key is not measured. To accept the new law, a person runs `chock baseline laws`, and the
reviewer sees the change in `.chock/baseline.json`. The first run records the laws that the tree
has.

### Pins and downloads

`tool-versions.env` pins each file that a proof gate downloads, by its URL and its SHA-256:

```sh
TLA_TOOLS_VERSION=1.7.4
TLA_TOOLS_URL=https://github.com/tlaplus/tlaplus/releases/download/v${TLA_TOOLS_VERSION}/tla2tools.jar
TLA_TOOLS_SHA256=936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88
```

| key | what it says |
|---|---|
| `X_VERSION` | the version; the other keys can use it as `${X_VERSION}` |
| `X_URL`, `X_SHA256` | the file and its hash. chock refuses a file with another hash. |
| `X_URL_<system>`, `X_SHA256_<system>` | one file for each system, in words that name it, such as `x86_linux`, `x86_64_linux_gnu`, `arm64_macos`, `arm64_darwin` or `x86_win` |
| `X_SETUP` | a command that runs after the download |
| `X_GIT`, `X_REV` | a cargo tool that `chock init --global` builds from one commit of a repository, such as `CARGO_DENY_GIT` |

chock keeps the files in `CHOCK_TOOLS`, else in `chock-tools` in cargo's home, such as
`~/.cargo/chock-tools`. `chock init --global` fetches the files of the gates that are on, and
`chock init --downloads` fetches only those files.

## Notes on some gates

- **Names that sound alike.** `slop` measures comment length; `codeslop` counts clippy's code-shape
  lints. `unused` and `unused-deep` each find dependencies the other misses. `unreferenced` counts
  only what Outpost is certain nothing reaches; `measures` holds `dead_items_production`, a wider
  analysis, so a new public function with no caller trips `measures` and not `unreferenced`.
- **`test` reads the failures of a runner's own stage.** A `runner` can check cases that are no
  Rust test, such as a ledger of scenarios. Where no test failed, each line `FAIL <case> <why>` in
  its output is a finding at that case. A nextest line `FAIL [ 0.005s] ...` is a test, not a case.
- **How `commits` and `chock message` judge a message.** A subject is at most 72 characters wide, a
  body has at most 12 lines that are not blank, and a body line is at most 72 characters wide. The
  `message` keys `subject`, `body` and `width` set other limits. A line with no space, such as a
  URL, is never too wide.
- **When `bigfiles` and `boundaries` disagree.** Splitting a file for `bigfiles` can leave the new
  file calling helpers that stayed behind, and `boundaries` counts that edge. Split along a concern,
  and move what both halves need to where both can reach it.
- **`acl` asks for more grants than you might expect.** cackle pre-approves only the one-colon
  `cargo:` build-script instructions, so a crate printing `cargo::` needs its own grant, and it asks
  for proc-macro grants across the whole dependency graph.
- **How `lean` counts.** It reads each statement and match arm as a shape: names and literals
  are values, and keywords stay as written. It reads tests too. Production code, the tests in
  each `src` directory and the tests in each `tests` directory are apart: no group mixes them. A
  run of 3 lines and 30 tokens or more that repeats is a group. The finding names the merge:
  - one function with a parameter for each value that differs, for at most 6 values;
  - one table with a column for each value, for copies side by side, for at most 12 values;
  - a call to the function whose whole body is one copy;
  - one function that takes the one statement that differs as a closure;
  - one test that loops over a table of cases, for tests whose whole bodies repeat.

  The count is the lines the merge removes, less the lines that the function, its calls and its
  rows add. A call or a row wider than 88 columns costs a line for each value and 2 more, because
  rustfmt breaks it over lines. A copy can bind at most 3 names that later code reads, and the
  function returns them. A group counts only when it removes 6 lines or more. Each file holds its
  share of each group, and the lines of each private function that only passes its parameters on.
  A candidate is a note and never counts. Two arms with the same body are a candidate only where
  one arm with both patterns takes fewer lines, at rustfmt's default width of 100. A public
  function that only passes its parameters on is no candidate, since other modules may call it.
- **How `testlint` reads a test.** It reads each `#[test]` function in the files that a cargo
  target compiles. A file that no target reaches by `mod`, `#[path]` or `include!` is data that a
  harness reads, such as a UI fixture; it holds no test. Each finding names one of eleven rules:

  | rule | what it finds |
  |---|---|
  | `no-assertion` | a test that holds nothing that can fail it |
  | `early-return` | a test that returns before anything in it can fail, with a check after the return; a `return` in a closure, a nested function or an `async` block leaves only that |
  | `tautological-assertion` | `assert!(true)`, or `assert_eq!` with the same text on both sides |
  | `ignore-without-reason` | `#[ignore]` with no reason |
  | `should-panic-without-expected` | `#[should_panic]` with no `expected` text |
  | `placeholder-test-name` | a name such as `it_works`, `test_1` or `foo` |
  | `one-word-test-name` | a name of one word, which states no claim |
  | `ambiguous-test-name` | a name at a module path that a test in another file has too, so one failure line stands for both |
  | `escapes-the-run-dir` | a call of `env::temp_dir`, `set_current_dir`, `dirs::home_dir`, `dirs::data_dir` or `dirs::config_dir` in the body of the test |
  | `harness-without-pipefail` | a shell script in `bin/` or `scripts/` that pipes and does not set `pipefail`; a plain `sh` can refuse `pipefail`, so the finding on a `sh` or `dash` script asks for a bash shebang; a `case` pattern such as `a \| b)` is no pipe |
  | `waiver-without-reason` | a waiver that gives no reason; it silences nothing |

  A test asserts when its body holds an assert macro of any family, a panic macro, an `unwrap`,
  or a call of a function named `require…`, `expect…`, `verify…` or `must_…`. A test asserts
  through a helper too: it calls a helper that asserts, or gives the name of that helper to a
  runner as one argument. A helper is a function of the test code: a function in a `tests` or
  `benches` directory, in a `tests.rs` file, or below an item with `#[cfg(test)]`. A helper
  asserts when it holds an assert macro or a panic macro, or calls a helper that does. A
  production function is no helper.

  A test in a `proptest!` block takes generated arguments, written `name in strategy`. It claims
  that its body panics for no input, so `no-assertion` does not name it.

  `escapes-the-run-dir` reads the calls in the body of the test. It does not read what the test
  does with the path, and it does not read what a helper calls.

  A comment between the first attribute of a test and its closing brace leaves one rule out for
  that test: `// test-lint: allow(<rule>) — <reason>`. A comment above the first attribute is
  outside the test and waives nothing.

  `ambiguous-test-name` compares the failure line of each test of the tree, across crates: the
  modules below `src`, or below the file of a test target, and then the name. Two tests in two
  files with one failure line, such as `s3::round_trips` in two crates, are named, because no
  name filter runs one of them alone. One name in two modules of a crate is not named. A finding
  gives the number of tests with that failure line and lists at
  most 3 of them. A file with the comment `chock:modcheck-exempt` declares its modules with a macro, so
  each file in its directory counts as compiled.
- **How `claims` reads a document.** It reads each `.md` file of the tree. A fenced block claims
  nothing. A code span that names a file of the tree and a line is an anchor: one line, a range
  or a list. A span with a revision in it, such as `src/main.rs:12 @ 3f2a91c`, is no anchor: it
  cites the file as that revision has it. A comment states what the tree does not hold, and the gate checks it:

  ```md
  <!-- absent: Scheduler, fn retry_all, src/queue/legacy.rs -->
  <!-- absent-by-design: Scheduler — one process runs each gate -->
  <!-- doc-check: foreign -->
  ```

  | rule | what it finds |
  |---|---|
  | `stale-doc-anchor` | an anchor past the end of its file: the code that it named moved |
  | `absent-claim-refuted` | an `absent:` item that production code defines, or a path that the tree holds |
  | `unverifiable-absence-claim` | an `absent:` item that is neither a symbol nor a path |
  | `absence-waiver-without-reason` | an `absent-by-design:` comment with no reason |

  A path that starts at `src/`, `tests/`, `examples/` or `benches/` is written from a root. It
  names a file from the root of this tree, or from a directory above the document, such as the
  crate that holds the document. Any other path with a `/` names the one file that ends in it. A
  path that names no file of this tree is about another tree, and the gate does not check it.

  The text after `doc-check: foreign` describes another tree, up to `doc-check: ours`. There the
  gate checks only a path written from the root of this tree.
- **How `pinned` reads a pin.** A pin keeps a named test in place after the plan row that cited it
  is gone. It is a code span in a `.md` file of the tree: `test:`, a Rust file written from the
  root, `#` and a name. A fenced block shows syntax and pins nothing, so this list is an example:

  ```md
  ## Pinned
  - Q157: `test:crates/core/src/push.rs#a_lost_answer_lands_once`
  ```

  The pin holds while the file has one of these:
  - a function under a test attribute: `#[test]`, a runtime's `#[tokio::test]`, `#[rstest]`,
    `#[test_case]`, or an attribute whose name ends in `_test`, such as `#[repo_test]`. A test in a
    `proptest!` block counts too.
  - a string that opens a row of a table in test code, as in `("name", …)`. Test code is a file
    under `tests/` or `benches/`, a `tests.rs` file, or an item under `#[cfg(test)]`. Two rows can
    have the same name, and either one holds the pin.

  Each pin is one row of the record: 0 while it holds, 1 when its test is gone. A new pin is a note
  until `chock baseline pinned` records it. A recorded pin that leaves the documents trips the gate
  until `chock baseline pinned` records that change. So a commit that deletes a test and its pin
  also changes `.chock/baseline.json`, and a reviewer sees it.
- **`.chock/escapes.json` keeps a step that `admits` counts.** Each entry names a file, the
  line of the step and the reason it stays:

  ```json
  [{"path": "src/lib.rs", "text": "kani::assume(n < 9);", "reason": "bounds the loop"}]
  ```

  `text` is the whole line; chock trims its spaces. An entry keeps one step, so two equal lines in
  one file need two entries. An entry that keeps no step trips the gate, so a reason cannot outlive
  its line. An entry with a blank reason stops the gate.
- **How `proof` keeps each harness.** Kani lists the harnesses of `proof.packages`, or of the whole
  workspace, and verifies them in one run. Each harness is one row of the record, 0 when it
  verifies. A harness that fails trips the gate whatever the record holds, and chock records
  nothing from that run. A harness that leaves the code trips the gate until `chock baseline proof`
  records that change. A run past `proof.timeout_s` trips with every harness unproven.
- **`proof.require_cover` asks each harness for a cover.** When it is true, a harness with no
  `kani::cover!` trips the gate. A satisfied cover shows that some case gets past the harness's
  `kani::assume` calls. A cover that no case satisfies trips the gate whatever the setting.
- **How `proof` refutes a mutant.** A mutant is a patch in `proof.mutants`, `proofs/kani/mutants`
  when left out, that puts a fault in one file. Line 1 names the file, as a path from the project
  root. Line 2 names the harness that must fail, as Kani lists it, such as
  `+++ proofs::put_keeps_len`. chock patches a copy of the project under
  `target/test-scratch/chock/kani`, never your file, and Kani runs only that harness. The run
  builds in the project's target directory, so it reuses the build of the dependencies. A mutant
  that verifies, that stops Kani before it checks the harness, or that names a harness Kani does
  not list trips the gate. The mutants run only when every harness verifies. A path dependency
  outside the project root does not resolve in the copy.
- **`unused` and `dead` give leads.** Neither sees a dependency used only in a doc example, or a
  function reached only through a name a macro builds; mark such a function `#[expect(dead_code)]`.

## Why each gate starts its own compiler

One compiler process for every gate would not make a run faster, so chock does not build one.

- Rust has had no plugin interface since 1.75. A tool that reads the compiler's own data is a
  driver: a program that links one exact nightly and runs in place of `rustc`.
- `clippy`, `miri`, `mutest-rs` and `kani` are four drivers. Each one needs a different build of
  the same source, so they cannot share one compilation.
- Dylint shares one driver between lints only. `rustc_public`, the stable view of the compiler's
  data, is not stable yet.
- Most of a run is code that executes: tests, mutants, Miri and proofs. A shared compiler would
  save none of that time.

chock shares what can be shared: one target directory, one build for the gates that take the
same flags, and a kept verdict for each gate whose inputs did not change.

## What a gate needs

`chock init --global` installs the tools at the versions in `tool-versions.env`. `chock doctor`
says which of them this machine has. These gates need more:

| gate | also needs |
|---|---|
| `coverage`, `crap` | `rustup component add llvm-tools-preview` |
| `unused-deep`, `miri` | the `nightly` toolchain, and for `miri` its `miri` and `rust-src` components; `chock init --global` installs all three |
| `msrv` | the Rust release that `rust-version` in `Cargo.toml` names; `chock init --global` installs it in the crate it runs in |
| `mutest` | Outpost's fork of [mutest-rs](https://github.com/outpostHQ/mutest-rs) and the nightly it names; `chock init --global` builds the newest commit of the fork's `main`, which needs `git` |
| `proof` | `#[kani::proof]` functions; Linux or macOS |
| `tla` | Java 11 or later for TLC, and Java 21 or later for Apalache; the pinned jars and tlapm, which `chock init --downloads` fetches |
| `verus` | the pinned Verus, which `chock init --downloads` fetches, and the Rust toolchain that this Verus names, which its `VERUS_SETUP` line installs |
| `acl` | a `cackle.toml`, and bubblewrap; Linux only |
| `commits`, `hygiene`, `history` | a git or Outpost repository; outside one they report that they could not run |
| Outpost gates, `history`, `duplicates`, `unread` | [Outpost](https://github.com/outpostHQ) on `PATH`, one whose `check` reports the grouping measures by default (2026-10-18 on) |

On a system a gate's tool does not run on (`acl` off Linux, `proof` on Windows), `chock run` leaves
that gate out and says so; a Linux run enforces it.

## Write only some keys

`chock baseline GATE... --keys KEY...` writes only the named keys, for example after a file moved.
Each named key takes the number that the gate measures now. A key that the gate no longer measures
leaves the record. The other keys stay as they are.

The command never raises a key. When a named key measures more than its record, it writes nothing
and exits `1`. A key that no named gate measures or records gives exit `2`. `crap` keeps its own
record file, so `--keys` does not write it.

`chock baseline [GATE...] --moved OLD=NEW...` gives a renamed file the keys of its old path, on
every system's record. OLD must be gone from the tree and NEW must be a file. The command refuses
a NEW that the record already holds, so no total goes up. With no gate named, it moves the keys of
every gate. `crap` keeps its own record file, so `--moved` does not write it.

## Part of the suite

`chock run test --package=NAME` runs the suite of one package, and `--filter=TEXT` only the tests
whose name holds TEXT. The verdict of such a run answers for that part alone. Both work when the
`runner` is `cargo test` or `cargo nextest run`.

`chock run mutest --mutate=FILE,...` mutates only the named Rust files, and keeps the record for
every other file. Its verdict answers for those files alone.

## Output a finding points to

A finding quotes only a part of a long output, so some gates keep the whole of it under
`target/test-scratch/chock/`:

- `test.txt` and `idempotent.txt` hold the whole output of each suite run, under a line that
  counts the tests that passed, failed and were ignored. A run that chock stops at its memory
  limit or its deadline keeps what it printed too, and its CANNOT RUN reason gives the count.
- `coverage.txt` lists the lines no test ran, file by file. Keep it from a CI job and compare it
  with the list of your machine when the two counts differ.

## Records by machine and system

Coverage, mutation testing, binary size, clippy's shape lints and the Outpost gates measure
differently from machine to machine. A run reports their gains, and `chock baseline --lower GATE`
records them.

Coverage, mutation testing, binary size, `bsize` and `crap` also measure differently on each
system. macOS and Windows keep their own records, as `coverage@macos` and `coverage@windows`; Linux
keeps `coverage`. `crap` keeps its own file the same way, as `.chock/crap-baseline@macos.json`
beside `.chock/crap-baseline.json`.

A count can differ between two machines when code runs only where a tool is present. Compare the
two `coverage.txt` lists, and change the code or the tests so that the same lines run on both.

`coverage` leaves out the code that only tests compile: a `tests.rs` or `*_tests.rs` file, and the
lines of each `#[cfg(test)]` module. A test module counts the same in its parent file and in its
own `tests.rs`.

A test process that stops early can leave a cut-short `.profraw` profile. chock adds
`-failure-mode=all` to `LLVM_PROFDATA_FLAGS` for the coverage command, unless the variable already
names a failure mode, so llvm-profdata leaves such a profile out and merges the rest. chock reads
llvm-profdata's `warning: <path>.profraw: <reason>` lines from the command's stderr (the first 16
MiB). For each profile left out, `coverage` and `crap` give CANNOT RUN and name the profile. They
still show what the other profiles count, and no record is written from that run.

## How `crap` counts

`crap` counts the functions that score over 30. Its findings are each function whose score went up
and each new function over 30. A finding shows the function's complexity and coverage now and on
record, and says which of the two got worse. A pass says how many functions the record lacks,
because a rise in one of them passes until it is over 30; `chock baseline crap` records them, and
`chock baseline --lower crap` also records each score that went down.

A function that only got shorter is no finding. Its coverage percent falls when a line that a test
ran goes away, and so its score goes up. `crap` passes it where it has no more branches and no more
lines without a test than the record holds for it.

chock holds a function to the record by its file and its name. Where one file has two functions
with one name, such as two `impl From` blocks, chock pairs them in file order. Where their number
changed since the record, chock compares the scores from the worst down, so a pass means that no
score level holds more of them than the record does.

`crap` leaves out a file of test code: a `tests.rs`, a name that ends in `_tests.rs`, and each
file below a `tests` directory. No coverage row counts such a file, so cargo-crap would score each
of its functions as production code that no test ran.

`cargo crap` with no coverage file scores every function as if no test ran it, so it shows far more
functions than chock does. Give it the coverage file chock wrote to see the same scores:

```sh
cargo crap --lcov lcov.info --workspace
```

## What a gate leaves out

A gate leaves these out on purpose. Each one has a place that shows it.

| gate | what it leaves out | where you see it |
|---|---|---|
| `hazards`, `lenses` | an Outpost finding with the severity `note` | `outpost check` |
| `history` | a leak that `history.accepted` names in the config | the config entry, with its reason |
| `source` | a line that has `<tool>: ignore[<rule>] <why>` on it or on the line above | the comment, with its reason |
| `deps` | a `cargo deny` check that has no section in `deny.toml` | the run names each absent section |
| `unused` | a dependency that a source file of the crate names as a whole word, also in a comment or a string | `cargo machete`; `unused-deep` compiles and finds it |
| `supply` | a new version of a crate that the record already holds | the diff of `Cargo.lock` |
| `features` | a `.rs` file that does not parse and is outside `src/` or read by `include!` | the compiler, where a build uses the file |
| `placement` | a path dependency outside the tree, where the tree is in no repository | the `path` in `Cargo.toml` |
| `mutest` | on your machine, the files that the change did not touch. Where more than 2% of the mutations in the touched files time out and mutest did not confirm them by a re-run alone, the run mutates the whole crate | the run says which it did; CI mutates the whole crate |
| `mutest` | a function marked `#[cfg_attr(mutest, mutest::skip)]`, with a comment beside it that says why | the attribute in the source |
| `mutest` | a file that `not_mutated` in the config names, with its `why`, such as generated code | the config entry, with its reason |
| `mutest` | a mutation that times out counts as detected, for up to 2% of the mutations, or for any number once mutest re-ran each alone (`timeouts confirmed:`) | each timed-out mutation is a `candidate` finding at its file and line |
| `lean` | a file that a tool wrote: a part of its path is `generated`, or one of its first 5 lines says `@generated`, `do not edit`, `automatically generated` or `auto-generated` | the path or the header of the file |
| `lean` | copies that differ in a string that a macro reads, except format strings with the same placeholders, and copies where code after them reads more than 3 names that they bind; a production function that only tests call; copies of the fields of one struct that a `derive` would write | `duplication`, which compares whole function bodies |
| `test` | a change to a `Cargo.lock`, to a file under `.chock`, or to a file a declared command names in `outputs`; with no repository, every change | `git status` |
| `crap` | a function with no coverage in a file that the coverage run did not build, such as a Kani proof or code for another target; a report that names no file cargo-crap scored stops the gate | the coverage report, which names no line of that file |
| every gate that walks the tree | a directory that holds its own `.git`, such as a nested repository | the `.git` in that directory |
| `shellcheck` | a file that neither ends in `.sh` or `.bash` nor starts with a `sh`, `bash`, `dash`, `ksh` or `busybox` shebang | the first line of the file |
| `testlint` | a rule that a test waives with `// test-lint: allow(<rule>) — <reason>`; a test that asserts only through a production function is a finding | the comment in the test, with its reason |
| `claims` | a fenced block; a citation with no `/` in its path; a file name that two files of the tree have; a name that only test code defines | the document |
| `pinned` | a fenced block; the text after `doc-check: foreign`; a `test:` span whose file does not end in `.rs` | the document |
| `modcheck` | the directory of a file that holds the text `chock:modcheck-exempt` | the comment in that file |
| `modcheck` | a `mod` name that a macro builds and no file has; a `.rs` file that does not parse and is outside `src/` or read by `include!` | the compiler, where a build uses it |

A tool can print more than the 16 MiB that chock keeps. A list that chock reads from such an output
ends with a finding that says the list is not complete. A count is not read from such an output:
the gate reports that it cannot run, and its reason says that the output was cut.
