--------------------------- MODULE EventModeCatalog ---------------------------
(***************************************************************************)
(* Optional event mode catalog and runtime bus enforcement (#1324):        *)
(* concurrent dispatchers (user emit/bail/serial and framework `rutis/*`), *)
(* host catalog swaps (valid and invalid), atomic `has_catalog` / epoch /  *)
(* Warn deduplication synchronization, and two-phase listener close        *)
(* admission / drain.                                                      *)
(*                                                                         *)
(* Design: docs/design-event-mode-catalog-2026-11-20.md §2 (D1-D9) and     *)
(* §3 (rules 3.1-3.5). Implementation: crates/rutis/src/bus.rs             *)
(* (`set_event_mode_catalog`, `check_mode`, `snapshot_with`),              *)
(* crates/rutis/src/events/catalog.rs (`EventModeCatalog::validate`), and  *)
(* crates/rutis/src/ctx.rs (`emitter_label`, `set_event_mode_catalog`).    *)
(*                                                                         *)
(* Key safety properties verified exhaustively by TLC:                     *)
(* - NoPartialExecOnError: under Error policy, a mismatched dispatch       *)
(*   invokes 0 listeners and leaves 0 in-flight calls.                     *)
(* - AtomicCatalogValidation: invalid catalog updates ("", `rutis/*`,      *)
(*   `internal/*`) never overwrite or clear the prior valid catalog/epoch. *)
(* - CatalogFlagInSync: `has_catalog` stays synchronized with whether a    *)
(*   catalog is installed (`(catalogMode # "none") <=> hasCatalog`),       *)
(*   `warned` tracks the active catalog epoch, and no epoch warning is     *)
(*   lost or emitted twice.                                                *)
(* - WarnPreservesDelivery: under Warn policy, mismatched dispatches still *)
(*   invoke every eligible live listener identically to an empty catalog,  *)
(*   and warn at most once per (emitter, event, actual, expected) per      *)
(*   catalog epoch.                                                        *)
(* - FrameworkNeverBlocked: rutis/* framework dispatches are never         *)
(*   rejected by user catalog entries.                                     *)
(* - CloseDrainPreserved: catalog checks and swaps never admit a listener  *)
(*   after close commit or allow cleanup while a call is in flight.        *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
    Mutation,       \* "none" or a deliberate bug (see README.md)
    MaxEpoch,       \* bound on valid catalog replacements
    MaxDispatches   \* bound on dispatches per dispatcher

Dispatchers == {"d1", "d2"}
Listeners   == {"l1", "l2"}
Closable    == "l2"

\* Representative dispatch shapes:
\* - ("msg", "emit", FALSE)      : mismatched user emit when catalog expects bail
\* - ("msg", "serial", FALSE)    : mismatched user serial when catalog expects bail
\* - ("msg", "bail", FALSE)      : matching user bail
\* - ("other", "emit", FALSE)    : unlisted user event (never catalogued)
\* - ("rutis/row", "emit", TRUE) : framework emit (bypasses user catalog)
Jobs == {
    <<"msg", "emit", FALSE>>,
    <<"msg", "serial", FALSE>>,
    <<"msg", "bail", FALSE>>,
    <<"other", "emit", FALSE>>,
    <<"rutis/row", "emit", TRUE>>
}

CatalogConfigs == {
    <<"none", "warn">>,
    <<"bail", "warn">>,
    <<"bail", "error">>
}

\* Deduplication keys for mismatched Warn dispatches on catalogued "msg" -> "bail".
WarnKeys == Dispatchers \X {"emit", "serial"}

ASSUME Mutation \in {
    "none",
    "checkAfterStart",
    "clearOnInvalid",
    "blockFramework",
    "warnDropsDelivery",
    "nonAtomicSwap"
}
ASSUME MaxEpoch \in Nat /\ MaxEpoch >= 1
ASSUME MaxDispatches \in Nat /\ MaxDispatches >= 1

VARIABLES
    \* Catalog state (bus.rs: has_catalog, catalog{epoch, catalog}, warned{epoch, seen})
    catalogMode,         \* "none" | "bail" (expected mode for "msg")
    catalogPolicy,       \* "warn" | "error"
    hasCatalog,          \* BOOLEAN fast-path flag (`BusCore::has_catalog`)
    epoch,               \* 0..MaxEpoch, incremented on each valid swap
    warned,              \* subset of WarnKeys already warned in warnedEpoch
    warnedEpoch,         \* 0..MaxEpoch, epoch that `warned` belongs to
    epochWarnCount,      \* [WarnKeys -> Nat]: warnings emitted in current epoch
    swapPhase,           \* "idle" | "wroteCatalog" | "storedFlag" (nonAtomicSwap only)
    swapNonEmpty,        \* BOOLEAN pending has_catalog store (nonAtomicSwap only)
    swapEpoch,           \* 0..MaxEpoch pending warned reset epoch (nonAtomicSwap only)
    invalidTried,        \* whether SetInvalidCatalog has been exercised in current epoch

    \* Ghost: last successfully validated catalog configuration and epoch
    lastValidMode,
    lastValidPolicy,
    lastValidEpoch,

    \* Per-dispatcher state
    dState,              \* [Dispatchers -> "idle" | "checking" | "recordingWarn" | "delivering" | "done" | "rejected"]
    dEvent,              \* [Dispatchers -> {"msg", "other", "rutis/row"}]
    dMode,               \* [Dispatchers -> {"emit", "serial", "bail"}]
    dFw,                 \* [Dispatchers -> BOOLEAN]
    dCount,              \* [Dispatchers -> 0..MaxDispatches]
    dSnapEpoch,          \* [Dispatchers -> 0..MaxEpoch]: epoch read under catalog.read()
    dRemaining,          \* [Dispatchers -> SUBSET Listeners]
    dInvoked,            \* [Dispatchers -> SUBSET Listeners]
    dSkippedClosed,      \* [Dispatchers -> SUBSET Listeners]
    dResult,             \* [Dispatchers -> "none" | "ok" | "warned" | "dropped" | "error"]

    \* Listener close admission & in-flight calls
    inFlight,            \* SUBSET (Dispatchers \X Listeners)
    marked,              \* SUBSET Listeners (lineage_closed)
    closeCommitted,      \* SUBSET Listeners (marker pass completed)
    closing,             \* SUBSET Listeners (closing flag published)
    cleaned,             \* SUBSET Listeners (cleanup completed)
    admittedAfterCommit  \* [Dispatchers -> [Listeners -> BOOLEAN]]

catalogVars == <<
    catalogMode, catalogPolicy, hasCatalog, epoch, warned, warnedEpoch,
    epochWarnCount, swapPhase, swapNonEmpty, swapEpoch, invalidTried
>>
ghostVars == <<lastValidMode, lastValidPolicy, lastValidEpoch>>
dispVars == <<
    dState, dEvent, dMode, dFw, dCount, dSnapEpoch,
    dRemaining, dInvoked, dSkippedClosed, dResult
>>
closeVars == <<inFlight, marked, closeCommitted, closing, cleaned, admittedAfterCommit>>
vars == <<catalogVars, ghostVars, dispVars, closeVars>>

Init ==
    /\ catalogMode = "none"
    /\ catalogPolicy = "warn"
    /\ hasCatalog = FALSE
    /\ epoch = 0
    /\ warned = {}
    /\ warnedEpoch = 0
    /\ epochWarnCount = [k \in WarnKeys |-> 0]
    /\ swapPhase = "idle"
    /\ swapNonEmpty = FALSE
    /\ swapEpoch = 0
    /\ invalidTried = FALSE
    /\ lastValidMode = "none"
    /\ lastValidPolicy = "warn"
    /\ lastValidEpoch = 0
    /\ dState = [d \in Dispatchers |-> "idle"]
    /\ dEvent = [d \in Dispatchers |-> "msg"]
    /\ dMode = [d \in Dispatchers |-> "emit"]
    /\ dFw = [d \in Dispatchers |-> FALSE]
    /\ dCount = [d \in Dispatchers |-> 0]
    /\ dSnapEpoch = [d \in Dispatchers |-> 0]
    /\ dRemaining = [d \in Dispatchers |-> {}]
    /\ dInvoked = [d \in Dispatchers |-> {}]
    /\ dSkippedClosed = [d \in Dispatchers |-> {}]
    /\ dResult = [d \in Dispatchers |-> "none"]
    /\ inFlight = {}
    /\ marked = {}
    /\ closeCommitted = {}
    /\ closing = {}
    /\ cleaned = {}
    /\ admittedAfterCommit = [d \in Dispatchers |-> [l \in Listeners |-> FALSE]]

(***************************************************************************)
(* Host catalog updates (bus.rs: set_event_mode_catalog, catalog.rs)       *)
(***************************************************************************)

\* Healthy atomic swap: under `catalog.write()`, increment `epoch`, install
\* the validated catalog, reset `warned` to the new epoch (lock order
\* `catalog -> warned`), and store `has_catalog = !catalog.is_empty()`.
SetValidCatalog(cfg) ==
    /\ Mutation # "nonAtomicSwap"
    /\ cfg \in CatalogConfigs
    /\ epoch < MaxEpoch
    /\ (cfg[1] # catalogMode \/ cfg[2] # catalogPolicy)
    /\ catalogMode' = cfg[1]
    /\ catalogPolicy' = cfg[2]
    /\ hasCatalog' = (cfg[1] # "none")
    /\ epoch' = epoch + 1
    /\ warned' = {}
    /\ warnedEpoch' = epoch + 1
    /\ epochWarnCount' = [k \in WarnKeys |-> 0]
    /\ invalidTried' = FALSE
    /\ lastValidMode' = cfg[1]
    /\ lastValidPolicy' = cfg[2]
    /\ lastValidEpoch' = epoch + 1
    /\ UNCHANGED <<swapPhase, swapNonEmpty, swapEpoch, dispVars, closeVars>>

\* Mutation "nonAtomicSwap": release `catalog.write()` between updating
\* `catalogMode`, storing `has_catalog`, and clearing `warned`, letting a
\* second swap or `CheckCatalog` / `RecordWarn` interleave between steps.
NonAtomicWriteCatalog(cfg) ==
    /\ Mutation = "nonAtomicSwap"
    /\ swapPhase = "idle"
    /\ cfg \in CatalogConfigs
    /\ epoch < MaxEpoch
    /\ (cfg[1] # catalogMode \/ cfg[2] # catalogPolicy)
    /\ catalogMode' = cfg[1]
    /\ catalogPolicy' = cfg[2]
    /\ epoch' = epoch + 1
    /\ epochWarnCount' = [k \in WarnKeys |-> 0]
    /\ swapPhase' = "wroteCatalog"
    /\ swapNonEmpty' = (cfg[1] # "none")
    /\ swapEpoch' = epoch + 1
    /\ invalidTried' = FALSE
    /\ lastValidMode' = cfg[1]
    /\ lastValidPolicy' = cfg[2]
    /\ lastValidEpoch' = epoch + 1
    /\ UNCHANGED <<hasCatalog, warned, warnedEpoch, dispVars, closeVars>>

NonAtomicInterleaveSwap(cfg) ==
    /\ Mutation = "nonAtomicSwap"
    /\ swapPhase # "idle"
    /\ cfg \in CatalogConfigs
    /\ epoch < MaxEpoch
    /\ (cfg[1] # catalogMode \/ cfg[2] # catalogPolicy)
    /\ catalogMode' = cfg[1]
    /\ catalogPolicy' = cfg[2]
    /\ hasCatalog' = (cfg[1] # "none")
    /\ epoch' = epoch + 1
    /\ warned' = {}
    /\ warnedEpoch' = epoch + 1
    /\ epochWarnCount' = [k \in WarnKeys |-> 0]
    /\ invalidTried' = FALSE
    /\ lastValidMode' = cfg[1]
    /\ lastValidPolicy' = cfg[2]
    /\ lastValidEpoch' = epoch + 1
    /\ UNCHANGED <<swapPhase, swapNonEmpty, swapEpoch, dispVars, closeVars>>

NonAtomicStoreFlag ==
    /\ Mutation = "nonAtomicSwap"
    /\ swapPhase = "wroteCatalog"
    /\ hasCatalog' = swapNonEmpty
    /\ swapPhase' = "storedFlag"
    /\ UNCHANGED <<catalogMode, catalogPolicy, epoch, warned, warnedEpoch,
                   epochWarnCount, swapNonEmpty, swapEpoch, invalidTried,
                   ghostVars, dispVars, closeVars>>

NonAtomicClearWarned ==
    /\ Mutation = "nonAtomicSwap"
    /\ swapPhase = "storedFlag"
    /\ warned' = {}
    /\ warnedEpoch' = swapEpoch
    /\ swapPhase' = "idle"
    /\ swapNonEmpty' = FALSE
    /\ swapEpoch' = 0
    /\ UNCHANGED <<catalogMode, catalogPolicy, hasCatalog, epoch,
                   epochWarnCount, invalidTried, ghostVars, dispVars, closeVars>>

\* Attempt to install a catalog containing an invalid key ("", "rutis/*", or
\* "internal/*"). Correct code validates before mutating, leaving the prior
\* valid catalog and epoch intact; Mutation "clearOnInvalid" clears it first.
SetInvalidCatalog ==
    /\ ~invalidTried
    /\ invalidTried' = TRUE
    /\ IF Mutation = "clearOnInvalid" /\ catalogMode # "none"
       THEN /\ catalogMode' = "none"
            /\ catalogPolicy' = "warn"
            /\ hasCatalog' = FALSE
            /\ warned' = {}
            /\ epochWarnCount' = [k \in WarnKeys |-> 0]
       ELSE UNCHANGED <<catalogMode, catalogPolicy, hasCatalog, warned, epochWarnCount>>
    /\ UNCHANGED <<epoch, warnedEpoch, swapPhase, swapNonEmpty, swapEpoch,
                   ghostVars, dispVars, closeVars>>

(***************************************************************************)
(* Concurrent dispatchers (bus.rs: snapshot_with, check_mode, start)       *)
(***************************************************************************)

BeginDispatch(d, job) ==
    /\ d \in Dispatchers
    /\ job \in Jobs
    /\ dState[d] = "idle"
    /\ dCount[d] < MaxDispatches
    /\ dState' = [dState EXCEPT ![d] = "checking"]
    /\ dEvent' = [dEvent EXCEPT ![d] = job[1]]
    /\ dMode' = [dMode EXCEPT ![d] = job[2]]
    /\ dFw' = [dFw EXCEPT ![d] = job[3]]
    /\ dCount' = [dCount EXCEPT ![d] = @ + 1]
    /\ dSnapEpoch' = [dSnapEpoch EXCEPT ![d] = 0]
    /\ dRemaining' = [dRemaining EXCEPT ![d] = Listeners]
    /\ dInvoked' = [dInvoked EXCEPT ![d] = {}]
    /\ dSkippedClosed' = [dSkippedClosed EXCEPT ![d] = {}]
    /\ dResult' = [dResult EXCEPT ![d] = "none"]
    /\ UNCHANGED <<catalogVars, ghostVars, closeVars>>

\* Mutation "checkAfterStart": mistakenly starts a listener BEFORE running
\* `check_mode`, allowing partial execution before an Error-policy rejection.
StartBeforeCheck(d, l) ==
    /\ Mutation = "checkAfterStart"
    /\ d \in Dispatchers
    /\ l \in Listeners
    /\ dState[d] = "checking"
    /\ l \in dRemaining[d]
    /\ l \notin marked /\ l \notin closing /\ l \notin cleaned
    /\ dRemaining' = [dRemaining EXCEPT ![d] = @ \ {l}]
    /\ dInvoked' = [dInvoked EXCEPT ![d] = @ \cup {l}]
    /\ inFlight' = inFlight \cup {<<d, l>>}
    /\ admittedAfterCommit' =
         [admittedAfterCommit EXCEPT ![d][l] = @ \/ (l \in closeCommitted)]
    /\ UNCHANGED <<catalogVars, ghostVars,
                   dState, dEvent, dMode, dFw, dCount, dSnapEpoch,
                   dSkippedClosed, dResult, marked, closeCommitted, closing, cleaned>>

\* Mode catalog preflight (`EventBus::check_mode` fast path + `catalog.read()`).
\* For Warn mismatches, releases `catalog.read()` carrying `dSnapEpoch[d]` and
\* enters `"recordingWarn"` so concurrent catalog swaps can interleave before
\* `warned.lock()` is acquired in `RecordWarn`.
CheckCatalog(d) ==
    /\ d \in Dispatchers
    /\ dState[d] = "checking"
    /\ LET activeMode == IF hasCatalog THEN catalogMode ELSE "none"
           fwBlocked == (dFw[d] /\ Mutation = "blockFramework" /\ activeMode # "none" /\ catalogPolicy = "error")
           userMismatch == (~dFw[d] /\ dEvent[d] = "msg" /\ activeMode # "none" /\ dMode[d] # activeMode)
       IN IF fwBlocked
          THEN /\ dState' = [dState EXCEPT ![d] = "rejected"]
               /\ dResult' = [dResult EXCEPT ![d] = "error"]
               /\ dRemaining' = [dRemaining EXCEPT ![d] = {}]
               /\ UNCHANGED dSnapEpoch
          ELSE IF userMismatch /\ catalogPolicy = "error"
          THEN /\ dState' = [dState EXCEPT ![d] = "rejected"]
               /\ dResult' = [dResult EXCEPT ![d] = "error"]
               /\ dRemaining' = [dRemaining EXCEPT ![d] = {}]
               /\ UNCHANGED dSnapEpoch
          ELSE IF userMismatch /\ catalogPolicy = "warn"
          THEN /\ dState' = [dState EXCEPT ![d] = "recordingWarn"]
               /\ dSnapEpoch' = [dSnapEpoch EXCEPT ![d] = epoch]
               /\ UNCHANGED <<dResult, dRemaining>>
          ELSE /\ dState' = [dState EXCEPT ![d] = "delivering"]
               /\ dResult' = [dResult EXCEPT ![d] = "ok"]
               /\ UNCHANGED <<dSnapEpoch, dRemaining>>
    /\ UNCHANGED <<catalogVars, ghostVars,
                   dEvent, dMode, dFw, dCount, dInvoked, dSkippedClosed, closeVars>>

\* Acquire `warned.lock()` after releasing `catalog.read()`: records `(d, mode)`
\* only if `warnedEpoch = dSnapEpoch[d]` (ignoring stale pre-swap checks so
\* they never pollute a newer epoch's deduplication table).
RecordWarn(d) ==
    /\ d \in Dispatchers
    /\ dState[d] = "recordingWarn"
    /\ LET k == <<d, dMode[d]>>
           sameEpoch == (Mutation = "nonAtomicSwap") \/ (warnedEpoch = dSnapEpoch[d])
           fresh == sameEpoch /\ (k \notin warned)
       IN /\ warned' = IF fresh THEN warned \cup {k} ELSE warned
          /\ epochWarnCount' =
               IF fresh
               THEN [epochWarnCount EXCEPT ![k] = @ + 1]
               ELSE epochWarnCount
          /\ dSnapEpoch' = [dSnapEpoch EXCEPT ![d] = 0]
          /\ IF Mutation = "warnDropsDelivery"
             THEN /\ dState' = [dState EXCEPT ![d] = "done"]
                  /\ dResult' = [dResult EXCEPT ![d] = "dropped"]
                  /\ dRemaining' = [dRemaining EXCEPT ![d] = {}]
             ELSE /\ dState' = [dState EXCEPT ![d] = "delivering"]
                  /\ dResult' = [dResult EXCEPT ![d] = "warned"]
                  /\ UNCHANGED dRemaining
    /\ UNCHANGED <<catalogMode, catalogPolicy, hasCatalog, epoch, warnedEpoch,
                   swapPhase, swapNonEmpty, swapEpoch, invalidTried, ghostVars,
                   dEvent, dMode, dFw, dCount, dInvoked, dSkippedClosed, closeVars>>

\* Deliver to one listener, checking two-phase close admission (`Entry::live`).
AdmitListener(d, l) ==
    /\ d \in Dispatchers
    /\ l \in Listeners
    /\ dState[d] = "delivering"
    /\ l \in dRemaining[d]
    /\ LET open == (l \notin marked /\ l \notin closing /\ l \notin cleaned)
       IN /\ dRemaining' = [dRemaining EXCEPT ![d] = @ \ {l}]
          /\ IF open
             THEN /\ dInvoked' = [dInvoked EXCEPT ![d] = @ \cup {l}]
                  /\ inFlight' = inFlight \cup {<<d, l>>}
                  /\ admittedAfterCommit' =
                       [admittedAfterCommit EXCEPT ![d][l] = @ \/ (l \in closeCommitted)]
                  /\ UNCHANGED dSkippedClosed
             ELSE /\ dSkippedClosed' = [dSkippedClosed EXCEPT ![d] = @ \cup {l}]
                  /\ UNCHANGED <<dInvoked, inFlight, admittedAfterCommit>>
    /\ UNCHANGED <<catalogVars, ghostVars,
                   dState, dEvent, dMode, dFw, dCount, dSnapEpoch, dResult,
                   marked, closeCommitted, closing, cleaned>>

FinishDelivery(d) ==
    /\ d \in Dispatchers
    /\ dState[d] = "delivering"
    /\ dRemaining[d] = {}
    /\ dState' = [dState EXCEPT ![d] = "done"]
    /\ UNCHANGED <<catalogVars, ghostVars,
                   dEvent, dMode, dFw, dCount, dSnapEpoch, dRemaining,
                   dInvoked, dSkippedClosed, dResult, closeVars>>

CompleteCall(d, l) ==
    /\ <<d, l>> \in inFlight
    /\ inFlight' = inFlight \ {<<d, l>>}
    /\ UNCHANGED <<catalogVars, ghostVars, dispVars,
                   marked, closeCommitted, closing, cleaned, admittedAfterCommit>>

\* Once a dispatch finished (or was rejected) and its calls drained, the
\* dispatcher may begin another dispatch (up to MaxDispatches) to exercise
\* same-epoch Warn deduplication and cross-epoch reset.
ResetDispatcher(d) ==
    /\ d \in Dispatchers
    /\ dState[d] \in {"done", "rejected"}
    /\ dCount[d] < MaxDispatches
    /\ ~(\E l \in Listeners : <<d, l>> \in inFlight)
    /\ dState' = [dState EXCEPT ![d] = "idle"]
    /\ dSnapEpoch' = [dSnapEpoch EXCEPT ![d] = 0]
    /\ dInvoked' = [dInvoked EXCEPT ![d] = {}]
    /\ dSkippedClosed' = [dSkippedClosed EXCEPT ![d] = {}]
    /\ dResult' = [dResult EXCEPT ![d] = "none"]
    /\ UNCHANGED <<catalogVars, ghostVars,
                   dEvent, dMode, dFw, dCount, dRemaining, closeVars>>

(***************************************************************************)
(* Two-phase close admission & drain on Closable listener (fiber.rs)       *)
(***************************************************************************)

CloseMark ==
    /\ Closable \notin marked
    /\ marked' = marked \cup {Closable}
    /\ closeCommitted' = closeCommitted \cup {Closable}
    /\ UNCHANGED <<catalogVars, ghostVars, dispVars,
                   inFlight, closing, cleaned, admittedAfterCommit>>

ClosePublish ==
    /\ Closable \in marked
    /\ Closable \notin closing
    /\ closing' = closing \cup {Closable}
    /\ UNCHANGED <<catalogVars, ghostVars, dispVars,
                   inFlight, marked, closeCommitted, cleaned, admittedAfterCommit>>

CloseCleanup ==
    /\ Closable \in closing
    /\ Closable \notin cleaned
    /\ ~(\E d \in Dispatchers : <<d, Closable>> \in inFlight)
    /\ cleaned' = cleaned \cup {Closable}
    /\ UNCHANGED <<catalogVars, ghostVars, dispVars,
                   inFlight, marked, closeCommitted, closing, admittedAfterCommit>>

CompleteSome == \E d \in Dispatchers, l \in Listeners : CompleteCall(d, l)
ProgressDispatch == \E d \in Dispatchers :
    \/ CheckCatalog(d)
    \/ RecordWarn(d)
    \/ (\E l \in Listeners : AdmitListener(d, l))
    \/ FinishDelivery(d)

Next ==
    \/ \E cfg \in CatalogConfigs : SetValidCatalog(cfg)
    \/ \E cfg \in CatalogConfigs : NonAtomicWriteCatalog(cfg)
    \/ \E cfg \in CatalogConfigs : NonAtomicInterleaveSwap(cfg)
    \/ NonAtomicStoreFlag
    \/ NonAtomicClearWarned
    \/ SetInvalidCatalog
    \/ \E d \in Dispatchers, job \in Jobs : BeginDispatch(d, job)
    \/ \E d \in Dispatchers, l \in Listeners : StartBeforeCheck(d, l)
    \/ \E d \in Dispatchers : CheckCatalog(d)
    \/ \E d \in Dispatchers : RecordWarn(d)
    \/ \E d \in Dispatchers, l \in Listeners : AdmitListener(d, l)
    \/ \E d \in Dispatchers : FinishDelivery(d)
    \/ CompleteSome
    \/ \E d \in Dispatchers : ResetDispatcher(d)
    \/ CloseMark
    \/ ClosePublish
    \/ CloseCleanup

Spec == Init /\ [][Next]_vars /\ WF_vars(CompleteSome) /\ WF_vars(ProgressDispatch)

(***************************************************************************)
(* Safety invariants & liveness                                            *)
(***************************************************************************)

TypeOK ==
    /\ catalogMode \in {"none", "bail"}
    /\ catalogPolicy \in {"warn", "error"}
    /\ hasCatalog \in BOOLEAN
    /\ epoch \in 0..MaxEpoch
    /\ warnedEpoch \in 0..MaxEpoch
    /\ swapPhase \in {"idle", "wroteCatalog", "storedFlag"}
    /\ swapNonEmpty \in BOOLEAN
    /\ swapEpoch \in 0..MaxEpoch
    /\ warned \subseteq WarnKeys
    /\ dState \in [Dispatchers -> {"idle", "checking", "recordingWarn", "delivering", "done", "rejected"}]
    /\ dSnapEpoch \in [Dispatchers -> 0..MaxEpoch]
    /\ inFlight \subseteq (Dispatchers \X Listeners)
    /\ marked \subseteq Listeners
    /\ closeCommitted \subseteq Listeners
    /\ closing \subseteq Listeners
    /\ cleaned \subseteq Listeners

\* Under Error policy, a mismatched dispatch invokes 0 listeners and leaves
\* 0 in-flight calls.
NoPartialExecOnError ==
    \A d \in Dispatchers :
        dState[d] = "rejected" =>
            /\ dInvoked[d] = {}
            /\ ~(\E l \in Listeners : <<d, l>> \in inFlight)

\* Invalid catalog updates ("", rutis/*, internal/*) never overwrite or clear
\* the prior valid catalog or epoch.
AtomicCatalogValidation ==
    /\ catalogMode = lastValidMode
    /\ catalogPolicy = lastValidPolicy
    /\ epoch = lastValidEpoch

\* Fast-path `hasCatalog` stays synchronized with whether a catalog is
\* installed, `warned` stays bound to the current epoch, and no epoch warning
\* is lost (warned without emitting) or emitted twice in the same epoch.
CatalogFlagInSync ==
    /\ ((catalogMode # "none") <=> hasCatalog)
    /\ (warnedEpoch = epoch)
    /\ \A k \in WarnKeys :
         IF k \in warned
         THEN epochWarnCount[k] = 1
         ELSE epochWarnCount[k] = 0

\* Under Warn policy, mismatched dispatches still invoke every eligible live
\* listener identically to an empty catalog, and warn at most once per
\* (emitter, event, actual, expected) per catalog epoch.
WarnPreservesDelivery ==
    /\ \A d \in Dispatchers :
         (dState[d] = "done" /\ dResult[d] \in {"warned", "dropped"}) =>
             /\ dResult[d] = "warned"
             /\ \A l \in Listeners :
                  (l \notin dSkippedClosed[d]) => l \in dInvoked[d]
    /\ \A k \in WarnKeys : epochWarnCount[k] <= 1

\* Framework dispatches (`rutis/*`, `opts.framework = TRUE`) are never
\* rejected by user catalog entries.
FrameworkNeverBlocked ==
    \A d \in Dispatchers :
        dFw[d] => dState[d] # "rejected"

\* Catalog checks and swaps never admit a listener after close commit, never
\* run cleanup with an in-flight call, and preserve close ordering.
CloseDrainPreserved ==
    /\ \A d \in Dispatchers, l \in Listeners : ~admittedAfterCommit[d][l]
    /\ \A l \in cleaned : ~(\E d \in Dispatchers : <<d, l>> \in inFlight)
    /\ cleaned \subseteq closing
    /\ closing \subseteq marked

\* Liveness: every admitted in-flight call eventually completes (no leaked flight).
AllInFlightDrain == []<>(inFlight = {})

=============================================================================
