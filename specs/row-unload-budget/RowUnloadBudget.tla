--------------------------- MODULE RowUnloadBudget ---------------------------
(***************************************************************************)
(* Single-row unload cleanup budget & Route B taint semantics (#1323).     *)
(*                                                                         *)
(* Design: docs/design-row-unload-budget-2026-11-20.md §2 (D1-D7) and §3.  *)
(* Implementation:                                                         *)
(*   - node/rutis-runtime/src/leaf.mjs (`toCordis`, `drain`, `provide`)    *)
(*   - node/rutis-runtime/src/runner.mjs (`loadRow`, `unloadRow`,          *)
(*       `disposeRowFiber`, `markTainted`)                                 *)
(*   - bun/rutis-bun/src/runner.ts (`Runtime.load`, `Runtime.unload`)      *)
(*   - python/rutis/rutis/runner.py (`Runtime.load`, `Runtime.unload`)     *)
(*                                                                         *)
(* Models a Host coordinating multiple rows inside one shared runtime      *)
(* process (Node, Bun, or Python) across row generations and runtime       *)
(* process restarts:                                                       *)
(*   - In "leaf" mode (Node leaf, Bun, Python), each row's unload stack    *)
(*     drains in strict reverse order: outer user cleanup c2 -> framework  *)
(*     service withdrawal wd -> inner user cleanup c1 -> finally teardown. *)
(*   - In "cordis" mode (Node native Cordis), Cordis runs disposables      *)
(*     concurrently (Promise.all): service withdrawal runs on the first    *)
(*     microtask while user cleanup may hang.                              *)
(*   - When the opt-in per-row budget expires, neither JS nor Python can   *)
(*     forcibly kill the stuck Promise/coroutine, so the old generation's  *)
(*     cleanup enters `lingering` in the current process `epoch`.          *)
(*   - Under Route B (`Policy = "taint"`), the runtime still runs all      *)
(*     framework withdrawals (D8) and marks the timed-out row tainted in   *)
(*     this process, refusing in-place reload until `RestartRuntime`       *)
(*     starts a fresh process incarnation (`epoch + 1`). Unrelated healthy *)
(*     rows continue unaffected.                                           *)
(*   - Under Route A (`Policy = "allowReload"`), the runtime allows        *)
(*     in-place reload of the same row while its old cleanup is still      *)
(*     lingering, violating `NoConcurrentSameRowInSharedProcess`.          *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
    Rows,      \* set of row ids sharing one runtime process, e.g. {"w", "m"}
    MaxGen,    \* maximum load generations per row
    MaxEpoch,  \* maximum runtime process incarnations (restarts)
    Policy,    \* "taint" (Route B) | "allowReload" (Route A)
    Mode,      \* "leaf" (sequential stack) | "cordis" (Node native Promise.all)
    Mutation   \* "none" | "skipWithdraw" | "continueSerial" | "corruptNewGen"

None == <<0, 0>>

ASSUME Rows # {} /\ IsFiniteSet(Rows)
ASSUME MaxGen \in Nat /\ MaxGen >= 1
ASSUME MaxEpoch \in Nat /\ MaxEpoch >= 1
ASSUME Policy \in {"taint", "allowReload"}
ASSUME Mode \in {"leaf", "cordis"}
ASSUME Mutation \in {"none", "skipWithdraw", "continueSerial", "corruptNewGen"}

VARIABLES
    epoch,              \* current runtime process incarnation (1..MaxEpoch)
    gen,                \* [Rows -> 0..MaxGen]: latest generation loaded for r
    hostState,          \* [Rows -> {"idle", "running", "unloading", "tainted"}]
    rtRow,              \* [Rows -> {"none", "active", "draining", "done"}]
    stackPhase,         \* [Rows -> {"none", "c2", "wd", "c1", "cordis_disposing", "finish"}]
    svcOwner,           \* [Rows -> {None} \cup ((1..MaxEpoch) \X (1..MaxGen))]
    listenerActive,     \* [Rows -> {None} \cup ((1..MaxEpoch) \X (1..MaxGen))]
    timedOut,           \* [Rows -> BOOLEAN]: whether current unload timed out
    tainted,            \* SUBSET Rows: rows blocked from reload in this epoch
    lingering,          \* SUBSET ((1..MaxEpoch) \X Rows \X (1..MaxGen)): stuck cleanups still running
    c1StartedWhileHung, \* ghost: inner cleanup c1 started while c2 of same gen was hung
    corrupted           \* ghost: a lingering old cleanup corrupted a live row or new generation

vars == <<epoch, gen, hostState, rtRow, stackPhase, svcOwner, listenerActive,
          timedOut, tainted, lingering, c1StartedWhileHung, corrupted>>

TypeOK ==
    /\ epoch \in 1..MaxEpoch
    /\ gen \in [Rows -> 0..MaxGen]
    /\ hostState \in [Rows -> {"idle", "running", "unloading", "tainted"}]
    /\ rtRow \in [Rows -> {"none", "active", "draining", "done"}]
    /\ stackPhase \in [Rows -> {"none", "c2", "wd", "c1", "cordis_disposing", "finish"}]
    /\ svcOwner \in [Rows -> {None} \cup ((1..MaxEpoch) \X (1..MaxGen))]
    /\ listenerActive \in [Rows -> {None} \cup ((1..MaxEpoch) \X (1..MaxGen))]
    /\ timedOut \in [Rows -> BOOLEAN]
    /\ tainted \subseteq Rows
    /\ lingering \subseteq ((1..MaxEpoch) \X Rows \X (1..MaxGen))
    /\ c1StartedWhileHung \in [Rows -> BOOLEAN]
    /\ corrupted \in [Rows -> BOOLEAN]

Init ==
    /\ epoch = 1
    /\ gen = [r \in Rows |-> 0]
    /\ hostState = [r \in Rows |-> "idle"]
    /\ rtRow = [r \in Rows |-> "none"]
    /\ stackPhase = [r \in Rows |-> "none"]
    /\ svcOwner = [r \in Rows |-> None]
    /\ listenerActive = [r \in Rows |-> None]
    /\ timedOut = [r \in Rows |-> FALSE]
    /\ tainted = {}
    /\ lingering = {}
    /\ c1StartedWhileHung = [r \in Rows |-> FALSE]
    /\ corrupted = [r \in Rows |-> FALSE]

(***************************************************************************)
(* Host & Runtime: loading and starting unload                             *)
(***************************************************************************)

\* Host loads row r into the current runtime process (new generation).
Load(r) ==
    /\ hostState[r] = "idle"
    /\ gen[r] < MaxGen
    /\ r \notin tainted
    /\ LET g == gen[r] + 1 IN
       /\ gen' = [gen EXCEPT ![r] = g]
       /\ hostState' = [hostState EXCEPT ![r] = "running"]
       /\ rtRow' = [rtRow EXCEPT ![r] = "active"]
       /\ stackPhase' = [stackPhase EXCEPT ![r] = "none"]
       /\ svcOwner' = [svcOwner EXCEPT ![r] = <<epoch, g>>]
       /\ listenerActive' = [listenerActive EXCEPT ![r] = <<epoch, g>>]
       /\ timedOut' = [timedOut EXCEPT ![r] = FALSE]
       /\ UNCHANGED <<epoch, tainted, lingering, c1StartedWhileHung, corrupted>>

\* Under Route B, attempting to load a tainted row in the same runtime fails
\* explicitly without mutating any runtime table or starting a new generation.
LoadRefused(r) ==
    /\ hostState[r] = "tainted"
    /\ r \in tainted
    /\ UNCHANGED vars

\* Host requests rows.unload(r). Listeners are withdrawn immediately at unload
\* start (§7.1); service stays visible only to r until its withdrawal step.
StartUnload(r) ==
    /\ hostState[r] = "running"
    /\ rtRow[r] = "active"
    /\ hostState' = [hostState EXCEPT ![r] = "unloading"]
    /\ rtRow' = [rtRow EXCEPT ![r] = "draining"]
    /\ stackPhase' = [stackPhase EXCEPT ![r] =
                        IF Mode = "leaf" THEN "c2" ELSE "cordis_disposing"]
    /\ listenerActive' = [listenerActive EXCEPT ![r] = None]
    /\ timedOut' = [timedOut EXCEPT ![r] = FALSE]
    /\ UNCHANGED <<epoch, gen, svcOwner, tainted, lingering,
                   c1StartedWhileHung, corrupted>>

(***************************************************************************)
(* Leaf unload stack: c2 (outer cleanup) -> wd (withdrawal) -> c1 (inner)  *)
(***************************************************************************)

\* Outer user cleanup c2 finishes within the opt-in budget.
RunC2Ok(r) ==
    /\ Mode = "leaf"
    /\ rtRow[r] = "draining"
    /\ stackPhase[r] = "c2"
    /\ stackPhase' = [stackPhase EXCEPT ![r] = "wd"]
    /\ UNCHANGED <<epoch, gen, hostState, rtRow, svcOwner, listenerActive,
                   timedOut, tainted, lingering, c1StartedWhileHung, corrupted>>

\* Outer user cleanup c2 exceeds the opt-in budget: the runtime stops waiting
\* (c2 remains lingering in this process), marks r tainted under Route B, and
\* proceeds to wd (unless Mutation = "skipWithdraw" skips withdrawals).
TimeoutC2(r) ==
    /\ Mode = "leaf"
    /\ rtRow[r] = "draining"
    /\ stackPhase[r] = "c2"
    /\ timedOut' = [timedOut EXCEPT ![r] = TRUE]
    /\ lingering' = lingering \cup {<<epoch, r, gen[r]>>}
    /\ tainted' = IF Policy = "taint" THEN tainted \cup {r} ELSE tainted
    /\ stackPhase' = [stackPhase EXCEPT ![r] =
                        IF Mutation = "skipWithdraw" THEN "finish" ELSE "wd"]
    /\ UNCHANGED <<epoch, gen, hostState, rtRow, svcOwner, listenerActive,
                   c1StartedWhileHung, corrupted>>

\* Framework service withdrawal wd (D8 invariant: never bounded by timeout).
\* After a timeout, subsequent user cleanup c1 is skipped unless the
\* deliberate mutation `continueSerial` violates D3.
RunWithdraw(r) ==
    /\ Mode = "leaf"
    /\ rtRow[r] = "draining"
    /\ stackPhase[r] = "wd"
    /\ svcOwner' = [svcOwner EXCEPT ![r] =
                      IF @ = <<epoch, gen[r]>> THEN None ELSE @]
    /\ stackPhase' = [stackPhase EXCEPT ![r] =
                        IF timedOut[r] /\ Mutation # "continueSerial"
                        THEN "finish"
                        ELSE "c1"]
    /\ UNCHANGED <<epoch, gen, hostState, rtRow, listenerActive, timedOut,
                   tainted, lingering, c1StartedWhileHung, corrupted>>

\* Inner user cleanup c1 runs. If c2 of this same generation is still lingering,
\* strict serial LIFO order within the generation has been violated.
RunC1(r) ==
    /\ Mode = "leaf"
    /\ rtRow[r] = "draining"
    /\ stackPhase[r] = "c1"
    /\ c1StartedWhileHung' =
         [c1StartedWhileHung EXCEPT ![r] = @ \/ (<<epoch, r, gen[r]>> \in lingering)]
    /\ stackPhase' = [stackPhase EXCEPT ![r] = "finish"]
    /\ UNCHANGED <<epoch, gen, hostState, rtRow, svcOwner, listenerActive,
                   timedOut, tainted, lingering, corrupted>>

(***************************************************************************)
(* Node native Cordis fiber: Promise.all concurrent disposal               *)
(***************************************************************************)

\* Cordis's ctx.provide disposer removes the service from store on its first
\* microtask, concurrently with user disposables.
CordisWithdraw(r) ==
    /\ Mode = "cordis"
    /\ rtRow[r] = "draining"
    /\ stackPhase[r] = "cordis_disposing"
    /\ svcOwner[r] = <<epoch, gen[r]>>
    /\ svcOwner' = [svcOwner EXCEPT ![r] = None]
    /\ UNCHANGED <<epoch, gen, hostState, rtRow, stackPhase, listenerActive,
                   timedOut, tainted, lingering, c1StartedWhileHung, corrupted>>

\* All Cordis disposables settle within the budget.
CordisFinishOk(r) ==
    /\ Mode = "cordis"
    /\ rtRow[r] = "draining"
    /\ stackPhase[r] = "cordis_disposing"
    /\ svcOwner[r] = None
    /\ stackPhase' = [stackPhase EXCEPT ![r] = "finish"]
    /\ UNCHANGED <<epoch, gen, hostState, rtRow, svcOwner, listenerActive,
                   timedOut, tainted, lingering, c1StartedWhileHung, corrupted>>

\* row.fiber.dispose() exceeds the budget: stop waiting, mark tainted under
\* Route B, proceed to finally teardown.
CordisTimeout(r) ==
    /\ Mode = "cordis"
    /\ rtRow[r] = "draining"
    /\ stackPhase[r] = "cordis_disposing"
    /\ (svcOwner[r] = None \/ Mutation = "skipWithdraw")
    /\ timedOut' = [timedOut EXCEPT ![r] = TRUE]
    /\ lingering' = lingering \cup {<<epoch, r, gen[r]>>}
    /\ tainted' = IF Policy = "taint" THEN tainted \cup {r} ELSE tainted
    /\ stackPhase' = [stackPhase EXCEPT ![r] = "finish"]
    /\ UNCHANGED <<epoch, gen, hostState, rtRow, svcOwner, listenerActive,
                   c1StartedWhileHung, corrupted>>

(***************************************************************************)
(* Final teardown & reply to host                                          *)
(***************************************************************************)

FinishUnload(r) ==
    /\ rtRow[r] = "draining"
    /\ stackPhase[r] = "finish"
    /\ rtRow' = [rtRow EXCEPT ![r] = "done"]
    /\ stackPhase' = [stackPhase EXCEPT ![r] = "none"]
    /\ listenerActive' = [listenerActive EXCEPT ![r] = None]
    /\ hostState' = [hostState EXCEPT ![r] =
                       IF r \in tainted THEN "tainted" ELSE "idle"]
    /\ UNCHANGED <<epoch, gen, svcOwner, timedOut, tainted, lingering,
                   c1StartedWhileHung, corrupted>>

(***************************************************************************)
(* Lingering old cleanup (background Promise / coroutine in this process)  *)
(***************************************************************************)

\* A stuck cleanup eventually settles on its own and exits background execution.
LingeringFinish(item) ==
    /\ item \in lingering
    /\ item[1] = epoch
    /\ lingering' = lingering \ {item}
    /\ UNCHANGED <<epoch, gen, hostState, rtRow, stackPhase, svcOwner,
                   listenerActive, timedOut, tainted, c1StartedWhileHung,
                   corrupted>>

\* A lingering cleanup attempts a late provide / emit / withdraw / rejection
\* while another row or a newer generation is running in the same process.
\* Properly guarded runtimes refuse/absorb it; `Mutation = "corruptNewGen"`
\* models missing isolation or generation guards corrupting the live row.
LingeringLateAction(item, target) ==
    /\ item \in lingering
    /\ item[1] = epoch
    /\ target \in Rows
    /\ hostState[target] = "running"
    /\ (target # item[2] \/ gen[target] > item[3])
    /\ corrupted' =
         IF Mutation = "corruptNewGen"
         THEN [corrupted EXCEPT ![target] = TRUE]
         ELSE corrupted
    /\ UNCHANGED <<epoch, gen, hostState, rtRow, stackPhase, svcOwner,
                   listenerActive, timedOut, tainted, lingering,
                   c1StartedWhileHung>>

(***************************************************************************)
(* Runtime process restart (clears taint and terminates lingering work)    *)
(***************************************************************************)

RestartRuntime ==
    /\ epoch < MaxEpoch
    /\ tainted # {}
    /\ epoch' = epoch + 1
    /\ hostState' = [r \in Rows |-> "idle"]
    /\ rtRow' = [r \in Rows |-> "none"]
    /\ stackPhase' = [r \in Rows |-> "none"]
    /\ svcOwner' = [r \in Rows |-> None]
    /\ listenerActive' = [r \in Rows |-> None]
    /\ timedOut' = [r \in Rows |-> FALSE]
    /\ tainted' = {}
    /\ lingering' = {}
    /\ UNCHANGED <<gen, c1StartedWhileHung, corrupted>>

Next ==
    \/ \E r \in Rows :
         \/ Load(r)
         \/ LoadRefused(r)
         \/ StartUnload(r)
         \/ RunC2Ok(r)
         \/ TimeoutC2(r)
         \/ RunWithdraw(r)
         \/ RunC1(r)
         \/ CordisWithdraw(r)
         \/ CordisFinishOk(r)
         \/ CordisTimeout(r)
         \/ FinishUnload(r)
    \/ \E item \in lingering :
         \/ LingeringFinish(item)
         \/ \E target \in Rows : LingeringLateAction(item, target)
    \/ RestartRuntime

Spec == Init /\ [][Next]_vars

(***************************************************************************)
(* Safety invariants                                                       *)
(***************************************************************************)

\* D8: After any normal or timed-out unload finishes (row idle or tainted),
\* no service or listener of that row remains registered in the runtime.
NoOrphanWithdrawal ==
    \A r \in Rows :
        (hostState[r] \in {"idle", "tainted"}) =>
            /\ svcOwner[r] = None
            /\ listenerActive[r] = None

\* D3: No later user cleanup (c1) starts while an earlier user cleanup (c2)
\* of the same generation is timed out and still lingering.
SerialCleanupsWithinGen ==
    \A r \in Rows : ~c1StartedWhileHung[r]

\* D5 / Route B: A lingering old cleanup can never fail, withdraw, or corrupt
\* another row or a newer generation.
NoCrossGenCorruption ==
    \A r \in Rows : ~corrupted[r]

\* Route B vs Route A: In one shared runtime process (`epoch`), a row is never
\* running a new generation concurrently with its own lingering old cleanup.
\* Holds under Route B (`Policy = "taint"`); violated under Route A
\* (`Policy = "allowReload"`).
NoConcurrentSameRowInSharedProcess ==
    \A r \in Rows :
        (hostState[r] = "running") =>
            ~\E g \in 1..MaxGen : <<epoch, r, g>> \in lingering

=============================================================================
