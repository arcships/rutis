---------------------------- MODULE ManagedTimers ----------------------------
(***************************************************************************)
(* Managed timers (`ctx.timeout`, `ctx.interval`) built on `effect`        *)
(* (#1325, docs/design-managed-timers-2026-11-20.en.md).                   *)
(*                                                                         *)
(* Models one plugin row holding an earlier resource (`pool`: open/closed) *)
(* registered on its LIFO unload stack BEFORE a managed timer              *)
(* (`[poolCleanup, timerCleanup]`).                                        *)
(*                                                                         *)
(* Explores all interleavings of:                                          *)
(*   - Timer firing (`scheduled` -> `invoking_sync` -> sync finish or      *)
(*     `in_flight_async` -> `idle` / `done`),                              *)
(*   - Manual cancellation (external or synchronous self-cancel during     *)
(*     `invoking_sync`, repeatable for idempotence),                       *)
(*   - Row unload (`active` -> `unloading` -> LIFO `timerCleanup` ->       *)
(*     `poolCleanup` -> `disposed`),                                       *)
(*   - Both abortable runtimes (Rust `tokio::select!` + `TimerExitSignal`, *)
(*     Python `task.cancel()` + `await task`) and non-abortable Promise    *)
(*     runtimes (Node leaf / Bun awaiting `running` before `pool.close`).  *)
(***************************************************************************)
EXTENDS Naturals, TLC

CONSTANTS
    Kind,       \* "timeout" | "interval"
    Mode,       \* "abortable" | "nonAbortable"
    Mutation,   \* "none" | "selfCancelDropsStep" | "noAwaitInFlight" | "overlapTick" | "fireAfterUnload" | "timeoutLeaksStack"
    MaxTicks    \* 1 for timeout, 2 for interval

ASSUME Kind \in {"timeout", "interval"}
ASSUME Mode \in {"abortable", "nonAbortable"}
ASSUME Mutation \in {"none", "selfCancelDropsStep", "noAwaitInFlight",
                     "overlapTick", "fireAfterUnload", "timeoutLeaksStack"}
ASSUME MaxTicks \in Nat /\ MaxTicks >= 1

VARIABLES
    rowState,           \* "active" | "unloading" | "disposed"
    unloadStage,        \* "none" | "timer" | "pool" | "done"
    pool,               \* "open" | "closed"
    timerState,         \* "scheduled" | "invoking_sync" | "in_flight_async" | "idle" | "done"
    inFlight,           \* 0..2: number of active callback executions
    onStack,            \* BOOLEAN: timerCleanup still present on unload stack
    cancelled,          \* BOOLEAN: timer cancelled (manual, self-cancel, or unload)
    cancelCalls,        \* 0..2: repeated manual/self cancel counter
    ticks,              \* 0..MaxTicks: callbacks started so far
    cleanupRuns,        \* 0..2: how many times timerCleanup entered/ran on unload
    cleanupEntered,     \* BOOLEAN: unload has entered timerCleanup
    usedPoolAfterClose, \* ghost: callback accessed pool after pool = "closed"
    firedAfterUnload    \* ghost: callback started after rowState /= "active"

vars == <<rowState, unloadStage, pool, timerState, inFlight, onStack,
          cancelled, cancelCalls, ticks, cleanupRuns, cleanupEntered,
          usedPoolAfterClose, firedAfterUnload>>

Init ==
    /\ rowState = "active"
    /\ unloadStage = "none"
    /\ pool = "open"
    /\ timerState = "scheduled"
    /\ inFlight = 0
    /\ onStack = TRUE
    /\ cancelled = FALSE
    /\ cancelCalls = 0
    /\ ticks = 0
    /\ cleanupRuns = 0
    /\ cleanupEntered = FALSE
    /\ usedPoolAfterClose = FALSE
    /\ firedAfterUnload = FALSE

(***************************************************************************)
(* Timer execution: scheduled -> invoking_sync -> sync / in_flight_async   *)
(***************************************************************************)

TimerFire ==
    /\ timerState = "scheduled"
    /\ ticks < MaxTicks
    /\ IF Mutation = "fireAfterUnload"
       THEN ~cancelled
       ELSE ~cancelled /\ rowState = "active"
    /\ timerState' = "invoking_sync"
    /\ inFlight' = inFlight + 1
    /\ ticks' = ticks + 1
    /\ firedAfterUnload' = (firedAfterUnload \/ rowState /= "active")
    /\ usedPoolAfterClose' = (usedPoolAfterClose \/ pool = "closed")
    /\ UNCHANGED <<rowState, unloadStage, pool, onStack, cancelled,
                   cancelCalls, cleanupRuns, cleanupEntered>>

\* Callback synchronously calls its own cancel() handle BEFORE returning its
\* Promise / Future. Healthy JS keeps `invoking = true` (Python/Rust keep
\* task/record active), so `onStack` stays TRUE; `selfCancelDropsStep`
\* reproduces the bug `if (!running) removeStep(cleanup)` while `running` is
\* still undefined.
SyncSelfCancel ==
    /\ timerState = "invoking_sync"
    /\ cancelCalls < 2
    /\ cancelled' = TRUE
    /\ cancelCalls' = cancelCalls + 1
    /\ onStack' = IF Mutation = "selfCancelDropsStep" THEN FALSE ELSE onStack
    /\ UNCHANGED <<rowState, unloadStage, pool, timerState, inFlight, ticks,
                   cleanupRuns, cleanupEntered, usedPoolAfterClose, firedAfterUnload>>

\* Synchronous callback finishes (or throws a contained error).
SyncComplete ==
    /\ timerState = "invoking_sync"
    /\ usedPoolAfterClose' = (usedPoolAfterClose \/ pool = "closed")
    /\ inFlight' = inFlight - 1
    /\ IF Kind = "timeout" \/ cancelled \/ rowState /= "active"
       THEN /\ timerState' = "done"
            /\ onStack' = IF Mutation = "timeoutLeaksStack" /\ Kind = "timeout" /\ ~cancelled /\ rowState = "active"
                          THEN onStack
                          ELSE FALSE
       ELSE /\ timerState' = "idle"
            /\ UNCHANGED onStack
    /\ UNCHANGED <<rowState, unloadStage, pool, cancelled, cancelCalls, ticks,
                   cleanupRuns, cleanupEntered, firedAfterUnload>>

\* Async callback suspends at its first `await`, returning a Promise / Future.
ReturnAsync ==
    /\ timerState = "invoking_sync"
    /\ timerState' = "in_flight_async"
    /\ UNCHANGED <<rowState, unloadStage, pool, inFlight, onStack, cancelled,
                   cancelCalls, ticks, cleanupRuns, cleanupEntered,
                   usedPoolAfterClose, firedAfterUnload>>

\* Deliberate mutation only: fixed-rate setInterval firing a second tick while
\* the first async tick is still in flight.
OverlapFire ==
    /\ Mutation = "overlapTick"
    /\ Kind = "interval"
    /\ timerState = "in_flight_async"
    /\ inFlight = 1
    /\ ~cancelled
    /\ rowState = "active"
    /\ ticks < MaxTicks
    /\ inFlight' = inFlight + 1
    /\ ticks' = ticks + 1
    /\ usedPoolAfterClose' = (usedPoolAfterClose \/ pool = "closed")
    /\ UNCHANGED <<rowState, unloadStage, pool, timerState, onStack, cancelled,
                   cancelCalls, cleanupRuns, cleanupEntered, firedAfterUnload>>

\* In-flight async callback finishes (normal return or contained rejection),
\* touching `pool` before settling.
AsyncComplete ==
    /\ timerState = "in_flight_async"
    /\ usedPoolAfterClose' = (usedPoolAfterClose \/ pool = "closed")
    /\ inFlight' = inFlight - 1
    /\ IF inFlight - 1 = 0
       THEN IF Kind = "timeout" \/ cancelled \/ rowState /= "active"
            THEN /\ timerState' = "done"
                 /\ onStack' = IF Mutation = "timeoutLeaksStack" /\ Kind = "timeout" /\ ~cancelled /\ rowState = "active"
                               THEN onStack
                               ELSE FALSE
            ELSE /\ timerState' = "idle"
                 /\ UNCHANGED onStack
       ELSE /\ timerState' = "in_flight_async"
            /\ UNCHANGED onStack
    /\ UNCHANGED <<rowState, unloadStage, pool, cancelled, cancelCalls, ticks,
                   cleanupRuns, cleanupEntered, firedAfterUnload>>

\* Abortable runtimes only (Rust tokio::select! drop, Python task.cancel()):
\* cancels an in-flight async callback at its await point and runs Drop/finally.
AsyncAbort ==
    /\ Mode = "abortable"
    /\ timerState = "in_flight_async"
    /\ (cancelled \/ rowState /= "active")
    /\ usedPoolAfterClose' = (usedPoolAfterClose \/ pool = "closed")
    /\ inFlight' = 0
    /\ timerState' = "done"
    /\ onStack' = FALSE
    /\ UNCHANGED <<rowState, unloadStage, pool, cancelled, cancelCalls, ticks,
                   cleanupRuns, cleanupEntered, firedAfterUnload>>

\* Fixed-delay interval schedules the next wait ONLY after the previous tick
\* settled and transitioned to `idle`.
ScheduleNext ==
    /\ Kind = "interval"
    /\ timerState = "idle"
    /\ IF cancelled \/ rowState /= "active"
       THEN /\ timerState' = "done"
            /\ onStack' = FALSE
       ELSE /\ timerState' = "scheduled"
            /\ UNCHANGED onStack
    /\ UNCHANGED <<rowState, unloadStage, pool, inFlight, cancelled,
                   cancelCalls, ticks, cleanupRuns, cleanupEntered,
                   usedPoolAfterClose, firedAfterUnload>>

(***************************************************************************)
(* External manual cancel (idempotent, may repeat up to 2 calls)           *)
(***************************************************************************)

ManualCancel ==
    /\ timerState /= "invoking_sync"
    /\ cancelCalls < 2
    /\ cancelled' = TRUE
    /\ cancelCalls' = cancelCalls + 1
    /\ IF timerState \in {"scheduled", "idle"}
       THEN /\ timerState' = "done"
            /\ onStack' = FALSE
       ELSE IF timerState = "done"
            THEN /\ timerState' = "done"
                 /\ onStack' = FALSE
            ELSE \* in_flight_async: keep onStack intact so unload still waits
                 /\ UNCHANGED <<timerState, onStack>>
    /\ UNCHANGED <<rowState, unloadStage, pool, inFlight, ticks, cleanupRuns,
                   cleanupEntered, usedPoolAfterClose, firedAfterUnload>>

(***************************************************************************)
(* LIFO unload stack: timerCleanup pops BEFORE poolCleanup                 *)
(***************************************************************************)

StartUnload ==
    /\ rowState = "active"
    \* Single-threaded JS runtimes cannot preempt the synchronous prologue
    \* `invoking_sync`; multi-threaded abortable runtimes (Rust) can.
    /\ (Mode = "abortable" \/ timerState /= "invoking_sync")
    /\ rowState' = "unloading"
    /\ unloadStage' = "timer"
    /\ UNCHANGED <<pool, timerState, inFlight, onStack, cancelled, cancelCalls,
                   ticks, cleanupRuns, cleanupEntered, usedPoolAfterClose,
                   firedAfterUnload>>

\* Unload reaches timerCleanup while callback is active: signals cancellation
\* and waits without advancing to poolCleanup.
UnloadSignalTimer ==
    /\ rowState = "unloading"
    /\ unloadStage = "timer"
    /\ onStack
    /\ timerState \in {"invoking_sync", "in_flight_async"}
    /\ (~cancelled \/ ~cleanupEntered)
    /\ cancelled' = TRUE
    /\ cleanupEntered' = TRUE
    /\ cleanupRuns' = IF cleanupEntered THEN cleanupRuns ELSE cleanupRuns + 1
    /\ UNCHANGED <<rowState, unloadStage, pool, timerState, inFlight, onStack,
                   cancelCalls, ticks, usedPoolAfterClose, firedAfterUnload>>

\* Unload finishes timerCleanup and advances to poolCleanup.
UnloadTimerStep ==
    /\ rowState = "unloading"
    /\ unloadStage = "timer"
    /\ \/ \* Already detached (completed timeout, early cancel while idle, or
          \* prematurely dropped by selfCancelDropsStep mutation).
          /\ ~onStack
          /\ unloadStage' = "pool"
          /\ UNCHANGED <<timerState, onStack, cancelled, cleanupRuns, cleanupEntered>>
       \/ \* Timer is not in flight: cancel/clear immediately and detach.
          /\ onStack
          /\ timerState \in {"scheduled", "idle", "done"}
          /\ cancelled' = TRUE
          /\ timerState' = "done"
          /\ onStack' = FALSE
          /\ cleanupEntered' = TRUE
          /\ cleanupRuns' = IF cleanupEntered THEN cleanupRuns ELSE cleanupRuns + 1
          /\ unloadStage' = "pool"
       \/ \* Deliberate mutation: advance to pool.close() without awaiting in-flight tick.
          /\ Mutation = "noAwaitInFlight"
          /\ onStack
          /\ timerState = "in_flight_async"
          /\ cancelled' = TRUE
          /\ onStack' = FALSE
          /\ cleanupEntered' = TRUE
          /\ cleanupRuns' = IF cleanupEntered THEN cleanupRuns ELSE cleanupRuns + 1
          /\ unloadStage' = "pool"
          /\ UNCHANGED timerState
    /\ UNCHANGED <<rowState, pool, inFlight, cancelCalls, ticks,
                   usedPoolAfterClose, firedAfterUnload>>

\* Unload pops the earlier resource cleanup (`pool.close()`).
UnloadPoolStep ==
    /\ rowState = "unloading"
    /\ unloadStage = "pool"
    /\ pool' = "closed"
    /\ rowState' = "disposed"
    /\ unloadStage' = "done"
    /\ UNCHANGED <<timerState, inFlight, onStack, cancelled, cancelCalls,
                   ticks, cleanupRuns, cleanupEntered, usedPoolAfterClose,
                   firedAfterUnload>>

Next ==
    \/ TimerFire
    \/ SyncSelfCancel
    \/ SyncComplete
    \/ ReturnAsync
    \/ OverlapFire
    \/ AsyncComplete
    \/ AsyncAbort
    \/ ScheduleNext
    \/ ManualCancel
    \/ StartUnload
    \/ UnloadSignalTimer
    \/ UnloadTimerStep
    \/ UnloadPoolStep

Spec == Init /\ [][Next]_vars

(***************************************************************************)
(* Invariants                                                              *)
(***************************************************************************)

TypeOK ==
    /\ rowState \in {"active", "unloading", "disposed"}
    /\ unloadStage \in {"none", "timer", "pool", "done"}
    /\ pool \in {"open", "closed"}
    /\ timerState \in {"scheduled", "invoking_sync", "in_flight_async", "idle", "done"}
    /\ inFlight \in 0..2
    /\ onStack \in BOOLEAN
    /\ cancelled \in BOOLEAN
    /\ cancelCalls \in 0..2
    /\ ticks \in 0..MaxTicks
    /\ cleanupRuns \in 0..2
    /\ cleanupEntered \in BOOLEAN
    /\ usedPoolAfterClose \in BOOLEAN
    /\ firedAfterUnload \in BOOLEAN

\* An in-flight callback never executes or accesses `pool` after `pool` closes
\* (including after a synchronous self-cancel before its first await).
NoUseAfterPoolClose ==
    /\ ~usedPoolAfterClose
    /\ (pool = "closed" => inFlight = 0)

\* Once unload begins or finishes, no new timer callback ever starts.
NoFireAfterUnload ==
    ~firedAfterUnload

\* At most one callback execution is active at any instant (non-overlapping).
NoOverlappingTicks ==
    inFlight <= 1

\* Completed or cancelled timers detach cleanly from the unload stack;
\* after dispose nothing remains on stack or active; cleanup runs at most once.
CleanDetach ==
    /\ (timerState = "done" => ~onStack)
    /\ (rowState = "disposed" => ~onStack /\ timerState = "done")
    /\ cleanupRuns <= 1

=============================================================================
