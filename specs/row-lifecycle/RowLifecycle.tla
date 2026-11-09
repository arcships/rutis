--------------------------- MODULE RowLifecycle ----------------------------
(***************************************************************************)
(* The rows of one runtime session across a break and a reconnect of the   *)
(* same id: `rows.load` / `rows.unload` from the host, `rows.ended` /      *)
(* `rows.failed` from the runtime, and the runtime's order when a new      *)
(* connection replaces the old one (stop reading the old session, clean    *)
(* its rows, then answer the new hello).                                   *)
(*                                                                         *)
(* Design: docs/design-remote-session-2026-10-28.md §4.2 (a replacement    *)
(* unloads the old session first) and §4.3 (rows on a session's far end);  *)
(* docs/design-remote-plugins-2026-10-03.md §4.4 (close the old session,   *)
(* stop reading its frames, clean its lease, answer the new hello);        *)
(* docs/formal-trace.md §3.0 (the session and row lifecycle).              *)
(* Implementation: crates/rutis-loader/src/runtime.rs (the row's            *)
(* generation key), crates/rutis-bridge/src/runtime/process.rs             *)
(* (`rows.ended`, `rows.failed`, `unload_row`),                            *)
(* crates/rutis-bridge/src/session/plugin/rows.rs (`Server::unload` is     *)
(* idempotent; `follow` reports one ending), and the runtime's row table   *)
(* (node/rutis-runtime/src/runner.mjs: `unloadRow`, `endRow`).             *)
(* README.md next to this file lists the simplifications.                  *)
(*                                                                         *)
(* Connection s is the host's s-th dial. The host loads its rows through   *)
(* the session it has; the runtime serves one session at a time and, when  *)
(* a new hello comes, stops reading the old session and only answers the   *)
(* new one once the old session's rows are gone. Each direction of a       *)
(* connection is FIFO and delayed arbitrarily long; a side that closed a   *)
(* connection reads nothing more on it.                                    *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS
    Rows,        \* the host's rows on the runtime ("a", "b")
    MaxConns,    \* connections the host may make (1 + reconnects)
    MaxLoads,    \* loads of one row in one session (1 + restarts)
    Faults,      \* what the environment may do: "lose"
    Failures,    \* a loaded row may fail (rows.failed)
    SelfEnds,    \* a loaded row may end on its own (rows.ended)
    Mutation     \* "none", or a deliberate bug (README, "Mutations")

ASSUME Rows /= {}
ASSUME MaxConns \in Nat \ {0}
ASSUME MaxLoads \in Nat \ {0}
ASSUME Faults \subseteq {"lose"}
ASSUME Failures \in BOOLEAN
ASSUME SelfEnds \in BOOLEAN
ASSUME Mutation \in {"none", "acceptBeforeClean", "readOld", "keyedByRow",
                     "reclean", "keepRows"}

Sessions == 1..MaxConns
Keys == Sessions \X Rows \X (1..MaxLoads)
NoKey == <<0, "none", 0>>
SessOf(k) == k[1]
RowOf(k) == k[2]

VARIABLES
    hst,          \* the host: "connecting" | "greeting" | "ready" | "waiting"
    hconn,        \* the connection the host is on, 0 when none
    ndial,        \* connections the host made
    hrow,         \* per row: "idle" | "loading" | "loaded" | "unloading" | "gone"
    hkey,         \* per row: the key of its current (or last) load
    hpending,     \* per row: the key its unload waits for
    hgen,         \* per row: loads made in the session the host is on
    hEndedFrom,   \* ghost: the key whose ending ended the row
    hFailedFrom,  \* ghost: the key whose failure restarted the row
    cur,          \* the runtime: the session it serves, 0 when none
    pick,         \* the session whose hello it took up, awaiting the answer
    rside,        \* per session at the runtime: "none" | "open" | "closed"
    answered,     \* per session: the runtime answered its hello
    ended,        \* per session: the runtime saw the session end
    loaded,       \* the runtime's rows, by key
    pendingEnding,\* rows whose `rows.ended` the runtime has not sent yet
    reportedFail,\* rows whose `rows.failed` the runtime already sent
    cleanCount,   \* ghost: how often a row's cleanup ran
    wasSwept,     \* ghost: the session was swept at least once
    lostByHost,   \* per session: the host lost it
    toR,          \* per session: frames to the runtime, FIFO
    toH           \* per session: frames to the host, FIFO

vars == <<hst, hconn, ndial, hrow, hkey, hpending, hgen, hEndedFrom,
          hFailedFrom, cur, pick, rside, answered, ended, loaded,
          pendingEnding, reportedFail, cleanCount, wasSwept, lostByHost,
          toR, toH>>

RowsOf(s) == {k \in loaded : SessOf(k) = s}

Init ==
    /\ hst = "connecting"
    /\ hconn = 0
    /\ ndial = 0
    /\ hrow = [r \in Rows |-> "idle"]
    /\ hkey = [r \in Rows |-> NoKey]
    /\ hpending = [r \in Rows |-> NoKey]
    /\ hgen = [r \in Rows |-> 0]
    /\ hEndedFrom = [r \in Rows |-> NoKey]
    /\ hFailedFrom = [r \in Rows |-> NoKey]
    /\ cur = 0
    /\ pick = 0
    /\ rside = [s \in Sessions |-> "none"]
    /\ answered = [s \in Sessions |-> FALSE]
    /\ ended = [s \in Sessions |-> FALSE]
    /\ loaded = {}
    /\ pendingEnding = {}
    /\ reportedFail = {}
    /\ cleanCount = [k \in Keys |-> 0]
    /\ wasSwept = [s \in Sessions |-> FALSE]
    /\ lostByHost = [s \in Sessions |-> FALSE]
    /\ toR = [s \in Sessions |-> <<>>]
    /\ toH = [s \in Sessions |-> <<>>]

(***************************************************************************)
(* The host: its loader's rows and its dial row.                           *)
(***************************************************************************)

HDial ==
    /\ hst \in {"connecting", "waiting"}
    /\ ndial < MaxConns
    /\ LET n == ndial + 1 IN
       /\ ndial' = n
       /\ hconn' = n
       /\ hst' = "greeting"
       /\ hgen' = [r \in Rows |-> 0]
       /\ rside' = [rside EXCEPT ![n] = "open"]
       /\ toR' = [toR EXCEPT ![n] = Append(@, <<"hello", NoKey>>)]
       /\ UNCHANGED <<hrow, hkey, hpending, hEndedFrom, hFailedFrom, cur,
                      pick, answered, ended, loaded, pendingEnding,
                      reportedFail, cleanCount, wasSwept, lostByHost, toH>>

\* The runtime's hello: the session is up and may run rows.
HGreeted ==
    /\ hst = "greeting"
    /\ hconn > 0
    /\ toH[hconn] /= <<>>
    /\ LET f == Head(toH[hconn]) IN
       /\ f[1] = "hello"
       /\ hst' = "ready"
       /\ toH' = [toH EXCEPT ![hconn] = Tail(@)]
       /\ UNCHANGED <<hconn, ndial, hrow, hkey, hpending, hgen, hEndedFrom,
                      hFailedFrom, cur, pick, rside, answered, ended, loaded,
                      pendingEnding, reportedFail, cleanCount, wasSwept, lostByHost, toR>>

\* The loader loads row r: a fresh key (the generation) per load.
HLoad(r) ==
    /\ hst = "ready"
    /\ hconn > 0
    /\ hrow[r] = "idle"
    /\ hgen[r] < MaxLoads
    /\ LET k == <<hconn, r, hgen[r] + 1>> IN
       /\ hgen' = [hgen EXCEPT ![r] = @ + 1]
       /\ hkey' = [hkey EXCEPT ![r] = k]
       /\ hpending' = [hpending EXCEPT ![r] = NoKey]
       /\ hrow' = [hrow EXCEPT ![r] = "loading"]
       /\ hEndedFrom' = [hEndedFrom EXCEPT ![r] = NoKey]
       /\ hFailedFrom' = [hFailedFrom EXCEPT ![r] = NoKey]
       /\ toR' = [toR EXCEPT ![hconn] = Append(@, <<"load", k>>)]
       /\ UNCHANGED <<hst, hconn, ndial, cur, pick, rside, answered, ended,
                      loaded, pendingEnding, reportedFail, cleanCount, wasSwept, lostByHost,
                      toH>>

HLoaded ==
    /\ hst = "ready"
    /\ hconn > 0
    /\ toH[hconn] /= <<>>
    /\ LET f == Head(toH[hconn]) IN
       /\ f[1] = "loaded"
       /\ LET k == f[2]  r == RowOf(k) IN
          /\ toH' = [toH EXCEPT ![hconn] = Tail(@)]
          /\ hrow' = IF hrow[r] = "loading" /\ hkey[r] = k
                     THEN [hrow EXCEPT ![r] = "loaded"] ELSE hrow
          /\ UNCHANGED <<hst, hconn, ndial, hkey, hpending, hgen,
                         hEndedFrom, hFailedFrom, cur, pick, rside, answered,
                         ended, loaded, pendingEnding, reportedFail, cleanCount, wasSwept,
                         lostByHost, toR>>

\* The runtime unloaded the row: the restart may load a new generation.
HUnloaded ==
    /\ hst = "ready"
    /\ hconn > 0
    /\ toH[hconn] /= <<>>
    /\ LET f == Head(toH[hconn]) IN
       /\ f[1] = "unloaded"
       /\ LET k == f[2]  r == RowOf(k)
              waiting == hpending[r] = k /\ hrow[r] = "unloading"
          IN /\ toH' = [toH EXCEPT ![hconn] = Tail(@)]
             /\ hrow' = IF waiting THEN [hrow EXCEPT ![r] = "idle"] ELSE hrow
             /\ hpending' = IF waiting
                            THEN [hpending EXCEPT ![r] = NoKey] ELSE hpending
             /\ UNCHANGED <<hst, hconn, ndial, hkey, hgen, hEndedFrom,
                            hFailedFrom, cur, pick, rside, answered, ended,
                            loaded, pendingEnding, reportedFail, cleanCount, wasSwept,
                            lostByHost, toR>>

\* `rows.ended`: the row ended itself. Only the generation it belongs to
\* ends; a late ending of an earlier generation is ignored.
\* A row that ends is disabled: it is not loaded again.
HEnded ==
    /\ hst = "ready"
    /\ hconn > 0
    /\ toH[hconn] /= <<>>
    /\ LET f == Head(toH[hconn]) IN
       /\ f[1] = "ended"
       /\ LET k == f[2]
              r == RowOf(k)
              acts == hkey[r] = k /\ hrow[r] \in {"loading", "loaded"}
              bug == Mutation = "keyedByRow" /\ hkey[r] # k
                     /\ hrow[r] # "gone"
          IN /\ toH' = [toH EXCEPT ![hconn] = Tail(@)]
             /\ hrow' = IF acts \/ bug
                        THEN [hrow EXCEPT ![r] = "gone"] ELSE hrow
             /\ hEndedFrom' = IF acts \/ bug
                              THEN [hEndedFrom EXCEPT ![r] = k]
                              ELSE hEndedFrom
             \* The row's disposer unloads it as any disposal does; the
             \* runtime already disposed it.
             /\ toR' = IF acts /\ hrow[r] = "loaded"
                       THEN [toR EXCEPT ![hconn] = Append(@, <<"unload", k>>)]
                       ELSE toR
             /\ UNCHANGED <<hst, hconn, ndial, hkey, hpending, hgen,
                            hFailedFrom, cur, pick, rside, answered, ended,
                            loaded, pendingEnding, reportedFail, cleanCount, wasSwept,
                            lostByHost>>

\* `rows.failed`: the row's code failed. The host restarts it: unload the
\* generation, then load a new one. A failure of an earlier generation is
\* ignored.
HFailed ==
    /\ hst = "ready"
    /\ hconn > 0
    /\ toH[hconn] /= <<>>
    /\ LET f == Head(toH[hconn]) IN
       /\ f[1] = "failed"
       /\ LET k == f[2]
              r == RowOf(k)
              acts == hkey[r] = k /\ hrow[r] = "loaded"
              bug == Mutation = "keyedByRow" /\ hkey[r] # k
                     /\ hkey[r] # NoKey /\ hrow[r] = "loaded"
          IN /\ toH' = [toH EXCEPT ![hconn] = Tail(@)]
             /\ hrow' = IF acts \/ bug
                        THEN [hrow EXCEPT ![r] = "unloading"] ELSE hrow
             /\ hpending' = IF acts
                            THEN [hpending EXCEPT ![r] = k]
                            ELSE IF bug
                                 THEN [hpending EXCEPT ![r] = hkey[r]]
                                 ELSE hpending
             /\ toR' = IF acts
                       THEN [toR EXCEPT
                               ![hconn] = Append(@, <<"unload", k>>)]
                       ELSE IF bug
                            THEN [toR EXCEPT
                                    ![hconn] = Append(@,
                                      <<"unload", hkey[r]>>)]
                            ELSE toR
             /\ hFailedFrom' = IF acts \/ bug
                               THEN [hFailedFrom EXCEPT ![r] = k]
                               ELSE hFailedFrom
             /\ UNCHANGED <<hst, hconn, ndial, hkey, hgen, hEndedFrom, cur,
                            pick, rside, answered, ended, loaded,
                            pendingEnding, reportedFail, cleanCount, wasSwept, lostByHost>>

\* The connection breaks: the host's rows on it stop; it dials again.
HLose ==
    /\ "lose" \in Faults
    /\ hst \in {"greeting", "ready"}
    /\ hconn > 0
    /\ LET s == hconn IN
       /\ hst' = "waiting"
       /\ hconn' = 0
       /\ lostByHost' = [lostByHost EXCEPT ![s] = TRUE]
       /\ hrow' = [r \in Rows |->
                     IF SessOf(hkey[r]) = s /\ hrow[r] # "gone"
                     THEN "idle" ELSE hrow[r]]
       /\ hpending' = [r \in Rows |->
                         IF SessOf(hkey[r]) = s /\ hrow[r] # "gone"
                         THEN NoKey ELSE hpending[r]]
       /\ UNCHANGED <<ndial, hkey, hgen, hEndedFrom, hFailedFrom, cur, pick,
                      rside, answered, ended, loaded, pendingEnding,
                      reportedFail, cleanCount, wasSwept, toR, toH>>

(***************************************************************************)
(* The runtime: its row table and the listener that replaces the session.  *)
(***************************************************************************)

\* A side that closed a connection reads nothing more on it. A read-old
\* mutation keeps reading the old session after the replace.
CanRead(s) ==
    (cur = s /\ rside[s] = "open") \/ (Mutation = "readOld" /\ ended[s])

\* Swept: the session ended and nothing of it is loaded any more.
Swept(s) == ended[s] /\ RowsOf(s) = {}

\* The listener takes up a new connection's hello. It closes the old
\* session and starts cleaning it; the answer waits for that (Answer).
TakeUp(s) ==
    /\ pick = 0
    /\ rside[s] = "open"
    /\ toR[s] /= <<>>
    /\ Head(toR[s]) = <<"hello", NoKey>>
    /\ LET old == cur IN
       /\ toR' = [toR EXCEPT ![s] = Tail(@)]
       /\ pick' = s
       /\ rside' = IF old = 0 THEN rside
                  ELSE [rside EXCEPT ![old] = "closed"]
       /\ ended' = IF old = 0 THEN ended
                   ELSE [ended EXCEPT ![old] = TRUE]
       /\ cur' = 0
       /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen,
                      hEndedFrom, hFailedFrom, answered, loaded,
                      pendingEnding, reportedFail, cleanCount, wasSwept, lostByHost, toH>>

\* The answer to the new hello: only once every earlier session's rows are
\* cleaned, so no two sessions' rows are ever registered together.
Answer(s) ==
    /\ pick = s
    /\ (Mutation = "acceptBeforeClean"
        \/ \A p \in Sessions : p < s => Swept(p))
    /\ pick' = 0
    /\ cur' = s
    /\ answered' = [answered EXCEPT ![s] = TRUE]
    /\ toH' = [toH EXCEPT ![s] = Append(@, <<"hello", NoKey>>)]
    /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen,
                   hEndedFrom, hFailedFrom, rside, ended, loaded,
                   pendingEnding, reportedFail, cleanCount, wasSwept, lostByHost, toR>>

\* The runtime notices the host's close: the session is over.
EndSession(s) ==
    /\ lostByHost[s]
    /\ rside[s] = "open"
    /\ rside' = [rside EXCEPT ![s] = "closed"]
    /\ ended' = [ended EXCEPT ![s] = TRUE]
    /\ cur' = IF cur = s THEN 0 ELSE cur
    /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen,
                   hEndedFrom, hFailedFrom, pick, answered, loaded,
                   pendingEnding, reportedFail, cleanCount, wasSwept,
                   lostByHost, toR, toH>>

RLoad(k) ==
    /\ LET s == SessOf(k) IN
       /\ toR[s] /= <<>>
       /\ Head(toR[s]) = <<"load", k>>
       /\ CanRead(s)
       /\ toR' = [toR EXCEPT ![s] = Tail(@)]
       /\ loaded' = loaded \cup {k}
       /\ toH' = [toH EXCEPT ![s] = Append(@, <<"loaded", k>>)]
       /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen,
                      hEndedFrom, hFailedFrom, cur, pick, rside, answered,
                      ended, pendingEnding, reportedFail, cleanCount, wasSwept, lostByHost>>

\* `rows.unload`: idempotent. A row still loaded is disposed once; one
\* already gone is only answered.
RUnload(k) ==
    /\ LET s == SessOf(k) IN
       /\ toR[s] /= <<>>
       /\ Head(toR[s]) = <<"unload", k>>
       /\ CanRead(s)
       /\ toR' = [toR EXCEPT ![s] = Tail(@)]
       /\ toH' = [toH EXCEPT ![s] = Append(@, <<"unloaded", k>>)]
       /\ IF k \in loaded
          THEN /\ loaded' = loaded \ {k}
               /\ cleanCount' = [cleanCount EXCEPT ![k] = @ + 1]
          ELSE /\ loaded' = loaded
               /\ cleanCount' = [cleanCount EXCEPT ![k] =
                                   IF Mutation = "reclean" THEN @ + 1 ELSE @]
       /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen,
                      hEndedFrom, hFailedFrom, cur, pick, rside, answered,
                      ended, pendingEnding, reportedFail, wasSwept,
                      lostByHost>>

\* The plugin of a loaded row disposes itself: the row is gone, and its
\* ending is still to be sent. The runtime's disposal counts once.
EndSelf(k) ==
    /\ SelfEnds
    /\ k \in loaded
    /\ loaded' = loaded \ {k}
    /\ pendingEnding' = pendingEnding \cup {k}
    /\ cleanCount' = [cleanCount EXCEPT ![k] = @ + 1]
    /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen,
                   hEndedFrom, hFailedFrom, cur, pick, rside, answered,
                   ended, reportedFail, wasSwept, lostByHost, toR, toH>>

\* The ending goes out. Sent only while the session is up: after it ended
\* the runtime has nothing to send on it.
SendEnding(k) ==
    /\ k \in pendingEnding
    /\ LET s == SessOf(k) IN
       /\ rside[s] = "open"
       /\ answered[s]
       /\ pendingEnding' = pendingEnding \ {k}
       /\ toH' = [toH EXCEPT ![s] = Append(@, <<"ended", k>>)]
       /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen,
                      hEndedFrom, hFailedFrom, cur, pick, rside, answered,
                      ended, loaded, reportedFail, cleanCount, wasSwept,
                      lostByHost, toR>>

\* The row's code left an unhandled error: the row stays loaded (the host
\* decides what to do with it) and hears `rows.failed`.
RFail(k) ==
    /\ Failures
    /\ k \in loaded
    /\ k \notin reportedFail
    /\ LET s == SessOf(k) IN
       /\ rside[s] = "open"
       /\ answered[s]
       /\ reportedFail' = reportedFail \cup {k}
       /\ toH' = [toH EXCEPT ![s] = Append(@, <<"failed", k>>)]
       /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen,
                      hEndedFrom, hFailedFrom, cur, pick, rside, answered,
                      ended, loaded, pendingEnding, cleanCount, wasSwept,
                      lostByHost, toR>>

\* The session ended: its rows are disposed, once each.
Sweep(k) ==
    /\ Mutation /= "keepRows"
    /\ k \in loaded
    /\ ended[SessOf(k)]
    /\ loaded' = loaded \ {k}
    /\ cleanCount' = [cleanCount EXCEPT ![k] = @ + 1]
    /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen,
                   hEndedFrom, hFailedFrom, cur, pick, rside, answered,
                   ended, pendingEnding, reportedFail, wasSwept, lostByHost,
                   toR, toH>>

\* Ghost: the session was swept at least once (for NoRowsAfterSweep).
MarkSwept(s) ==
    /\ Swept(s)
    /\ ~wasSwept[s]
    /\ wasSwept' = [wasSwept EXCEPT ![s] = TRUE]
    /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen,
                   hEndedFrom, hFailedFrom, cur, pick, rside, answered,
                   ended, loaded, pendingEnding, reportedFail, cleanCount, lostByHost,
                   toR, toH>>

(***************************************************************************)
(* The specification.                                                      *)
(***************************************************************************)
Next ==
    \/ HDial
    \/ HGreeted
    \/ HLoaded
    \/ HUnloaded
    \/ HEnded
    \/ HFailed
    \/ HLose
    \/ \E r \in Rows : HLoad(r)
    \/ \E s \in Sessions :
         TakeUp(s) \/ Answer(s) \/ EndSession(s) \/ MarkSwept(s)
    \/ \E k \in Keys :
         RLoad(k) \/ RUnload(k) \/ EndSelf(k) \/ SendEnding(k) \/ RFail(k)
         \/ Sweep(k)

Spec == Init /\ [][Next]_vars /\ WF_vars(Next)

(***************************************************************************)
(* Properties.                                                             *)
(***************************************************************************)

\* One row of a name is loaded at the runtime at a time: the old session's
\* rows are cleaned before a new session's load, and a restart follows its
\* unload.
OneRowPerName ==
    \A r \in Rows : Cardinality({k \in loaded : RowOf(k) = r}) <= 1

\* A session is answered only after every earlier session's rows are gone.
NewSessionOnlyAfterSweep ==
    \A s \in Sessions : answered[s] => \A p \in Sessions : p < s => Swept(p)

\* Once a session was swept, no frame of it ever starts a row again: the
\* runtime stopped reading it.
NoRowsAfterSweep ==
    \A s \in Sessions : wasSwept[s] => \A k \in loaded : SessOf(k) # s

\* The host acts on an ending or a failure only for the generation that
\* row currently is.
OwnFramesOnly ==
    \A r \in Rows :
        /\ (hEndedFrom[r] = NoKey \/ hEndedFrom[r] = hkey[r])
        /\ (hFailedFrom[r] = NoKey \/ hFailedFrom[r] = hkey[r])

\* A row is cleaned once: the sweep, the unload and the row's own end do
\* not dispose the same row twice.
CleanupOnce == \A k \in Keys : cleanCount[k] <= 1

\* A session that ended ends with its rows cleaned.
EndedLeadsToSwept ==
    \A s \in Sessions : (ended[s] ~> Swept(s))
==============================================================================
