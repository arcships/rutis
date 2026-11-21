--------------------------- MODULE RowUnloadBudget ---------------------------
(***************************************************************************)
(* Single-row unload cleanup budget & Route B taint semantics (#1323).     *)
(*                                                                         *)
(* Design: docs/design-row-unload-budget-2026-11-20.md §2 (D1-D7) and §3.  *)
(* Implementation:                                                         *)
(*   - node/rutis-runtime/src/leaf.mjs (`toCordis`, `drain`, `provide`)    *)
(*   - node/rutis-runtime/src/runner.mjs (`loadRow`, `unloadRow`,          *)
(*       `disposeRowFiber`, `markTainted`, `findTaint`, `charge`)          *)
(*   - bun/rutis-bun/src/runner.ts (`Runtime.load`, `Runtime.unload`,      *)
(*       `Runtime.provide`, `Runtime.update`)                              *)
(*   - python/rutis/rutis/runner.py (`Runtime.load`, `Runtime.unload`,     *)
(*       `Context.provide`, `Runtime.provide`, `Runtime.charge`)           *)
(*                                                                         *)
(* Models a Host coordinating multiple scoped row instances (`Slots =      *)
(* RowNames \X Scopes`, including copies sharing the same bare row name    *)
(* across different `instances` scopes) inside one shared runtime process  *)
(* across generations and process restarts, operating on real shared       *)
(* runtime tables (`svcTable`, `rowStatus`, `taintedSlots`):               *)
(*   - In "leaf" mode (Node leaf, Bun, Python), each row's unload stack    *)
(*     drains in strict reverse order: outer user cleanup c2 -> framework  *)
(*     service withdrawal wd -> inner user cleanup c1 -> finally teardown. *)
(*   - In "cordis" mode (Node native Cordis), Cordis runs disposables      *)
(*     concurrently (Promise.all): service withdrawal runs on the first    *)
(*     microtask while user cleanup may hang.                              *)
(*   - When the opt-in per-row budget expires, neither JS nor Python can   *)
(*     forcibly kill the stuck Promise/coroutine, so the old generation's  *)
(*     cleanup enters `lingering` in the current process `epoch`.          *)
(*   - Lingering cleanups may execute real late actions against the shared *)
(*     runtime tables:                                                     *)
(*       * `LateProvide`: guarded by active row + matching generation;     *)
(*         mutation `"missingActiveGuard"` omits it and writes `svcTable`. *)
(*       * `LateWithdraw`: guarded by registration identity                *)
(*         `<<epoch, slot, gen, token>>`; mutation `"withdrawByNameOnly"`  *)
(*         omits the identity check and deletes a sibling's service.       *)
(*       * `LateUnhandledError`: guarded by rejection absorption and       *)
(*         active-origin check; mutation `"errorCorruptsLive"` marks a     *)
(*         live row `"failed"` in `rowStatus`.                             *)
(*   - Under Route B (`Policy = "taint"`), the runtime still runs all      *)
(*     framework withdrawals (D8) and taints ONLY the timed-out scoped     *)
(*     slot in `taintedSlots`, refusing in-place reload until              *)
(*     `RestartRuntime` starts a fresh process (`epoch + 1`). Sibling      *)
(*     instances with the same bare `rowName` in other scopes stay         *)
(*     unblocked (mutation `"taintByBareName"` violates                    *)
(*     `HealthyInstanceNotBlocked`).                                       *)
(*   - Under Route A (`Policy = "allowReload"`), the runtime allows        *)
(*     in-place reload of the same slot while its old cleanup is still     *)
(*     lingering, violating `NoConcurrentSameRowInSharedProcess`.          *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
    RowNames,   \* set of bare row names, e.g. {"w"}
    Scopes,     \* set of scope chains, e.g. {"s1", "s2"}
    Services,   \* set of shared service names, e.g. {"forecast"}
    MaxGen,     \* maximum load generations per slot
    MaxEpoch,   \* maximum runtime process incarnations (restarts)
    Policy,     \* "taint" (Route B) | "allowReload" (Route A)
    Mode,       \* "leaf" (sequential stack) | "cordis" (Node native Promise.all)
    Mutation    \* "none" | "skipWithdraw" | "continueSerial"
                \* | "missingActiveGuard" | "withdrawByNameOnly"
                \* | "errorCorruptsLive" | "taintByBareName"

Slots == RowNames \X Scopes
NoSvc == ""
NoOwner == <<0, <<"", "">>, 0, 0>>
NoListener == <<0, 0>>
Tokens == {1, 2}   \* 1 = legitimate load registration, 2 = unguarded late provide
Owners == (1..MaxEpoch) \X Slots \X (1..MaxGen) \X Tokens

ASSUME RowNames # {} /\ IsFiniteSet(RowNames) /\ "" \notin RowNames
ASSUME Scopes # {} /\ IsFiniteSet(Scopes) /\ "" \notin Scopes
ASSUME Services # {} /\ IsFiniteSet(Services) /\ NoSvc \notin Services
ASSUME MaxGen \in Nat /\ MaxGen >= 1
ASSUME MaxEpoch \in Nat /\ MaxEpoch >= 1
ASSUME Policy \in {"taint", "allowReload"}
ASSUME Mode \in {"leaf", "cordis"}
ASSUME Mutation \in {
    "none",
    "skipWithdraw",
    "continueSerial",
    "missingActiveGuard",
    "withdrawByNameOnly",
    "errorCorruptsLive",
    "taintByBareName"
}

VARIABLES
    epoch,              \* current runtime process incarnation (1..MaxEpoch)
    gen,                \* [Slots -> 0..MaxGen]: latest generation loaded for slot
    hostState,          \* [Slots -> {"idle", "running", "unloading", "tainted"}]
    rowStatus,          \* [Slots -> {"none", "active", "draining", "failed"}]: runtime row table
    stackPhase,         \* [Slots -> {"none", "c2", "wd", "c1", "cordis_disposing", "finish"}]
    rowSvc,             \* [Slots -> Services \cup {NoSvc}]: service registered by slot's active/draining gen
    svcTable,           \* [Services -> {NoOwner} \cup Owners]: shared runtime service table
    listenerActive,     \* [Slots -> {NoListener} \cup ((1..MaxEpoch) \X (1..MaxGen))]
    timedOut,           \* [Slots -> BOOLEAN]: whether current unload timed out
    timedOutInEpoch,    \* SUBSET Slots: slots whose unload actually timed out in current epoch
    taintedSlots,       \* SUBSET Slots: runtime's Route B taint table for current epoch
    lingering,          \* SUBSET ((1..MaxEpoch) \X Slots \X (1..MaxGen) \X {1} \X (Services \cup {NoSvc}))
    c1StartedWhileHung  \* ghost: inner cleanup c1 started while c2 of same gen was hung

vars == <<epoch, gen, hostState, rowStatus, stackPhase, rowSvc, svcTable,
          listenerActive, timedOut, timedOutInEpoch, taintedSlots, lingering,
          c1StartedWhileHung>>

TypeOK ==
    /\ epoch \in 1..MaxEpoch
    /\ gen \in [Slots -> 0..MaxGen]
    /\ hostState \in [Slots -> {"idle", "running", "unloading", "tainted"}]
    /\ rowStatus \in [Slots -> {"none", "active", "draining", "failed"}]
    /\ stackPhase \in [Slots -> {"none", "c2", "wd", "c1", "cordis_disposing", "finish"}]
    /\ rowSvc \in [Slots -> Services \cup {NoSvc}]
    /\ svcTable \in [Services -> {NoOwner} \cup Owners]
    /\ listenerActive \in [Slots -> {NoListener} \cup ((1..MaxEpoch) \X (1..MaxGen))]
    /\ timedOut \in [Slots -> BOOLEAN]
    /\ timedOutInEpoch \subseteq Slots
    /\ taintedSlots \subseteq Slots
    /\ lingering \subseteq ((1..MaxEpoch) \X Slots \X (1..MaxGen) \X {1} \X (Services \cup {NoSvc}))
    /\ c1StartedWhileHung \in [Slots -> BOOLEAN]

Init ==
    /\ epoch = 1
    /\ gen = [s \in Slots |-> 0]
    /\ hostState = [s \in Slots |-> "idle"]
    /\ rowStatus = [s \in Slots |-> "none"]
    /\ stackPhase = [s \in Slots |-> "none"]
    /\ rowSvc = [s \in Slots |-> NoSvc]
    /\ svcTable = [svc \in Services |-> NoOwner]
    /\ listenerActive = [s \in Slots |-> NoListener]
    /\ timedOut = [s \in Slots |-> FALSE]
    /\ timedOutInEpoch = {}
    /\ taintedSlots = {}
    /\ lingering = {}
    /\ c1StartedWhileHung = [s \in Slots |-> FALSE]

\* Scoped taint key vs deliberate bare-rowName bug (`Mutation = "taintByBareName"`).
TaintFor(s) ==
    IF Policy # "taint" THEN {}
    ELSE IF Mutation = "taintByBareName"
         THEN {t \in Slots : t[1] = s[1]}
         ELSE {s}

(***************************************************************************)
(* Host & Runtime: loading and starting unload                             *)
(***************************************************************************)

\* Host loads scoped slot s into the current runtime process (new generation).
\* If a shared service name is free in svcTable, s provides it under identity
\* <<epoch, s, g, 1>>; otherwise s loads without claiming that service so
\* multiple slots can run concurrently and contend/take over on unload.
Load(s) ==
    /\ hostState[s] = "idle"
    /\ rowStatus[s] = "none"
    /\ gen[s] < MaxGen
    /\ s \notin taintedSlots
    /\ LET g == gen[s] + 1
           freeSvcs == {svc \in Services : svcTable[svc] = NoOwner}
       IN
       /\ gen' = [gen EXCEPT ![s] = g]
       /\ hostState' = [hostState EXCEPT ![s] = "running"]
       /\ rowStatus' = [rowStatus EXCEPT ![s] = "active"]
       /\ stackPhase' = [stackPhase EXCEPT ![s] = "none"]
       /\ listenerActive' = [listenerActive EXCEPT ![s] = <<epoch, g>>]
       /\ timedOut' = [timedOut EXCEPT ![s] = FALSE]
       /\ IF freeSvcs # {}
          THEN LET svc == CHOOSE x \in freeSvcs : TRUE IN
               /\ rowSvc' = [rowSvc EXCEPT ![s] = svc]
               /\ svcTable' = [svcTable EXCEPT ![svc] = <<epoch, s, g, 1>>]
          ELSE /\ rowSvc' = [rowSvc EXCEPT ![s] = NoSvc]
               /\ svcTable' = svcTable
       /\ UNCHANGED <<epoch, timedOutInEpoch, taintedSlots, lingering,
                      c1StartedWhileHung>>

\* Under Route B, attempting to load a tainted slot in the same runtime fails
\* explicitly without mutating any runtime table or starting a new generation.
LoadRefused(s) ==
    /\ s \in taintedSlots
    /\ rowStatus[s] = "none"
    /\ hostState[s] \in {"idle", "tainted"}
    /\ UNCHANGED vars

\* Host requests rows.unload(s). Listeners are withdrawn immediately at unload
\* start (§7.1); the row enters "draining" until its unload stack finishes.
StartUnload(s) ==
    /\ hostState[s] = "running"
    /\ rowStatus[s] = "active"
    /\ hostState' = [hostState EXCEPT ![s] = "unloading"]
    /\ rowStatus' = [rowStatus EXCEPT ![s] = "draining"]
    /\ stackPhase' = [stackPhase EXCEPT ![s] =
                        IF Mode = "leaf" THEN "c2" ELSE "cordis_disposing"]
    /\ listenerActive' = [listenerActive EXCEPT ![s] = NoListener]
    /\ timedOut' = [timedOut EXCEPT ![s] = FALSE]
    /\ UNCHANGED <<epoch, gen, rowSvc, svcTable, timedOutInEpoch,
                   taintedSlots, lingering, c1StartedWhileHung>>

(***************************************************************************)
(* Leaf unload stack: c2 (outer cleanup) -> wd (withdrawal) -> c1 (inner)  *)
(***************************************************************************)

\* Outer user cleanup c2 finishes within the opt-in budget.
RunC2Ok(s) ==
    /\ Mode = "leaf"
    /\ rowStatus[s] = "draining"
    /\ stackPhase[s] = "c2"
    /\ stackPhase' = [stackPhase EXCEPT ![s] = "wd"]
    /\ UNCHANGED <<epoch, gen, hostState, rowStatus, rowSvc, svcTable,
                   listenerActive, timedOut, timedOutInEpoch, taintedSlots,
                   lingering, c1StartedWhileHung>>

\* Outer user cleanup c2 exceeds the opt-in budget: the runtime stops waiting
\* (c2 remains lingering in this process), marks TaintFor(s) tainted under
\* Route B, and proceeds to wd (unless Mutation = "skipWithdraw").
TimeoutC2(s) ==
    /\ Mode = "leaf"
    /\ rowStatus[s] = "draining"
    /\ stackPhase[s] = "c2"
    /\ timedOut' = [timedOut EXCEPT ![s] = TRUE]
    /\ timedOutInEpoch' = timedOutInEpoch \cup {s}
    /\ lingering' = lingering \cup {<<epoch, s, gen[s], 1, rowSvc[s]>>}
    /\ taintedSlots' = taintedSlots \cup TaintFor(s)
    /\ stackPhase' = [stackPhase EXCEPT ![s] =
                        IF Mutation = "skipWithdraw" THEN "finish" ELSE "wd"]
    /\ UNCHANGED <<epoch, gen, hostState, rowStatus, rowSvc, svcTable,
                   listenerActive, c1StartedWhileHung>>

\* Framework service withdrawal wd (D8 invariant: never bounded by timeout).
\* Withdraws only this registration's exact identity <<epoch, s, gen[s], 1>>.
\* After a timeout, subsequent user cleanup c1 is skipped unless the
\* deliberate mutation `continueSerial` violates D3.
RunWithdraw(s) ==
    /\ Mode = "leaf"
    /\ rowStatus[s] = "draining"
    /\ stackPhase[s] = "wd"
    /\ svcTable' =
         IF rowSvc[s] # NoSvc /\ svcTable[rowSvc[s]] = <<epoch, s, gen[s], 1>>
         THEN [svcTable EXCEPT ![rowSvc[s]] = NoOwner]
         ELSE svcTable
    /\ stackPhase' = [stackPhase EXCEPT ![s] =
                        IF timedOut[s] /\ Mutation # "continueSerial"
                        THEN "finish"
                        ELSE "c1"]
    /\ UNCHANGED <<epoch, gen, hostState, rowStatus, rowSvc, listenerActive,
                   timedOut, timedOutInEpoch, taintedSlots, lingering,
                   c1StartedWhileHung>>

\* Inner user cleanup c1 runs. If c2 of this same generation is still lingering,
\* strict serial LIFO order within the generation has been violated.
RunC1(s) ==
    /\ Mode = "leaf"
    /\ rowStatus[s] = "draining"
    /\ stackPhase[s] = "c1"
    /\ c1StartedWhileHung' =
         [c1StartedWhileHung EXCEPT ![s] =
            @ \/ (\E item \in lingering : item[1] = epoch /\ item[2] = s /\ item[3] = gen[s])]
    /\ stackPhase' = [stackPhase EXCEPT ![s] = "finish"]
    /\ UNCHANGED <<epoch, gen, hostState, rowStatus, rowSvc, svcTable,
                   listenerActive, timedOut, timedOutInEpoch, taintedSlots,
                   lingering>>

(***************************************************************************)
(* Node native Cordis fiber: Promise.all concurrent disposal               *)
(***************************************************************************)

\* Cordis's ctx.provide disposer removes the service from store on its first
\* microtask, concurrently with user disposables.
CordisWithdraw(s) ==
    /\ Mode = "cordis"
    /\ rowStatus[s] = "draining"
    /\ stackPhase[s] = "cordis_disposing"
    /\ rowSvc[s] # NoSvc
    /\ svcTable[rowSvc[s]] = <<epoch, s, gen[s], 1>>
    /\ svcTable' = [svcTable EXCEPT ![rowSvc[s]] = NoOwner]
    /\ UNCHANGED <<epoch, gen, hostState, rowStatus, stackPhase, rowSvc,
                   listenerActive, timedOut, timedOutInEpoch, taintedSlots,
                   lingering, c1StartedWhileHung>>

\* All Cordis disposables settle within the budget.
CordisFinishOk(s) ==
    /\ Mode = "cordis"
    /\ rowStatus[s] = "draining"
    /\ stackPhase[s] = "cordis_disposing"
    /\ (IF rowSvc[s] = NoSvc THEN TRUE ELSE svcTable[rowSvc[s]] # <<epoch, s, gen[s], 1>>)
    /\ stackPhase' = [stackPhase EXCEPT ![s] = "finish"]
    /\ UNCHANGED <<epoch, gen, hostState, rowStatus, rowSvc, svcTable,
                   listenerActive, timedOut, timedOutInEpoch, taintedSlots,
                   lingering, c1StartedWhileHung>>

\* row.fiber.dispose() exceeds the budget: stop waiting, mark TaintFor(s)
\* tainted under Route B, proceed to finally teardown.
CordisTimeout(s) ==
    /\ Mode = "cordis"
    /\ rowStatus[s] = "draining"
    /\ stackPhase[s] = "cordis_disposing"
    /\ (IF rowSvc[s] = NoSvc \/ Mutation = "skipWithdraw"
        THEN TRUE
        ELSE svcTable[rowSvc[s]] # <<epoch, s, gen[s], 1>>)
    /\ timedOut' = [timedOut EXCEPT ![s] = TRUE]
    /\ timedOutInEpoch' = timedOutInEpoch \cup {s}
    /\ lingering' = lingering \cup {<<epoch, s, gen[s], 1, rowSvc[s]>>}
    /\ taintedSlots' = taintedSlots \cup TaintFor(s)
    /\ stackPhase' = [stackPhase EXCEPT ![s] = "finish"]
    /\ UNCHANGED <<epoch, gen, hostState, rowStatus, rowSvc, svcTable,
                   listenerActive, c1StartedWhileHung>>

(***************************************************************************)
(* Final teardown & reply to host                                          *)
(***************************************************************************)

FinishUnload(s) ==
    /\ rowStatus[s] = "draining"
    /\ stackPhase[s] = "finish"
    /\ rowStatus' = [rowStatus EXCEPT ![s] = "none"]
    /\ stackPhase' = [stackPhase EXCEPT ![s] = "none"]
    /\ rowSvc' = [rowSvc EXCEPT ![s] = NoSvc]
    /\ listenerActive' = [listenerActive EXCEPT ![s] = NoListener]
    /\ hostState' = [hostState EXCEPT ![s] =
                       IF s \in taintedSlots THEN "tainted" ELSE "idle"]
    /\ UNCHANGED <<epoch, gen, svcTable, timedOut, timedOutInEpoch,
                   taintedSlots, lingering, c1StartedWhileHung>>

(***************************************************************************)
(* Lingering old cleanup actions against shared runtime tables             *)
(***************************************************************************)

\* A stuck cleanup eventually settles on its own and exits background execution.
LingeringFinish(item) ==
    /\ item \in lingering
    /\ item[1] = epoch
    /\ lingering' = lingering \ {item}
    /\ UNCHANGED <<epoch, gen, hostState, rowStatus, stackPhase, rowSvc,
                   svcTable, listenerActive, timedOut, timedOutInEpoch,
                   taintedSlots, c1StartedWhileHung>>

\* Late `ctx.provide(svc, ...)` from a lingering old-generation cleanup.
\* Guarded by active-row + matching-generation check (Node leaf.mjs `if (draining)`,
\* Bun `this.#rows.get(row.key) !== row`, Python `rows.get(key) is not row or row.unloading`).
\* Mutation `"missingActiveGuard"` omits the guard and writes into `svcTable`.
LateProvide(item, svc) ==
    /\ item \in lingering
    /\ item[1] = epoch
    /\ svc \in Services
    /\ LET s == item[2]
           g == item[3]
           isActiveOrigin == (rowStatus[s] = "active") /\ (gen[s] = g)
       IN
       IF isActiveOrigin \/ Mutation = "missingActiveGuard"
       THEN svcTable' = [svcTable EXCEPT ![svc] = <<epoch, s, g, 2>>]
       ELSE svcTable' = svcTable
    /\ UNCHANGED <<epoch, gen, hostState, rowStatus, stackPhase, rowSvc,
                   listenerActive, timedOut, timedOutInEpoch, taintedSlots,
                   lingering, c1StartedWhileHung>>

\* Late invocation of a captured `withdraw()` handle from a lingering cleanup.
\* Guarded by exact registration identity `<<epoch, slot, gen, token>>`
\* (Node `provided.get(name) !== entry`, Bun `this.#services.get(id) !== entry`,
\* Python `self.services.get(id) is entry`).
\* Mutation `"withdrawByNameOnly"` omits the identity check and deletes by name.
LateWithdraw(item) ==
    /\ item \in lingering
    /\ item[1] = epoch
    /\ item[5] # NoSvc
    /\ LET svc == item[5]
           regId == <<item[1], item[2], item[3], item[4]>>
       IN
       IF svcTable[svc] = regId \/ Mutation = "withdrawByNameOnly"
       THEN svcTable' = [svcTable EXCEPT ![svc] = NoOwner]
       ELSE svcTable' = svcTable
    /\ UNCHANGED <<epoch, gen, hostState, rowStatus, stackPhase, rowSvc,
                   listenerActive, timedOut, timedOutInEpoch, taintedSlots,
                   lingering, c1StartedWhileHung>>

\* Late unhandled rejection / error from a lingering cleanup.
\* Guarded by rejection-absorption callbacks + active-origin row attribution
\* (Node/Python `charge()`). Mutation `"errorCorruptsLive"` omits the guard
\* and marks a live row `"failed"` in `rowStatus`.
LateUnhandledError(item, target) ==
    /\ item \in lingering
    /\ item[1] = epoch
    /\ target \in Slots
    /\ rowStatus[target] = "active"
    /\ LET isActiveOrigin == (target = item[2]) /\ (gen[target] = item[3]) IN
       IF isActiveOrigin \/ Mutation = "errorCorruptsLive"
       THEN rowStatus' = [rowStatus EXCEPT ![target] = "failed"]
       ELSE rowStatus' = rowStatus
    /\ UNCHANGED <<epoch, gen, hostState, stackPhase, rowSvc, svcTable,
                   listenerActive, timedOut, timedOutInEpoch, taintedSlots,
                   lingering, c1StartedWhileHung>>

(***************************************************************************)
(* Runtime process restart (clears taint and terminates lingering work)    *)
(***************************************************************************)

RestartRuntime ==
    /\ epoch < MaxEpoch
    /\ taintedSlots # {}
    /\ epoch' = epoch + 1
    /\ hostState' = [s \in Slots |-> "idle"]
    /\ rowStatus' = [s \in Slots |-> "none"]
    /\ stackPhase' = [s \in Slots |-> "none"]
    /\ rowSvc' = [s \in Slots |-> NoSvc]
    /\ svcTable' = [svc \in Services |-> NoOwner]
    /\ listenerActive' = [s \in Slots |-> NoListener]
    /\ timedOut' = [s \in Slots |-> FALSE]
    /\ timedOutInEpoch' = {}
    /\ taintedSlots' = {}
    /\ lingering' = {}
    /\ UNCHANGED <<gen, c1StartedWhileHung>>

Next ==
    \/ \E s \in Slots :
         \/ Load(s)
         \/ LoadRefused(s)
         \/ StartUnload(s)
         \/ RunC2Ok(s)
         \/ TimeoutC2(s)
         \/ RunWithdraw(s)
         \/ RunC1(s)
         \/ CordisWithdraw(s)
         \/ CordisFinishOk(s)
         \/ CordisTimeout(s)
         \/ FinishUnload(s)
    \/ \E item \in lingering :
         \/ LingeringFinish(item)
         \/ \E svc \in Services : LateProvide(item, svc)
         \/ LateWithdraw(item)
         \/ \E target \in Slots : LateUnhandledError(item, target)
    \/ RestartRuntime

Spec == Init /\ [][Next]_vars

(***************************************************************************)
(* Safety invariants                                                       *)
(***************************************************************************)

\* D8: After any normal or timed-out unload finishes (slot idle or tainted),
\* no service or listener owned by that slot remains registered in the runtime.
NoOrphanWithdrawal ==
    \A s \in Slots :
        (hostState[s] \in {"idle", "tainted"}) =>
            /\ listenerActive[s] = NoListener
            /\ \A svc \in Services :
                   svcTable[svc] # NoOwner => svcTable[svc][2] # s

\* D3: No later user cleanup (c1) starts while an earlier user cleanup (c2)
\* of the same generation is timed out and still lingering.
SerialCleanupsWithinGen ==
    \A s \in Slots : ~c1StartedWhileHung[s]

\* Real table invariant 1: Every entry in the shared service table belongs to
\* the current epoch, an active or draining slot of its current generation,
\* with a legitimate load registration token (1) and matching rowSvc.
ServiceOwnerValid ==
    \A svc \in Services :
        svcTable[svc] # NoOwner =>
            LET o == svcTable[svc] IN
            /\ o[1] = epoch
            /\ rowStatus[o[2]] \in {"active", "draining"}
            /\ gen[o[2]] = o[3]
            /\ o[4] = 1
            /\ rowSvc[o[2]] = svc

\* Real table invariant 2: Every active slot that registered a service still
\* owns that exact registration in svcTable (no late withdraw deleted it and
\* no late provide overwrote it).
ActiveServiceIntact ==
    \A s \in Slots :
        (rowStatus[s] = "active" /\ rowSvc[s] # NoSvc) =>
            svcTable[rowSvc[s]] = <<epoch, s, gen[s], 1>>

\* Real table invariant 3: No live slot is ever spuriously transitioned to
\* "failed" in rowStatus by a lingering old cleanup's late unhandled error.
NoSpuriousRowFailure ==
    \A s \in Slots : rowStatus[s] # "failed"

\* D5 / Route B cross-generation & cross-slot isolation on real runtime tables.
NoCrossGenCorruption ==
    /\ ServiceOwnerValid
    /\ ActiveServiceIntact
    /\ NoSpuriousRowFailure

\* Scoped multi-instance taint isolation: a healthy instance (even one sharing
\* the same bare row name in another scope) is never tainted or blocked unless
\* that exact scoped slot timed out in the current epoch.
HealthyInstanceNotBlocked ==
    \A s \in taintedSlots : s \in timedOutInEpoch

\* Route B vs Route A: In one shared runtime process (`epoch`), a scoped slot
\* is never active in a new generation concurrently with its own lingering old
\* cleanup. Holds under Route B (`Policy = "taint"`); violated under Route A
\* (`Policy = "allowReload"`).
NoConcurrentSameRowInSharedProcess ==
    \A s \in Slots :
        (rowStatus[s] = "active") =>
            ~\E item \in lingering : item[1] = epoch /\ item[2] = s

=============================================================================
