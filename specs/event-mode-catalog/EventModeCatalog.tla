--------------------------- MODULE EventModeCatalog ---------------------------
(***************************************************************************)
(* Optional event mode catalog and runtime bus enforcement (#1324):        *)
(* concurrent dispatchers (user emit/bail/serial and framework `rutis/*`), *)
(* host catalog swaps (valid and invalid), Warn deduplication epochs, and  *)
(* two-phase listener close admission / drain.                             *)
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
    "warnDropsDelivery"
}
ASSUME MaxEpoch \in Nat /\ MaxEpoch >= 1
ASSUME MaxDispatches \in Nat /\ MaxDispatches >= 1

VARIABLES
    \* Catalog state (bus.rs: has_catalog, catalog, warned)
    catalogMode,         \* "none" | "bail" (expected mode for "msg")
    catalogPolicy,       \* "warn" | "error"
    epoch,               \* 0..MaxEpoch, incremented on each valid swap
    warned,              \* subset of WarnKeys already warned in current epoch
    epochWarnCount,      \* [WarnKeys -> Nat]: warnings emitted in current epoch
    invalidTried,        \* whether SetInvalidCatalog has been exercised in current epoch

    \* Ghost: last successfully validated catalog configuration and epoch
    lastValidMode,
    lastValidPolicy,
    lastValidEpoch,

    \* Per-dispatcher state
    dState,              \* [Dispatchers -> "idle" | "checking" | "delivering" | "done" | "rejected"]
    dEvent,              \* [Dispatchers -> {"msg", "other", "rutis/row"}]
    dMode,               \* [Dispatchers -> {"emit", "serial", "bail"}]
    dFw,                 \* [Dispatchers -> BOOLEAN]
    dCount,              \* [Dispatchers -> 0..MaxDispatches]
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

vars == <<
    catalogMode, catalogPolicy, epoch, warned, epochWarnCount, invalidTried,
    lastValidMode, lastValidPolicy, lastValidEpoch,
    dState, dEvent, dMode, dFw, dCount, dRemaining, dInvoked, dSkippedClosed, dResult,
    inFlight, marked, closeCommitted, closing, cleaned, admittedAfterCommit
>>

Init ==
    /\ catalogMode = "none"
    /\ catalogPolicy = "warn"
    /\ epoch = 0
    /\ warned = {}
    /\ epochWarnCount = [k \in WarnKeys |-> 0]
    /\ invalidTried = FALSE
    /\ lastValidMode = "none"
    /\ lastValidPolicy = "warn"
    /\ lastValidEpoch = 0
    /\ dState = [d \in Dispatchers |-> "idle"]
    /\ dEvent = [d \in Dispatchers |-> "msg"]
    /\ dMode = [d \in Dispatchers |-> "emit"]
    /\ dFw = [d \in Dispatchers |-> FALSE]
    /\ dCount = [d \in Dispatchers |-> 0]
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

\* Install a valid catalog (or clear with "none"), advancing epoch and
\* resetting the Warn deduplication set.
SetValidCatalog(cfg) ==
    /\ cfg \in CatalogConfigs
    /\ epoch < MaxEpoch
    /\ (cfg[1] # catalogMode \/ cfg[2] # catalogPolicy)
    /\ catalogMode' = cfg[1]
    /\ catalogPolicy' = cfg[2]
    /\ epoch' = epoch + 1
    /\ warned' = {}
    /\ epochWarnCount' = [k \in WarnKeys |-> 0]
    /\ invalidTried' = FALSE
    /\ lastValidMode' = cfg[1]
    /\ lastValidPolicy' = cfg[2]
    /\ lastValidEpoch' = epoch + 1
    /\ UNCHANGED <<dState, dEvent, dMode, dFw, dCount, dRemaining, dInvoked,
                   dSkippedClosed, dResult, inFlight, marked, closeCommitted,
                   closing, cleaned, admittedAfterCommit>>

\* Attempt to install a catalog containing an invalid key ("", "rutis/*", or
\* "internal/*"). Correct code validates before mutating, leaving the prior
\* valid catalog and epoch intact; Mutation "clearOnInvalid" clears it first.
SetInvalidCatalog ==
    /\ ~invalidTried
    /\ invalidTried' = TRUE
    /\ IF Mutation = "clearOnInvalid" /\ catalogMode # "none"
       THEN /\ catalogMode' = "none"
            /\ catalogPolicy' = "warn"
            /\ warned' = {}
            /\ epochWarnCount' = [k \in WarnKeys |-> 0]
       ELSE UNCHANGED <<catalogMode, catalogPolicy, warned, epochWarnCount>>
    /\ UNCHANGED <<epoch, lastValidMode, lastValidPolicy, lastValidEpoch,
                   dState, dEvent, dMode, dFw, dCount, dRemaining, dInvoked,
                   dSkippedClosed, dResult, inFlight, marked, closeCommitted,
                   closing, cleaned, admittedAfterCommit>>

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
    /\ dRemaining' = [dRemaining EXCEPT ![d] = Listeners]
    /\ dInvoked' = [dInvoked EXCEPT ![d] = {}]
    /\ dSkippedClosed' = [dSkippedClosed EXCEPT ![d] = {}]
    /\ dResult' = [dResult EXCEPT ![d] = "none"]
    /\ UNCHANGED <<catalogMode, catalogPolicy, epoch, warned, epochWarnCount,
                   invalidTried, lastValidMode, lastValidPolicy, lastValidEpoch,
                   inFlight, marked, closeCommitted, closing, cleaned,
                   admittedAfterCommit>>

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
    /\ UNCHANGED <<catalogMode, catalogPolicy, epoch, warned, epochWarnCount,
                   invalidTried, lastValidMode, lastValidPolicy, lastValidEpoch,
                   dState, dEvent, dMode, dFw, dCount, dSkippedClosed, dResult,
                   marked, closeCommitted, closing, cleaned>>

\* Mode catalog preflight (`EventBus::check_mode` inside `snapshot_with`).
CheckCatalog(d) ==
    /\ d \in Dispatchers
    /\ dState[d] = "checking"
    /\ LET fwBlocked == (dFw[d] /\ Mutation = "blockFramework" /\ catalogMode # "none" /\ catalogPolicy = "error")
           userMismatch == (~dFw[d] /\ dEvent[d] = "msg" /\ catalogMode # "none" /\ dMode[d] # catalogMode)
       IN IF fwBlocked
          THEN /\ dState' = [dState EXCEPT ![d] = "rejected"]
               /\ dResult' = [dResult EXCEPT ![d] = "error"]
               /\ dRemaining' = [dRemaining EXCEPT ![d] = {}]
               /\ UNCHANGED <<warned, epochWarnCount>>
          ELSE IF userMismatch /\ catalogPolicy = "error"
          THEN /\ dState' = [dState EXCEPT ![d] = "rejected"]
               /\ dResult' = [dResult EXCEPT ![d] = "error"]
               /\ dRemaining' = [dRemaining EXCEPT ![d] = {}]
               /\ UNCHANGED <<warned, epochWarnCount>>
          ELSE IF userMismatch /\ catalogPolicy = "warn"
          THEN LET k == <<d, dMode[d]>>
                   fresh == k \notin warned
               IN /\ warned' = warned \cup {k}
                  /\ epochWarnCount' =
                       IF fresh
                       THEN [epochWarnCount EXCEPT ![k] = @ + 1]
                       ELSE epochWarnCount
                  /\ IF Mutation = "warnDropsDelivery"
                     THEN /\ dState' = [dState EXCEPT ![d] = "done"]
                          /\ dResult' = [dResult EXCEPT ![d] = "dropped"]
                          /\ dRemaining' = [dRemaining EXCEPT ![d] = {}]
                     ELSE /\ dState' = [dState EXCEPT ![d] = "delivering"]
                          /\ dResult' = [dResult EXCEPT ![d] = "warned"]
                          /\ UNCHANGED dRemaining
          ELSE /\ dState' = [dState EXCEPT ![d] = "delivering"]
               /\ dResult' = [dResult EXCEPT ![d] = "ok"]
               /\ UNCHANGED <<warned, epochWarnCount, dRemaining>>
    /\ UNCHANGED <<catalogMode, catalogPolicy, epoch, invalidTried,
                   lastValidMode, lastValidPolicy, lastValidEpoch,
                   dEvent, dMode, dFw, dCount, dInvoked, dSkippedClosed,
                   inFlight, marked, closeCommitted, closing, cleaned,
                   admittedAfterCommit>>

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
    /\ UNCHANGED <<catalogMode, catalogPolicy, epoch, warned, epochWarnCount,
                   invalidTried, lastValidMode, lastValidPolicy, lastValidEpoch,
                   dState, dEvent, dMode, dFw, dCount, dResult,
                   marked, closeCommitted, closing, cleaned>>

FinishDelivery(d) ==
    /\ d \in Dispatchers
    /\ dState[d] = "delivering"
    /\ dRemaining[d] = {}
    /\ dState' = [dState EXCEPT ![d] = "done"]
    /\ UNCHANGED <<catalogMode, catalogPolicy, epoch, warned, epochWarnCount,
                   invalidTried, lastValidMode, lastValidPolicy, lastValidEpoch,
                   dEvent, dMode, dFw, dCount, dRemaining, dInvoked,
                   dSkippedClosed, dResult, inFlight, marked, closeCommitted,
                   closing, cleaned, admittedAfterCommit>>

CompleteCall(d, l) ==
    /\ <<d, l>> \in inFlight
    /\ inFlight' = inFlight \ {<<d, l>>}
    /\ UNCHANGED <<catalogMode, catalogPolicy, epoch, warned, epochWarnCount,
                   invalidTried, lastValidMode, lastValidPolicy, lastValidEpoch,
                   dState, dEvent, dMode, dFw, dCount, dRemaining, dInvoked,
                   dSkippedClosed, dResult, marked, closeCommitted, closing,
                   cleaned, admittedAfterCommit>>

\* Once a dispatch finished (or was rejected) and its calls drained, the
\* dispatcher may begin another dispatch (up to MaxDispatches) to exercise
\* same-epoch Warn deduplication and cross-epoch reset.
ResetDispatcher(d) ==
    /\ d \in Dispatchers
    /\ dState[d] \in {"done", "rejected"}
    /\ dCount[d] < MaxDispatches
    /\ ~(\E l \in Listeners : <<d, l>> \in inFlight)
    /\ dState' = [dState EXCEPT ![d] = "idle"]
    /\ dInvoked' = [dInvoked EXCEPT ![d] = {}]
    /\ dSkippedClosed' = [dSkippedClosed EXCEPT ![d] = {}]
    /\ dResult' = [dResult EXCEPT ![d] = "none"]
    /\ UNCHANGED <<catalogMode, catalogPolicy, epoch, warned, epochWarnCount,
                   invalidTried, lastValidMode, lastValidPolicy, lastValidEpoch,
                   dEvent, dMode, dFw, dCount, dRemaining, inFlight,
                   marked, closeCommitted, closing, cleaned, admittedAfterCommit>>

(***************************************************************************)
(* Two-phase close admission & drain on Closable listener (fiber.rs)       *)
(***************************************************************************)

CloseMark ==
    /\ Closable \notin marked
    /\ marked' = marked \cup {Closable}
    /\ closeCommitted' = closeCommitted \cup {Closable}
    /\ UNCHANGED <<catalogMode, catalogPolicy, epoch, warned, epochWarnCount,
                   invalidTried, lastValidMode, lastValidPolicy, lastValidEpoch,
                   dState, dEvent, dMode, dFw, dCount, dRemaining, dInvoked,
                   dSkippedClosed, dResult, inFlight, closing, cleaned,
                   admittedAfterCommit>>

ClosePublish ==
    /\ Closable \in marked
    /\ Closable \notin closing
    /\ closing' = closing \cup {Closable}
    /\ UNCHANGED <<catalogMode, catalogPolicy, epoch, warned, epochWarnCount,
                   invalidTried, lastValidMode, lastValidPolicy, lastValidEpoch,
                   dState, dEvent, dMode, dFw, dCount, dRemaining, dInvoked,
                   dSkippedClosed, dResult, inFlight, marked, closeCommitted,
                   cleaned, admittedAfterCommit>>

CloseCleanup ==
    /\ Closable \in closing
    /\ Closable \notin cleaned
    /\ ~(\E d \in Dispatchers : <<d, Closable>> \in inFlight)
    /\ cleaned' = cleaned \cup {Closable}
    /\ UNCHANGED <<catalogMode, catalogPolicy, epoch, warned, epochWarnCount,
                   invalidTried, lastValidMode, lastValidPolicy, lastValidEpoch,
                   dState, dEvent, dMode, dFw, dCount, dRemaining, dInvoked,
                   dSkippedClosed, dResult, inFlight, marked, closeCommitted,
                   closing, admittedAfterCommit>>

CompleteSome == \E d \in Dispatchers, l \in Listeners : CompleteCall(d, l)
ProgressDispatch == \E d \in Dispatchers :
    \/ CheckCatalog(d)
    \/ (\E l \in Listeners : AdmitListener(d, l))
    \/ FinishDelivery(d)

Next ==
    \/ \E cfg \in CatalogConfigs : SetValidCatalog(cfg)
    \/ SetInvalidCatalog
    \/ \E d \in Dispatchers, job \in Jobs : BeginDispatch(d, job)
    \/ \E d \in Dispatchers, l \in Listeners : StartBeforeCheck(d, l)
    \/ \E d \in Dispatchers : CheckCatalog(d)
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
    /\ epoch \in 0..MaxEpoch
    /\ warned \subseteq WarnKeys
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
