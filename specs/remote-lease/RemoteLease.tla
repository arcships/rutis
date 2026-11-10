----------------------------- MODULE RemoteLease -----------------------------
(***************************************************************************)
(* A remote runtime that listens, and the controllers that dial it: the    *)
(* lease of the runtime, takeover by a newer connection, reconnecting and  *)
(* a link stopping; a listening runtime that stops and starts again (an    *)
(* app hosting its plugins, #615), including a stop during a takeover and  *)
(* the final stop after which it never binds again.                        *)
(*                                                                         *)
(* Design: docs/design-remote-plugins-2026-10-03.md §3.2 (retry, link       *)
(* stop), §4.4 (lease, takeover order: authenticate → close the old        *)
(* session → clean the old lease → answer the new hello); docs/design-     *)
(* remote-runtime-dial-2026-10-14.md §六 (retry); docs/design-remote-      *)
(* session-2026-10-28.md §4.2/§4.3 (session end, app hosting).             *)
(* Implementation modelled: crates/rutis-host/src/connect/dial.rs (the     *)
(* `rutis-host/dial` row: attempt, handshake, install, unloaded, back off; *)
(* the controller); python/rutis/rutis/__main__.py `serve`,               *)
(* go/rutis/internal/runtime/listen.go `listen`, node/rutis-runtime/src/   *)
(* serve.mjs `serve`, swift/rutis/Sources/Rutis/Remote/Listen.swift and    *)
(* kotlin/.../internal/remote/Listen.kt `Listening` (the runtime,          *)
(* including app hosting), and kotlin/.../RutisApp.kt `start` / `stop`.    *)
(* README.md next to this file lists the simplifications.                  *)
(*                                                                         *)
(* Connection <<c, n>> is the n-th connection controller c dialed. Each    *)
(* direction of a connection is FIFO; frames are delayed arbitrarily long. *)
(* A side that closed a connection reads nothing more on it: frames to it  *)
(* are dropped (kept only where a mutation reads them).                    *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS
    Controllers,  \* controller links dialing the runtime (model values)
    MaxConns,     \* successful connections a controller pays for (1 + reconnects)
    MaxLoads,     \* rows a controller loads on one session
    MaxFaults,    \* transient faults in the whole run: failed dials, lost
                  \* connections (0: none happen)
    LinkStops,    \* TRUE: a controller link may be disposed
    Cleanup,      \* how a takeover cleans the old lease:
                  \* "instant" (one step) or "deadline" (start, then finish
                  \* or be killed when the deadline passes)
    Auth,         \* where a connection is authenticated: "upgrade" folds
                  \* it into the dial; "step" makes it a step the runtime
                  \* takes before it may close the old session
    MaxRounds,    \* rounds of the listening runtime (1: no stop/start)
    MaxStops,     \* stops of the runtime in the run (at most MaxRounds-1)
    Mutation      \* "none", or a deliberate bug (README, "Mutations")

\* A controller pays for MaxConns connections; a redial replacing a
\* connection a fault lost is free (§615: the quota counts the connections
\* it established, not the retry), so a controller never dials more than
\* MaxConns + MaxFaults connections.
MaxDials == MaxConns + MaxFaults

ASSUME MaxConns \in Nat \ {0} /\ MaxLoads \in Nat /\ MaxFaults \in Nat
ASSUME LinkStops \in BOOLEAN
ASSUME Cleanup \in {"instant", "deadline"}
ASSUME Auth \in {"upgrade", "step"}
ASSUME MaxRounds \in Nat \ {0}
ASSUME MaxStops \in Nat
ASSUME Mutation \in {"none", "startBeforeClean", "readOld", "stopKeepsRetry",
                     "lateHello", "waitForOld", "killLeavesRows",
                     "staleRound", "startAfterFinalStop",
                     "heartbeatFromAccept", "lateAccept"}

VARIABLES
    lst,      \* lst[c]: "connecting" | "greeting" | "ready" | "waiting" | "stopped"
    cur,      \* cur[c]: number of c's current connection, 0 before the first
    ndial,    \* ndial[c]: connections c dialed (numbers)
    paid,     \* paid[c]: connections c paid for (at most MaxConns)
    free,     \* free[c]: the next dial replaces one a fault lost: it is free
    sent,     \* sent[c]: rows c loaded on its current session
    stopped,  \* stopped[c]: c's link was disposed
    faults,   \* transient faults so far (at most MaxFaults)
    cs,       \* cs[k]: controller side of k: "none" | "open" | "closed"
    toR,      \* toR[k]: frames sent on k to the runtime, not read yet
    toC,      \* toC[k]: frames sent on k to the controller, not read yet
    authq,    \* connections the listener accepted, not authenticated yet
    rs,       \* rs[k]: runtime side: "none" | "queued" | "serving" | "closed" | "done"
    accq,     \* authenticated connections the listener accepted, not taken yet
    rcur,     \* connection whose session the runtime runs (or replaces)
    rnew,     \* the connection a takeover is for
    rphase,   \* "idle" | "closing" | "cleaning" | "starting" | "stopping"
              \* (and mutations')
    rround,   \* the listening runtime's current round
    rstopped, \* the runtime stopped (it may start again, MaxRounds)
    pending,  \* a start was asked for while the runtime was stopping
    kround,   \* kround[k]: the runtime round that accepted k
    rows,     \* rows loaded on the runtime: [lease, by]
    ghost     \* observations for the invariants

vars == <<lst, cur, ndial, paid, free, sent, stopped, faults, cs, toR, toC,
          authq, rs, accq, rcur, rnew, rphase, rround, rstopped, pending,
          kround, rows, ghost>>
ctl == <<lst, cur, ndial, paid, free, sent, stopped>>
rt == <<cs, toR, toC, authq, rs, accq, rcur, rnew, rphase, rround, rstopped,
        pending, kround, rows>>

Conns == Controllers \X (1..MaxDials)
NoConn == <<"none", 0>>
Cur(c) == <<c, cur[c]>>

\* The connections waiting in a queue.
QueueSet(q) == {q[i] : i \in 1..Len(q)}

\* Whether the listening runtime accepts dials now.
Closed == rstopped \/ rphase = "stopping"

\* Whether each side still reads k.
ReadsC(k) == cs[k] = "open" \/ Mutation = "lateHello"
ReadsR(k) == rs[k] \in {"none", "queued", "serving"} \/ Mutation = "readOld"

(***************************************************************************)
(* Controller: rutis-host/dial (connect/dial.rs).                          *)
(***************************************************************************)
\* Close c's side of k: the session's close reaches the runtime; nothing
\* more is read on k.
CloseC(k) ==
    /\ cs' = [cs EXCEPT ![k] = "closed"]
    /\ toR' = [toR EXCEPT ![k] = IF ReadsR(k) THEN Append(@, "close") ELSE @]
    /\ toC' = [toC EXCEPT ![k] = IF Mutation = "lateHello" THEN @ ELSE <<>>]

\* Top of the dial loop.
Dial(c) ==
    /\ lst[c] = "connecting"
    /\ ndial[c] < MaxDials
    /\ free[c] \/ paid[c] < MaxConns
    /\ ~Closed
    /\ LET n == ndial[c] + 1
           k == <<c, n>>
       IN /\ ndial' = [ndial EXCEPT ![c] = n]
          /\ paid' = [paid EXCEPT ![c] = IF free[c] THEN @ ELSE @ + 1]
          /\ free' = [free EXCEPT ![c] = FALSE]
          /\ cur' = [cur EXCEPT ![c] = n]
          /\ sent' = [sent EXCEPT ![c] = 0]
          \* The upgrade succeeded: the runtime's listener queues the
          \* connection; the link opens its session (`Greet::open`; its own
          \* hello is read by whichever session the runtime starts on k, and
          \* is left out). With Auth = "step" the runtime checks the
          \* connection as a step before the queue the takeover reads; with
          \* "upgrade" that check is part of the upgrade.
          /\ cs' = [cs EXCEPT ![k] = "open"]
          /\ rs' = [rs EXCEPT ![k] = "queued"]
          /\ kround' = [kround EXCEPT ![k] = rround]
          /\ IF Auth = "step"
             THEN /\ authq' = Append(authq, k)
                  /\ UNCHANGED accq
             ELSE /\ accq' = Append(accq, k)
                  /\ UNCHANGED authq
          /\ lst' = [lst EXCEPT ![c] = "greeting"]
          /\ ghost' = [ghost EXCEPT !.dialAfterStop = @ \/ stopped[c]]
    /\ UNCHANGED <<stopped, faults, toR, toC, rcur, rnew, rphase, rround,
                   rstopped, pending, rows>>

\* The dial itself fails (connection refused, network down, the runtime
\* stopped): retryable, and it does not use up a connection. A connection
\* that was already under way when the runtime stopped is `LateAccept`
\* below (Swift, #615).
DialFails(c) ==
    /\ faults < MaxFaults
    /\ lst[c] = "connecting"
    /\ free[c] \/ paid[c] < MaxConns
    /\ faults' = faults + 1
    /\ lst' = [lst EXCEPT ![c] = "waiting"]
    /\ ghost' = [ghost EXCEPT !.dialAfterStop = @ \/ stopped[c]]
    /\ UNCHANGED <<cur, ndial, paid, free, sent, stopped, rt>>

\* The wait (backoff, or 30 s after an authentication failure) is over.
Retry(c) ==
    /\ lst[c] = "waiting"
    /\ lst' = [lst EXCEPT ![c] = "connecting"]
    /\ UNCHANGED <<cur, ndial, paid, free, sent, stopped, faults, cs, toR, toC,
                   authq, rs, accq, rcur, rnew, rphase, rround, rstopped,
                   pending, kround, rows, ghost>>

\* Read the next frame of connection n. A closed session reads nothing: a
\* handshake result arriving after the link gave up on it goes with the
\* session (the `lateHello` mutation reads it anyway).
CRecv(c, n) ==
    LET k == <<c, n>>
        f == Head(toC[k])
    IN
    /\ toC[k] /= <<>>
    /\ ReadsC(k)
    /\ CASE f = "hello" /\ cs[k] = "open" /\ lst[c] = "greeting" /\ cur[c] = n ->
              \* `start`: the far end greeted; the peer is provided.
              /\ toC' = [toC EXCEPT ![k] = Tail(@)]
              /\ lst' = [lst EXCEPT ![c] = "ready"]
              /\ UNCHANGED <<cs, toR, cur, ghost>>
         [] f = "hello" /\ cs[k] = "closed" /\ lst[c] \in {"connecting", "waiting"} ->
              \* lateHello: a late greeting revives a session given up on.
              /\ toC' = [toC EXCEPT ![k] = Tail(@)]
              /\ lst' = [lst EXCEPT ![c] = "ready"]
              /\ cur' = [cur EXCEPT ![c] = n]
              /\ UNCHANGED <<cs, toR, ghost>>
         [] f = "close" /\ cs[k] = "open" ->
              \* The session ended (replaced: 4002, the runtime stopped, or
              \* the runtime ended it): retryable; the peer is withdrawn.
              /\ CloseC(k)
              /\ lst' = [lst EXCEPT ![c] = IF cur[c] = n /\ @ \in {"greeting", "ready"}
                                           THEN "waiting" ELSE @]
              /\ UNCHANGED <<cur, ghost>>
         [] OTHER ->
              /\ toC' = [toC EXCEPT ![k] = Tail(@)]
              /\ UNCHANGED <<lst, cur, cs, toR, ghost>>
    /\ UNCHANGED <<ndial, paid, free, sent, stopped, faults, authq, rs, accq,
                   rcur, rnew, rphase, rround, rstopped, pending, kround, rows>>

\* The loader, through the peer, loads a row on the session.
SendLoad(c) ==
    LET k == Cur(c) IN
    /\ lst[c] = "ready"
    /\ cs[k] = "open"
    /\ sent[c] < MaxLoads
    /\ sent' = [sent EXCEPT ![c] = @ + 1]
    /\ toR' = [toR EXCEPT ![k] = IF ReadsR(k) THEN Append(@, "load") ELSE @]
    /\ UNCHANGED <<lst, cur, ndial, paid, free, stopped, faults, cs, toC,
                   authq, rs, accq, rcur, rnew, rphase, rround, rstopped,
                   pending, kround, rows, ghost>>

\* The handshake times out, or the connection is lost: retryable. Its
\* redial is free: it replaces the connection a fault took (§615).
Lose(c) ==
    /\ faults < MaxFaults
    /\ lst[c] \in {"greeting", "ready"}
    /\ cs[Cur(c)] = "open"
    /\ CloseC(Cur(c))
    /\ lst' = [lst EXCEPT ![c] = "waiting"]
    /\ free' = [free EXCEPT ![c] = TRUE]
    /\ faults' = faults + 1
    /\ UNCHANGED <<cur, ndial, paid, sent, stopped, authq, rs, accq, rcur,
                   rnew, rphase, rround, rstopped, pending, kround, rows, ghost>>

\* The link is disposed: its task is aborted; dropping it closes the
\* session and cancels the retry. A link may be disposed more than once
\* (`RutisApp.stop`); the first one takes it down.
Stop(c) ==
    /\ LinkStops
    /\ ~stopped[c]
    /\ stopped' = [stopped EXCEPT ![c] = TRUE]
    /\ IF cur[c] > 0 /\ cs[Cur(c)] = "open"
       THEN CloseC(Cur(c)) ELSE UNCHANGED <<cs, toR, toC>>
    /\ lst' = [lst EXCEPT ![c] =
                 IF Mutation = "stopKeepsRetry" /\ @ = "waiting" THEN @ ELSE "stopped"]
    /\ UNCHANGED <<cur, ndial, paid, free, sent, faults, authq, rs, accq,
                   rcur, rnew, rphase, rround, rstopped, pending, kround, rows, ghost>>

\* The connection was accepted before the runtime stopped, and is queued
\* after it (the Swift `.accepted` arriving after `stop`, #615). The drain
\* in `Listening::run` closes it; the mutation queues it and the new round
\* serves it.
LateAccept(c) ==
    /\ Mutation = "lateAccept"
    /\ Closed
    /\ lst[c] = "connecting"
    /\ ndial[c] < MaxDials
    /\ free[c] \/ paid[c] < MaxConns
    /\ LET n == ndial[c] + 1
           k == <<c, n>>
       IN /\ ndial' = [ndial EXCEPT ![c] = n]
          /\ paid' = [paid EXCEPT ![c] = IF free[c] THEN @ ELSE @ + 1]
          /\ free' = [free EXCEPT ![c] = FALSE]
          /\ cur' = [cur EXCEPT ![c] = n]
          /\ sent' = [sent EXCEPT ![c] = 0]
          /\ cs' = [cs EXCEPT ![k] = "open"]
          /\ rs' = [rs EXCEPT ![k] = "queued"]
          /\ kround' = [kround EXCEPT ![k] = rround + 1]
          /\ accq' = Append(accq, k)
          /\ lst' = [lst EXCEPT ![c] = "greeting"]
          /\ UNCHANGED authq
    /\ UNCHANGED <<stopped, faults, toR, toC, rcur, rnew, rphase, rround,
                   rstopped, pending, rows, ghost>>

(***************************************************************************)
(* Runtime: one controller at a time; a newer connection takes over        *)
(* (§4.4): authenticate → close the old session's channel and read none of *)
(* its frames → clean the old lease (finishing, or killed at the deadline) *)
(* → answer the new hello. A new connection's heartbeat starts only once   *)
(* its hello is answered: the silence while an old lease is cleaned is not *)
(* its silence (Kotlin, #615).                                             *)
(***************************************************************************)
SendC(k, f) == toC' = [toC EXCEPT ![k] = IF ReadsC(k) THEN Append(@, f) ELSE @]

\* Stop reading k: what is still on its way is dropped.
Unread(k) == toR' = [toR EXCEPT ![k] = IF Mutation = "readOld" THEN @ ELSE <<>>]

StartSession(k) ==
    /\ rs' = [rs EXCEPT ![k] = "serving"]
    /\ SendC(k, "hello")
    /\ ghost' = [ghost EXCEPT !.greeted = @ \cup {k}]

\* The new connection's heartbeat expires while it waits for the old lease
\* to be cleaned: it is closed with 1001. Only with the heartbeat started
\* at the accept, not at the answer to its hello (heartbeatFromAccept).
HeartbeatOut(k) ==
    /\ Mutation = "heartbeatFromAccept"
    /\ \/ (authq /= <<>> /\ k = Head(authq) /\ authq' = Tail(authq) /\ UNCHANGED accq)
       \/ (accq /= <<>> /\ k = Head(accq) /\ accq' = Tail(accq) /\ UNCHANGED authq)
    /\ rs[k] = "queued"
    /\ rs' = [rs EXCEPT ![k] = "done"]
    /\ SendC(k, "close")
    /\ Unread(k)
    /\ ghost' = [ghost EXCEPT !.heartbeat = @ \cup {k}]
    /\ UNCHANGED <<ctl, faults, cs, rcur, rnew, rphase, rround, rstopped,
                   pending, kround, rows>>

\* 1. Authenticate: a connection the runtime does not accept is closed
\* before it can touch the session it serves.
AuthPass(k) ==
    /\ Auth = "step"
    /\ authq /= <<>>
    /\ k = Head(authq)
    /\ authq' = Tail(authq)
    /\ accq' = Append(accq, k)
    /\ UNCHANGED <<ctl, faults, cs, toR, toC, rs, rcur, rnew, rphase, rround,
                   rstopped, pending, kround, rows, ghost>>

AuthRefuse(k) ==
    /\ Auth = "step"
    /\ authq /= <<>>
    /\ k = Head(authq)
    /\ authq' = Tail(authq)
    /\ rs' = [rs EXCEPT ![k] = "done"]
    /\ SendC(k, "close")
    /\ Unread(k)
    /\ UNCHANGED <<ctl, faults, cs, accq, rcur, rnew, rphase, rround, rstopped,
                   pending, kround, rows, ghost>>

TakeNext ==
    /\ rphase = "idle"
    /\ ~rstopped
    /\ accq /= <<>>
    /\ accq' = Tail(accq)
    /\ LET k == Head(accq) IN
       IF rcur = NoConn
       THEN /\ StartSession(k)
            /\ rcur' = k
            /\ UNCHANGED <<rnew, rphase>>
       ELSE /\ rnew' = k
            /\ rphase' = "closing"
            /\ UNCHANGED <<rs, toC, rcur, ghost>>
    /\ UNCHANGED <<ctl, faults, cs, toR, authq, rround, rstopped, pending,
                   kround, rows>>

\* 2. Close the old channel (4002, "replaced"); no frame of it is read any
\* more. waitForOld: stop reading, but leave closing to the old controller.
TakeoverClose ==
    /\ rphase = "closing"
    /\ rs' = [rs EXCEPT ![rcur] = "closed"]
    /\ IF Mutation = "waitForOld" THEN UNCHANGED toC ELSE SendC(rcur, "close")
    /\ Unread(rcur)
    /\ rphase' = IF Mutation = "startBeforeClean" THEN "startEarly" ELSE "cleaning"
    /\ UNCHANGED <<ctl, faults, cs, authq, accq, rcur, rnew, rround, rstopped,
                   pending, kround, rows, ghost>>

\* 3a. The old lease is cleaned in one step (Cleanup = "instant").
TakeoverClean ==
    /\ rphase = "cleaning"
    /\ Cleanup = "instant"
    /\ Mutation = "waitForOld" => cs[rcur] = "closed"
    /\ rows' = {r \in rows : r.lease /= rcur}
    /\ rs' = [rs EXCEPT ![rcur] = "done"]
    /\ rphase' = "starting"
    /\ UNCHANGED <<ctl, faults, cs, toR, toC, authq, accq, rcur, rnew, rround,
                   rstopped, pending, kround, ghost>>

\* 3b. Cleanup finishes within its deadline.
CleanDone ==
    /\ rphase = "cleaning"
    /\ Cleanup = "deadline"
    /\ rows' = {r \in rows : r.lease /= rcur}
    /\ rs' = [rs EXCEPT ![rcur] = "done"]
    /\ rphase' = "starting"
    /\ UNCHANGED <<ctl, faults, cs, toR, toC, authq, accq, rcur, rnew, rround,
                   rstopped, pending, kround, ghost>>

\* 3c. The deadline passes: the runtime kills the old session, truncating
\* its cleanups. The lease goes with the killed session; a late "cleaned"
\* never reaches the new one. killLeavesRows: the kill leaves the old rows
\* behind, so the new lease is not alone.
CleanTimeout ==
    /\ rphase = "cleaning"
    /\ Cleanup = "deadline"
    /\ ~ghost.killed
    /\ ghost' = [ghost EXCEPT !.killed = TRUE]
    /\ IF Mutation = "killLeavesRows"
       THEN UNCHANGED rows
       ELSE rows' = {r \in rows : r.lease /= rcur}
    /\ rs' = [rs EXCEPT ![rcur] = "done"]
    /\ rphase' = "starting"
    /\ UNCHANGED <<ctl, faults, cs, toR, toC, authq, accq, rcur, rnew, rround,
                   rstopped, pending, kround>>

\* 4. Only then the new session starts and answers its hello.
TakeoverStart ==
    /\ rphase = "starting"
    /\ StartSession(rnew)
    /\ rcur' = rnew
    /\ rnew' = NoConn
    /\ rphase' = "idle"
    /\ UNCHANGED <<ctl, faults, cs, toR, authq, accq, rround, rstopped, pending,
                   kround, rows>>

\* startBeforeClean: steps 3 and 4 swapped.
StartEarly ==
    /\ rphase = "startEarly"
    /\ StartSession(rnew)
    /\ rphase' = "cleanLate"
    /\ UNCHANGED <<ctl, faults, cs, toR, authq, accq, rcur, rnew, rround,
                   rstopped, pending, kround, rows>>

CleanLate ==
    /\ rphase = "cleanLate"
    /\ rows' = {r \in rows : r.lease /= rcur}
    /\ rs' = [rs EXCEPT ![rcur] = "done"]
    /\ rcur' = rnew
    /\ rnew' = NoConn
    /\ rphase' = "idle"
    /\ UNCHANGED <<ctl, faults, cs, toR, toC, authq, accq, rround, rstopped,
                   pending, kround, ghost>>

(***************************************************************************)
(* The listening runtime stops and starts again (an app hosting its        *)
(* plugins, RutisApp.start/stop): stop closes the current session and the  *)
(* connections accepted but not served, cleans the lease, accepts no more; *)
(* start opens a new round. A start asked for while the stop is cleaning   *)
(* up runs once that is done; a start while running is no step. A stop may *)
(* come at any moment, a takeover under way included; at most one stop per *)
(* round, and the stop of the last round is final.                         *)
(***************************************************************************)
RuntimeStop ==
    /\ MaxRounds > 1
    /\ Cardinality(ghost.stoppedRounds) < MaxStops
    /\ rphase /= "stopping"
    /\ rround \notin ghost.stoppedRounds
    /\ rphase' = "stopping"
    /\ IF Mutation = "staleRound"
       THEN /\ UNCHANGED <<authq, accq, rnew>>
            /\ toC' = [j \in Conns |-> IF j = rcur /\ ReadsC(j)
                                    THEN Append(toC[j], "close") ELSE toC[j]]
            /\ rs' = [k \in Conns |-> IF k = rcur THEN "closed" ELSE rs[k]]
       ELSE /\ authq' = <<>>
            /\ accq' = <<>>
            /\ rnew' = NoConn
            /\ toC' = [j \in Conns |-> IF (j \in QueueSet(authq) \/ j \in QueueSet(accq)
                                       \/ j = rcur \/ j = rnew) /\ ReadsC(j)
                                    THEN Append(toC[j], "close") ELSE toC[j]]
            /\ rs' = [k \in Conns |-> IF k = rcur \/ k = rnew
                                           \/ k \in QueueSet(authq) \/ k \in QueueSet(accq)
                                      THEN "closed" ELSE rs[k]]
    /\ toR' = [j \in Conns |-> IF j = rcur
                               \/ (Mutation /= "staleRound"
                                   /\ (j = rnew \/ j \in QueueSet(authq) \/ j \in QueueSet(accq)))
                               THEN <<>> ELSE toR[j]]
    /\ ghost' = [ghost EXCEPT !.stoppedRounds = @ \cup {rround}]
    /\ UNCHANGED <<ctl, faults, cs, rcur, rround, rstopped, pending, kround,
                   rows>>

\* The stop's cleanup is done. A start asked for meanwhile opens the next
\* round at once; otherwise the runtime stays stopped until StartNow.
StopClean ==
    /\ rphase = "stopping"
    /\ rows' = {}
    /\ rcur' = IF Mutation = "staleRound" THEN rcur ELSE NoConn
    /\ rnew' = IF Mutation = "staleRound" THEN rnew ELSE NoConn
    /\ rphase' = "idle"
    /\ IF pending
       THEN /\ rround' = rround + 1
            /\ pending' = FALSE
            /\ UNCHANGED rstopped
       ELSE /\ rstopped' = TRUE
            /\ UNCHANGED <<rround, pending>>
    /\ ghost' = [ghost EXCEPT !.final = @ \/ (rstopped' /\ rround' = MaxRounds)]
    /\ UNCHANGED <<ctl, faults, cs, toR, toC, authq, accq, rs, kround>>

StartNow ==
    /\ rstopped
    /\ (rround < MaxRounds
        \/ (Mutation = "startAfterFinalStop" /\ rround = MaxRounds))
    /\ rround' = rround + 1
    /\ rstopped' = FALSE
    /\ UNCHANGED <<ctl, faults, cs, toR, toC, authq, accq, rs, rcur, rnew,
                   rphase, pending, kround, rows, ghost>>

StartWhileStopping ==
    /\ rphase = "stopping"
    /\ ~pending
    /\ rround < MaxRounds
    /\ pending' = TRUE
    /\ UNCHANGED <<ctl, faults, cs, toR, toC, authq, accq, rs, rcur, rnew,
                   rphase, rround, rstopped, kround, rows, ghost>>

\* The runtime reads the next frame of a session it serves. readOld: it
\* still reads a replaced session, and applies what it reads to the rows
\* it now serves (as one shared row table would).
RRead(k) ==
    LET f == Head(toR[k]) IN
    /\ toR[k] /= <<>>
    /\ \/ rs[k] = "serving"
       \/ Mutation = "readOld" /\ rs[k] \in {"closed", "done"}
    /\ CASE f = "load" ->
              LET target == IF rs[k] = "serving" THEN k ELSE rcur IN
              /\ target \in Conns /\ rs[target] = "serving"
              /\ toR' = [toR EXCEPT ![k] = Tail(@)]
              /\ rows' = rows \cup {[lease |-> target, by |-> k]}
              /\ UNCHANGED <<rs, rcur>>
         [] f = "close" ->
              \* The session ended: its lease is cleaned up (`end`). While a
              \* takeover runs, the loop is busy; it closes this one anyway.
              /\ rs[k] = "serving" /\ rphase = "idle" /\ rcur = k
              /\ toR' = [toR EXCEPT ![k] = <<>>]
              /\ rows' = {r \in rows : r.lease /= k}
              /\ rs' = [rs EXCEPT ![k] = "done"]
              /\ rcur' = NoConn
    /\ UNCHANGED <<ctl, faults, cs, toC, authq, accq, rnew, rphase, rround,
                   rstopped, pending, kround, ghost>>

(***************************************************************************)
Init ==
    /\ lst = [c \in Controllers |-> "connecting"]
    /\ cur = [c \in Controllers |-> 0]
    /\ ndial = [c \in Controllers |-> 0]
    /\ paid = [c \in Controllers |-> 0]
    /\ free = [c \in Controllers |-> FALSE]
    /\ sent = [c \in Controllers |-> 0]
    /\ stopped = [c \in Controllers |-> FALSE]
    /\ faults = 0
    /\ cs = [k \in Conns |-> "none"]
    /\ toR = [k \in Conns |-> <<>>]
    /\ toC = [k \in Conns |-> <<>>]
    /\ authq = <<>>
    /\ rs = [k \in Conns |-> "none"]
    /\ accq = <<>>
    /\ rcur = NoConn
    /\ rnew = NoConn
    /\ rphase = "idle"
    /\ rround = 1
    /\ rstopped = FALSE
    /\ pending = FALSE
    /\ kround = [k \in Conns |-> 1]
    /\ rows = {}
    /\ ghost = [dialAfterStop |-> FALSE, killed |-> FALSE, stoppedRounds |-> {},
                final |-> FALSE, greeted |-> {}, heartbeat |-> {}]

Controller(c) ==
    \/ Dial(c) \/ DialFails(c) \/ Retry(c) \/ SendLoad(c) \/ Lose(c)
    \/ Stop(c) \/ LateAccept(c)
    \/ \E n \in 1..MaxDials : CRecv(c, n)

Runtime ==
    \/ TakeNext \/ TakeoverClose \/ TakeoverClean \/ CleanDone \/ CleanTimeout
    \/ TakeoverStart \/ StartEarly \/ CleanLate
    \/ RuntimeStop \/ StopClean \/ StartNow \/ StartWhileStopping
    \/ \E k \in Conns : AuthPass(k) \/ AuthRefuse(k) \/ HeartbeatOut(k) \/ RRead(k)

Next == Runtime \/ \E c \in Controllers : Controller(c)

\* Liveness is checked on runs where the environment's faults are bounded
\* (MaxFaults) and its stops are off (LinkStops = FALSE): every behaviour
\* is finite there, so weak fairness of Next makes every enabled step
\* happen. `lease-faults-live` checks this after transient faults;
\* `lease-app-restart`, `lease-app-restart-3` after a stop and start.
Spec == Init /\ [][Next]_vars /\ WF_vars(Next)

\* Controllers are interchangeable (safety configurations only: TLC does
\* not combine symmetry with liveness checking).
Symmetry == Permutations(Controllers)

(***************************************************************************)
(* Properties.                                                             *)
(***************************************************************************)
\* The runtime holds a lease for k: k's session runs, or rows of k remain.
Leased(k) == rs[k] = "serving" \/ \E r \in rows : r.lease = k

\* At most one active lease per runtime instance.
OneLease == Cardinality({k \in Conns : Leased(k)}) <= 1

\* A command of one session never acts on another session's lease.
OldCommandsStayOld == \A r \in rows : r.by = r.lease

\* The runtime answers the new hello only after the old lease is gone.
CleanBeforeHello == rphase = "starting" => rows = {}

\* A stopped link never dials again and keeps no session.
StoppedStaysDown ==
    /\ ~ghost.dialAfterStop
    /\ \A c \in Controllers : stopped[c] =>
          \A n \in 1..MaxDials : cs[<<c, n>>] /= "open"

\* A controller is ready only on its current, open connection: a late
\* handshake result never revives a session the link gave up on.
ReadyIsCurrent ==
    \A c \in Controllers : lst[c] = "ready" => cur[c] > 0 /\ cs[Cur(c)] = "open"

\* A stopped runtime serves nothing and holds no lease; nothing it accepted
\* before the stop is served after it.
RuntimeStopped ==
    rstopped =>
        /\ rcur = NoConn /\ rnew = NoConn /\ rphase = "idle"
        /\ rows = {} /\ authq = <<>> /\ accq = <<>>

\* The runtime serves, and is about to serve, only connections of its
\* current round; a start while a stop cleans up opens the next round only
\* after the old one is gone. A controller may still be ready on a
\* connection the runtime closed, until it reads the close.
RestartIsFresh ==
    /\ (rcur /= NoConn => kround[rcur] = rround)
    /\ \A k \in QueueSet(accq) \cup QueueSet(authq) : kround[k] = rround
    /\ \A c \in Controllers : lst[c] = "ready" =>
          cur[c] > 0 /\
          (kround[Cur(c)] = rround \/ rs[Cur(c)] \in {"closed", "done"})

\* The stop of the last round is final: no start opens a round after it.
FinalStopStaysDown == ghost.final => rstopped

\* A new connection is not closed by its heartbeat before its hello is
\* answered: the heartbeat starts at the answer, not at the accept.
NoHeartbeatBeforeHello == \A k \in ghost.heartbeat : k \in ghost.greeted

\* Liveness: some controller is ready on the session the runtime serves.
Held == \E c \in Controllers : lst[c] = "ready" /\ rs[Cur(c)] = "serving"
EventuallyHeld == <>Held
EndsHeld == []<>Held

\* After a stop whose round has a successor, with a controller that still
\* has connections to spend, the runtime starts again and someone holds the
\* lease. (A controller that already spent its connections cannot dial
\* after the start; the property does not demand it.)
RestartRecovers ==
    []( (rstopped /\ rround < MaxRounds /\
        \E c \in Controllers :
            ~stopped[c] /\ (free[c] \/ paid[c] < MaxConns) /\ ndial[c] < MaxDials)
       => <>Held )
==============================================================================
