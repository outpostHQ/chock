---------------------------- MODULE RunLock ----------------------------
\* One chock run per tree. A run takes .chock/run.lock before it measures, and lets go when it
\* ends. A run can die at any step; a run that finds the lock held waits and tries again.
\* Next is the lock chock keeps: the operating system's lock on an open file. The file stays,
\* and the system lets go of the lock when its run exits, a crash included.
EXTENDS Integers

CONSTANTS
    \* @type: Int;
    NR,
    \* @type: Int;
    MaxCrash

VARIABLES
    \* @type: Int -> Str;
    pc,
    \* @type: Int;
    path,
    \* @type: Int -> Int;
    writer,
    \* @type: Int -> Int;
    owner,
    \* @type: Int -> Int;
    fd,
    \* @type: Int;
    made,
    \* @type: Int;
    crashes

vars == <<pc, path, writer, owner, fd, made, crashes>>

Runs == 1..NR
\* Each file that ever stands at the path, by the order it was made; 0 is no file.
Files == 1..(2 * NR + 1)
None == 0
Ended == {"done", "dead", "refused"}

Init ==
    /\ pc = [r \in Runs |-> "idle"]
    /\ path = None
    /\ writer = [i \in Files |-> None]
    /\ owner = [i \in Files |-> None]
    /\ fd = [r \in Runs |-> None]
    /\ made = 0
    /\ crashes = 0

\* A new file at the path, holding the pid of run r.
Make(r) ==
    /\ made < 2 * NR + 1
    /\ made' = made + 1
    /\ path' = made + 1
    /\ writer' = [writer EXCEPT ![made + 1] = r]

\* code: none; a run dies at any step, and the system lets go of each lock its files held
Crash(r) ==
    /\ pc[r] \notin Ended
    /\ crashes < MaxCrash
    /\ crashes' = crashes + 1
    /\ pc' = [pc EXCEPT ![r] = "dead"]
    /\ owner' = [i \in Files |-> IF owner[i] = r THEN None ELSE owner[i]]
    /\ fd' = [fd EXCEPT ![r] = None]
    /\ UNCHANGED <<path, writer, made>>

----------------------------------------------------------------------------
\* The lock chock keeps: open the path, creating the file only where none stands, and ask the
\* system for the lock on it. The file is never removed.

\* code: src/run/lock.rs::once
Open(r) ==
    /\ pc[r] = "idle"
    /\ pc' = [pc EXCEPT ![r] = "opened"]
    /\ IF path = None
       THEN /\ made < 2 * NR + 1
            /\ made' = made + 1
            /\ path' = made + 1
            /\ fd' = [fd EXCEPT ![r] = made + 1]
       ELSE /\ fd' = [fd EXCEPT ![r] = path]
            /\ UNCHANGED <<path, made>>
    /\ UNCHANGED <<writer, owner, crashes>>

\* code: src/run/lock.rs::once (File::try_lock), then src/run/lock.rs::named
Lock(r) ==
    /\ pc[r] = "opened"
    /\ IF owner[fd[r]] = None
       THEN /\ owner' = [owner EXCEPT ![fd[r]] = r]
            /\ writer' = [writer EXCEPT ![fd[r]] = r]
            /\ pc' = [pc EXCEPT ![r] = "hold"]
            /\ UNCHANGED fd
       ELSE /\ pc' = [pc EXCEPT ![r] = "idle"]
            /\ fd' = [fd EXCEPT ![r] = None]
            /\ UNCHANGED <<owner, writer>>
    /\ UNCHANGED <<path, made, crashes>>

\* code: src/run/lock.rs::Held::drop
Release(r) ==
    /\ pc[r] = "hold"
    /\ owner' = [owner EXCEPT ![fd[r]] = None]
    /\ fd' = [fd EXCEPT ![r] = None]
    /\ pc' = [pc EXCEPT ![r] = "done"]
    /\ UNCHANGED <<path, writer, made, crashes>>

\* bad: the holder removes the file as it lets go. A run that opened the file before the remove
\* takes the lock of a file no path names, and a third run makes a new file and takes its lock.
ReleaseRemoving(r) ==
    /\ pc[r] = "hold"
    /\ path' = None
    /\ owner' = [owner EXCEPT ![fd[r]] = None]
    /\ fd' = [fd EXCEPT ![r] = None]
    /\ pc' = [pc EXCEPT ![r] = "done"]
    /\ UNCHANGED <<writer, made, crashes>>

Next == \E r \in Runs : Open(r) \/ Lock(r) \/ Release(r) \/ Crash(r)

NextDropRemoves == \E r \in Runs : Open(r) \/ Lock(r) \/ ReleaseRemoving(r) \/ Crash(r)

----------------------------------------------------------------------------
\* bad: the lock of chock 0.8.0. The file itself is the lock: a run makes it with create_new, a
\* run that finds it judges it stale when its writer has exited, removes it and makes it again,
\* and the holder removes it as it lets go. The age of a lock counts only when its writer has
\* stopped, which a dead writer has; a live one refreshes it well inside the deadline.

\* code: none; chock 0.8.0 judged this in its `stale`, which 0.9.0 removed
Stale(i) == i /= None /\ pc[writer[i]] = "dead"

\* code: none; chock 0.8.0 made the file in its `write_new`, which 0.9.0 removed
TryOld(r) ==
    /\ pc[r] = "idle"
    /\ IF path = None
       THEN Make(r) /\ pc' = [pc EXCEPT ![r] = "hold"]
       ELSE /\ pc' = [pc EXCEPT ![r] = "judge"]
            /\ UNCHANGED <<path, writer, made>>
    /\ UNCHANGED <<owner, fd, crashes>>

\* code: chock 0.8.0 src/run/lock.rs::once (the AlreadyExists arm)
JudgeOld(r) ==
    /\ pc[r] = "judge"
    /\ pc' = [pc EXCEPT ![r] = IF Stale(path) THEN "remove" ELSE "idle"]
    /\ UNCHANGED <<path, writer, owner, fd, made, crashes>>

\* code: chock 0.8.0 src/run/lock.rs::once (remove_file of the stale lock)
RemoveOld(r) ==
    /\ pc[r] = "remove"
    /\ path' = None
    /\ pc' = [pc EXCEPT ![r] = "retake"]
    /\ UNCHANGED <<writer, owner, fd, made, crashes>>

\* code: chock 0.8.0 src/run/lock.rs::once (write_new after the remove)
RetakeOld(r) ==
    /\ pc[r] = "retake"
    /\ IF path = None
       THEN Make(r) /\ pc' = [pc EXCEPT ![r] = "hold"]
       ELSE /\ pc' = [pc EXCEPT ![r] = "refused"]
            /\ UNCHANGED <<path, writer, made>>
    /\ UNCHANGED <<owner, fd, crashes>>

\* code: chock 0.8.0 src/run/lock.rs::Held::drop
ReleaseOld(r) ==
    /\ pc[r] = "hold"
    /\ path' = None
    /\ pc' = [pc EXCEPT ![r] = "done"]
    /\ UNCHANGED <<writer, owner, fd, made, crashes>>

NextOld ==
    \E r \in Runs :
        TryOld(r) \/ JudgeOld(r) \/ RemoveOld(r) \/ RetakeOld(r) \/ ReleaseOld(r) \/ Crash(r)

----------------------------------------------------------------------------
TypeOK ==
    /\ pc \in [Runs -> {"idle", "opened", "judge", "remove", "retake", "hold"} \cup Ended]
    /\ path \in 0..(2 * NR + 1)
    /\ writer \in [Files -> 0..NR]
    /\ owner \in [Files -> 0..NR]
    /\ fd \in [Runs -> 0..(2 * NR + 1)]
    /\ made \in 0..(2 * NR + 1)
    /\ crashes \in 0..MaxCrash

\* At most one run measures the tree.
OneHolder == \A a, b \in Runs : pc[a] = "hold" /\ pc[b] = "hold" => a = b

\* A run that died holds no lock, so it never keeps the tree from the next run.
NoDeadHolder == \A i \in Files : owner[i] /= None => pc[owner[i]] = "hold"

Safe == OneHolder /\ NoDeadHolder

\* Inductive for Next: one file stands at the path for good, and a lock is held only through it.
IndInv ==
    /\ TypeOK
    /\ Safe
    /\ made = path
    /\ made \in {0, 1}
    /\ \A r \in Runs : pc[r] \notin {"judge", "remove", "retake", "refused"}
    /\ \A r \in Runs : pc[r] \in {"opened", "hold"} => fd[r] = path /\ path /= None
    /\ \A r \in Runs : pc[r] \in {"idle", "done", "dead"} => fd[r] = None
    /\ \A r \in Runs : pc[r] = "hold" => owner[fd[r]] = r
    /\ \A i \in Files : owner[i] /= None => i = path /\ fd[owner[i]] = i

\* Reach: false once a run holds the lock while another run is dead. RunLock_reach.cfg lists
\* it, so TLC must find that state, which shows Safe guards states that occur.
HeldAfterCrash == ~\E a, b \in Runs : pc[a] = "dead" /\ pc[b] = "hold"
=============================================================================
