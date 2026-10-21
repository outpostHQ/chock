---------------------------- MODULE BaselineWrite ----------------------------
\* Runs and `chock baseline` read .chock/baseline.json, change their own records, and write the
\* whole file back. A run lowers the record of a gate it measured; `chock baseline` records one.
\* Each writer here changes one key of its own, so a key that goes back is a lost record.
\* Next is what chock does: take the run lock, read the file, write it whole through a temporary
\* that is renamed over it, and let go. A writer can die at any step; the system then lets go of
\* its lock (RunLock.tla).
EXTENDS Integers

CONSTANTS
    \* @type: Int;
    NW,
    \* @type: Int;
    MaxCrash

VARIABLES
    \* @type: Int -> Str;
    pc,
    \* @type: Int -> Int;
    disk,
    \* @type: Bool;
    torn,
    \* @type: Int -> (Int -> Int);
    snap,
    \* @type: Int;
    holder,
    \* @type: Set(Int);
    written,
    \* @type: Int;
    crashes

vars == <<pc, disk, torn, snap, holder, written, crashes>>

Writers == 1..NW
None == 0
Ended == {"done", "dead"}

Init ==
    /\ pc = [w \in Writers |-> "start"]
    /\ disk = [k \in Writers |-> 0]
    /\ torn = FALSE
    /\ snap = [w \in Writers |-> [k \in Writers |-> 0]]
    /\ holder = None
    /\ written = {}
    /\ crashes = 0

\* code: src/cli.rs::locked_context (src/run/lock.rs::take)
Take(w, from, to) ==
    /\ pc[w] = from
    /\ holder = None
    /\ holder' = w
    /\ pc' = [pc EXCEPT ![w] = to]
    /\ UNCHANGED <<disk, torn, snap, written, crashes>>

\* code: src/cli.rs::locked_context, then src/cli.rs::context (document::read of the baseline)
Read(w, from, to) ==
    /\ pc[w] = from
    /\ ~torn
    /\ snap' = [snap EXCEPT ![w] = disk]
    /\ pc' = [pc EXCEPT ![w] = to]
    /\ UNCHANGED <<disk, torn, holder, written, crashes>>

\* code: src/run/report.rs::Settled::written and src/cli.rs::record_baseline (document::write; counted_unstaged reads git after it)
Write(w) ==
    /\ pc[w] = "read"
    /\ disk' = [snap[w] EXCEPT ![w] = 1]
    /\ written' = written \cup {w}
    /\ pc' = [pc EXCEPT ![w] = "wrote"]
    /\ UNCHANGED <<torn, snap, holder, crashes>>

\* code: src/run/lock.rs::Held::drop
Release(w) ==
    /\ pc[w] = "wrote"
    /\ holder' = None
    /\ pc' = [pc EXCEPT ![w] = "done"]
    /\ UNCHANGED <<disk, torn, snap, written, crashes>>

\* code: none; a writer dies at any step, and the system lets go of its lock
Crash(w) ==
    /\ pc[w] \notin Ended
    /\ crashes < MaxCrash
    /\ crashes' = crashes + 1
    /\ pc' = [pc EXCEPT ![w] = "dead"]
    /\ holder' = IF holder = w THEN None ELSE holder
    /\ UNCHANGED <<disk, torn, snap, written>>

Next ==
    \E w \in Writers :
        \/ Take(w, "start", "held")
        \/ Read(w, "held", "read")
        \/ Write(w)
        \/ Release(w)
        \/ Crash(w)

\* bad: chock 0.8.0 reads the file before it takes the lock. A writer that holds the lock in
\* between writes its record, and the first writer then writes the file it read over it.
NextReadFirst ==
    \E w \in Writers :
        \/ Read(w, "start", "readfirst")
        \/ Take(w, "readfirst", "read")
        \/ Write(w)
        \/ Release(w)
        \/ Crash(w)

\* bad: the file is cut and filled in place, so a writer that dies between leaves it torn.
Cut(w) ==
    /\ pc[w] = "read"
    /\ torn' = TRUE
    /\ pc' = [pc EXCEPT ![w] = "cut"]
    /\ UNCHANGED <<disk, snap, holder, written, crashes>>

Fill(w) ==
    /\ pc[w] = "cut"
    /\ torn' = FALSE
    /\ disk' = [snap[w] EXCEPT ![w] = 1]
    /\ written' = written \cup {w}
    /\ pc' = [pc EXCEPT ![w] = "wrote"]
    /\ UNCHANGED <<snap, holder, crashes>>

NextInPlace ==
    \E w \in Writers :
        \/ Take(w, "start", "held")
        \/ Read(w, "held", "read")
        \/ Cut(w)
        \/ Fill(w)
        \/ Release(w)
        \/ Crash(w)

----------------------------------------------------------------------------
TypeOK ==
    /\ pc \in [Writers -> {"start", "held", "readfirst", "read", "cut", "wrote"} \cup Ended]
    /\ disk \in [Writers -> {0, 1}]
    /\ torn \in BOOLEAN
    /\ snap \in [Writers -> [Writers -> {0, 1}]]
    /\ holder \in 0..NW
    /\ written \in SUBSET Writers
    /\ crashes \in 0..MaxCrash

\* Each record a writer wrote is still in the file.
NoLostRecord == \A w \in written : disk[w] = 1

\* A half-written file always has a live writer that fills it, so no run is left to read it.
NeverTorn == torn => \E w \in Writers : pc[w] = "cut"

Safe == NoLostRecord /\ NeverTorn

\* Inductive for Next: a writer that read the file holds the lock, so the file is what it read.
IndInv ==
    /\ TypeOK
    /\ Safe
    /\ ~torn
    /\ \A w \in Writers : pc[w] \notin {"readfirst", "cut"}
    /\ \A w \in Writers : pc[w] \in {"held", "read", "wrote"} <=> holder = w
    /\ \A w \in Writers : pc[w] = "read" => snap[w] = disk
    /\ \A k \in Writers : disk[k] = 1 => k \in written

\* Reach: false once a writer died and each other writer wrote. BaselineWrite_reach.cfg lists
\* it, so TLC must find that state, which shows Safe guards states that occur.
AllWrittenAfterCrash == ~(crashes > 0 /\ \A w \in Writers : pc[w] = "dead" \/ w \in written)

----------------------------------------------------------------------------
\* The proof, for any number of writers and of crashes: TLC checks a few writers, and TLAPS
\* proves that IndInv holds at the start, that each step of Next keeps it, and that it gives Safe.
ASSUME Counts == NW \in Nat /\ MaxCrash \in Nat

THEOREM InitHolds == Init => IndInv
    BY Counts DEF Init, IndInv, TypeOK, Safe, NoLostRecord, NeverTorn, Writers, None

THEOREM StepKeeps == IndInv /\ [Next]_vars => IndInv'
<1> SUFFICES ASSUME IndInv, [Next]_vars PROVE IndInv'
    OBVIOUS
<1> USE Counts DEF IndInv, TypeOK, Safe, NoLostRecord, NeverTorn, Writers, None, Ended
<1>1. ASSUME NEW w \in Writers, Take(w, "start", "held") PROVE IndInv'
    BY <1>1 DEF Take
<1>2. ASSUME NEW w \in Writers, Read(w, "held", "read") PROVE IndInv'
    BY <1>2 DEF Read
<1>3. ASSUME NEW w \in Writers, Write(w) PROVE IndInv'
    BY <1>3 DEF Write
<1>4. ASSUME NEW w \in Writers, Release(w) PROVE IndInv'
    BY <1>4 DEF Release
<1>5. ASSUME NEW w \in Writers, Crash(w) PROVE IndInv'
    BY <1>5 DEF Crash
<1>6. CASE UNCHANGED vars
    BY <1>6 DEF vars
<1> QED
    BY <1>1, <1>2, <1>3, <1>4, <1>5, <1>6 DEF Next

THEOREM GivesSafe == IndInv => Safe
    BY DEF IndInv
=============================================================================
