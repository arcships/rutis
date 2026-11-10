----------------------------- MODULE AccessPoint -----------------------------
(***************************************************************************)
(* Far ends dialing a `rutis-host/listen` row: handshakes side by side,    *)
(* one session per id, a newer connection of a connected id replacing the  *)
(* older, the dialers' reconnection rules, and the static listen link whose *)
(* id is revoked (the link keeps its registration, waits as auth-rejected, *)
(* and closes later connections of that id with 4003 after their hello,    *)
(* including one revoked while its handshake is under way — DIA-5-10, the  *)
(* listen side).                                                           *)
(*                                                                         *)
(* A dialing app (RutisApp with `connect`, Kotlin / Swift) may stop and    *)
(* start again: each round numbers its dials afresh, a stop closes the     *)
(* current connection and cancels the retry, and the stop of the last      *)
(* round is final.                                                         *)
(*                                                                         *)
(* Design: docs/design-remote-session-2026-10-28.md §4.2 (replacement:     *)
(* unload the old session, close its channel as replaced, then install the *)
(* new one); remote plugins §4.4; remote session §4.3 (the link's status). *)
(* Implementation modelled: crates/rutis-host/src/connect/listen.rs        *)
(* (`Listening::run`, `Replaceable`); the dialers:                         *)
(* python/rutis/rutis/dialing.py `keep_dialing`, go/rutis/internal/runtime/ *)
(* dial.go, node/rutis-runtime/src/dialing.mjs, bun/rutis-bun/src/dialing.ts, *)
(* swift/.../Remote/Dial.swift, kotlin/.../internal/remote/Dialing.kt, and  *)
(* the app's start / stop: kotlin/.../RutisApp.kt. README.md next to this  *)
(* file lists the simplifications.                                         *)
(*                                                                         *)
(* Connection <<d, r, n>> is dialer d's n-th connection in round r; d      *)
(* greets with the id IdOf(d). Frames to the dialer are FIFO and delayed   *)
(* arbitrarily long; a side that closed a connection reads nothing more on *)
(* it (the matchByNumber mutation keeps reading it).                       *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS
    Dialers,      \* far ends dialing (model values)
    SameId,       \* TRUE: every dialer greets with the same id
    MaxConns,     \* successful connections a dialer pays for in a round
    MaxFaults,    \* transient faults in the whole run: failed dials,
                  \* lost connections (0: none happen)
    Faults,       \* what the environment may do: "lose" (failed dial,
                  \* failed handshake, lost connection, a session ending),
                  \* "stop" (a dialer stops), "revoke" (a dialer's id is
                  \* revoked once)
    MaxRounds,    \* rounds of a dialing app (1: no stop/start)
    MaxStops,     \* stops of a dialer in the run (at most MaxRounds-1)
    Mutation      \* "none", or a deliberate bug (README, "Mutations")

\* A dialer pays for MaxConns connections per round; a redial replacing a
\* connection a fault lost is free, so it never makes more than
\* MaxConns + MaxFaults connections in a round.
MaxDials == MaxConns + MaxFaults

ASSUME SameId \in BOOLEAN
ASSUME MaxConns \in Nat \ {0} /\ MaxFaults \in Nat
ASSUME Faults \subseteq {"lose", "stop", "revoke"}
ASSUME MaxRounds \in Nat \ {0} /\ MaxStops \in Nat
ASSUME Mutation \in {"none", "acceptBeforeRelease", "stopKeepsRetry",
                     "revokeInstalls", "revokeUnregisters",
                     "revokeAfterHandshake", "matchByNumber",
                     "finalStopDials"}

\* The id a dialer greets with: its own name, or one shared by all.
IdOf(d) == IF SameId THEN "one" ELSE d
Ids == {IdOf(d) : d \in Dialers}
Rounds == 1..(MaxRounds + (IF Mutation = "finalStopDials" THEN 1 ELSE 0))
Conns == Dialers \X Rounds \X (1..MaxDials)
Id(k) == IdOf(k[1])
W(k) == k[1]
R(k) == k[2]
N(k) == k[3]
NoConn == <<"none", 0, 0>>

VARIABLES
    dst,        \* dst[d]: "connecting" | "greeting" | "ready" | "waiting" | "stopped"
    dround,     \* dround[d]: the dialer's current round
    dcur,       \* dcur[d]: number of d's current connection, 0 before the first
    ndial,      \* ndial[d]: connections d made in this round
    paid,       \* paid[d]: connections d paid for in this round
    free,       \* free[d]: the next dial replaces one a fault lost: it is free
    dstopped,   \* dstopped[d]: d stopped in this round
    faults,     \* transient faults so far (at most MaxFaults)
    ds,         \* ds[k]: dialer side of k: "none" | "open" | "closed"
    toD,        \* toD[k]: frames to the dialer: "hello" | "close" | "rejected"
    hs,         \* hs[k]: listen side of k (below)
    plugins,    \* plugins[k]: the far plugins of k's session are registered
    replaced,   \* replaced[k]: k's `Replaceable` closer is marked replaced
    live,       \* live[i]: listen's `live` map: the session of id i
    loop,       \* where `Listening::run` is: [st, new, old]
    endings,    \* sessions unloaded whose ending listen has not read yet
    revoked,    \* ids whose identity was revoked
    registered, \* the listen row's registration on its listener
    outcome,    \* outcome[d]: why d last waited ("none", "authRejected", "other")
    helloed,    \* helloed[k]: rutis' hello was read on k
    ghost       \* observations for the invariants

vars == <<dst, dround, dcur, ndial, paid, free, dstopped, faults, ds, toD, hs,
          plugins, replaced, live, loop, endings, revoked, registered, outcome,
          helloed, ghost>>
dialer == <<dst, dround, dcur, ndial, paid, free, dstopped>>

\* hs[k]: "none"; "handshaking" (in the JoinSet); "greeted" (handshake done,
\* waiting for the loop); "failed" (handshake failed, channel closed);
\* "installed"; "unloading"; "unloaded" (every far plugin unloaded, channel
\* closed).
\* loop.st: "idle" (in `select!`), or "replacing": the arm for loop.new is
\* awaiting the unload of loop.old; nothing else runs in the loop meanwhile.
Idle == [st |-> "idle", new |-> NoConn, old |-> NoConn]
CurConn(d) == <<d, dround[d], dcur[d]>>
ReadsD(k) == ds[k] = "open" \/ Mutation = "matchByNumber"

(***************************************************************************)
(* Dialers: the rules of dialing.py `keep_dialing` (and its Go, Node, Bun, *)
(* Swift and Kotlin twins), with the app's stop / start: a round starts    *)
(* afresh, and a stop closes the connection and cancels the retry.         *)
(***************************************************************************)
\* Close the dialer's side of k.
CloseD(k) ==
    /\ ds' = [ds EXCEPT ![k] = "closed"]
    /\ toD' = [toD EXCEPT ![k] = IF Mutation = "matchByNumber" THEN @ ELSE <<>>]

\* Dial: the router lets the token through, the channel is delivered and
\* its handshake spawned. An unregistered row (the mutation
\* revokeUnregisters) has no entry for the token, so the dial is refused
\* at the upgrade instead.
DoDial(d) ==
    /\ dst[d] = "connecting"
    /\ ndial[d] < MaxDials
    /\ free[d] \/ paid[d] < MaxConns
    /\ registered
    /\ LET n == ndial[d] + 1
           k == <<d, dround[d], n>>
       IN /\ ndial' = [ndial EXCEPT ![d] = n]
          /\ paid' = [paid EXCEPT ![d] = IF free[d] THEN @ ELSE @ + 1]
          /\ free' = [free EXCEPT ![d] = FALSE]
          /\ dcur' = [dcur EXCEPT ![d] = n]
          /\ ds' = [ds EXCEPT ![k] = "open"]
          /\ hs' = [hs EXCEPT ![k] = "handshaking"]
          /\ dst' = [dst EXCEPT ![d] = "greeting"]
          /\ ghost' = [ghost EXCEPT !.dialAfterStop =
                         @ \/ (dround[d] \in ghost.stoppedRounds[d])]
    /\ UNCHANGED <<dround, dstopped, faults, toD, plugins, replaced, live,
                   loop, endings, revoked, registered, outcome, helloed>>

\* The listener has no registration of the row any more: the upgrade is
\* refused (403), which the dialer treats as an authentication failure.
DialRefused(d) ==
    /\ ~registered
    /\ dst[d] = "connecting"
    /\ ndial[d] < MaxDials
    /\ ndial' = [ndial EXCEPT ![d] = @ + 1]
    /\ dst' = [dst EXCEPT ![d] = "waiting"]
    /\ outcome' = [outcome EXCEPT ![d] = "authRejected"]
    /\ ghost' = [ghost EXCEPT !.dialAfterStop =
                   @ \/ (dround[d] \in ghost.stoppedRounds[d])]
    /\ UNCHANGED <<dround, dcur, paid, free, dstopped, faults, ds, toD, hs,
                   plugins, replaced, live, loop, endings, revoked, registered,
                   helloed>>

\* A dial failure: no connection was made, so it does not use up one.
DialFails(d) ==
    /\ "lose" \in Faults
    /\ faults < MaxFaults
    /\ dst[d] = "connecting"
    /\ ndial[d] < MaxDials
    /\ free[d] \/ paid[d] < MaxConns
    /\ faults' = faults + 1
    /\ dst' = [dst EXCEPT ![d] = "waiting"]
    /\ ghost' = [ghost EXCEPT !.dialAfterStop =
                   @ \/ (dround[d] \in ghost.stoppedRounds[d])]
    /\ UNCHANGED <<dround, dcur, ndial, paid, free, dstopped, ds, toD, hs,
                   plugins, replaced, live, loop, endings, revoked, registered,
                   outcome, helloed>>

Retry(d) ==
    /\ dst[d] = "waiting"
    /\ dst' = [dst EXCEPT ![d] = "connecting"]
    /\ UNCHANGED <<dround, dcur, ndial, paid, free, dstopped, faults, ds, toD,
                   hs, plugins, replaced, live, loop, endings, revoked,
                   registered, outcome, helloed, ghost>>

\* Read the next frame: rutis' hello (connected), the close (replaced 4002,
\* the runtime stopped, a failed handshake: wait, then dial again), or the
\* rejection of a revoked id (4003, after the hello): waiting as an
\* authentication failure. The hello counts for the dialer only when it
\* comes on the current connection of the current round (matchByNumber
\* matches the number alone).
DRecv(d, k) ==
    LET f == Head(toD[k]) IN
    /\ toD[k] /= <<>>
    /\ ReadsD(k)
    /\ IF f = "hello"
       THEN /\ toD' = [toD EXCEPT ![k] = Tail(@)]
            /\ helloed' = [helloed EXCEPT ![k] = TRUE]
            /\ IF dst[d] = "greeting"
                  /\ (k = CurConn(d)
                      \/ (Mutation = "matchByNumber" /\ dcur[d] > 0 /\ N(k) = dcur[d]))
               THEN /\ dst' = [dst EXCEPT ![d] = "ready"]
                    /\ UNCHANGED <<ds, outcome>>
               ELSE /\ UNCHANGED <<dst, ds, outcome>>
       ELSE /\ CloseD(k)
            /\ dst' = [dst EXCEPT ![d] = IF k = CurConn(d) /\ @ \in {"greeting", "ready"}
                                         THEN "waiting" ELSE @]
            /\ outcome' = [outcome EXCEPT ![d] =
                             IF f = "rejected" THEN "authRejected"
                             ELSE IF Id(k) \in revoked THEN "other" ELSE @]
            /\ UNCHANGED helloed
    /\ UNCHANGED <<dround, dcur, ndial, paid, free, dstopped, faults, hs,
                   plugins, replaced, live, loop, endings, revoked, registered,
                   ghost>>

\* No greeting in time, or the connection is lost: retryable, and its
\* redial is free.
Lose(d) ==
    /\ "lose" \in Faults
    /\ faults < MaxFaults
    /\ dst[d] \in {"greeting", "ready"}
    /\ ds[CurConn(d)] = "open"
    /\ CloseD(CurConn(d))
    /\ dst' = [dst EXCEPT ![d] = "waiting"]
    /\ free' = [free EXCEPT ![d] = TRUE]
    /\ faults' = faults + 1
    /\ UNCHANGED <<dround, dcur, ndial, paid, dstopped, hs, plugins, replaced,
                   live, loop, endings, revoked, registered, outcome, helloed,
                   ghost>>

\* Stopped (SIGTERM): the connection closes, no more dials in this round.
StopDialer(d) ==
    /\ "stop" \in Faults
    /\ ~dstopped[d]
    /\ Cardinality(ghost.stoppedRounds[d]) < MaxStops
    /\ dstopped' = [dstopped EXCEPT ![d] = TRUE]
    /\ IF dcur[d] > 0 /\ ds[CurConn(d)] = "open"
       THEN CloseD(CurConn(d)) ELSE UNCHANGED <<ds, toD>>
    /\ dst' = [dst EXCEPT ![d] =
                 IF Mutation = "stopKeepsRetry" /\ @ = "waiting" THEN @ ELSE "stopped"]
    /\ ghost' = [ghost EXCEPT
                   !.stoppedRounds = [ghost.stoppedRounds EXCEPT
                                        ![d] = ghost.stoppedRounds[d] \cup {dround[d]}],
                   !.final = IF dround[d] = MaxRounds
                             THEN ghost.final \cup {d} ELSE ghost.final]
    /\ UNCHANGED <<dround, dcur, ndial, paid, free, faults, hs, plugins,
                   replaced, live, loop, endings, revoked, registered, outcome,
                   helloed>>

\* The app starts again: the next round numbers its dials from the start.
StartDialer(d) ==
    /\ dstopped[d]
    /\ (dround[d] < MaxRounds
        \/ (Mutation = "finalStopDials" /\ dround[d] = MaxRounds))
    /\ dround' = [dround EXCEPT ![d] = @ + 1]
    /\ dstopped' = [dstopped EXCEPT ![d] = FALSE]
    /\ ndial' = [ndial EXCEPT ![d] = 0]
    /\ paid' = [paid EXCEPT ![d] = 0]
    /\ free' = [free EXCEPT ![d] = FALSE]
    /\ dcur' = [dcur EXCEPT ![d] = 0]
    /\ dst' = [dst EXCEPT ![d] = "connecting"]
    /\ UNCHANGED <<faults, ds, toD, hs, plugins, replaced, live, loop, endings,
                   revoked, registered, outcome, helloed, ghost>>

\* Whether d's current connection is an installed session.
CurrentInstalled(d) == IF dcur[d] > 0 THEN hs[CurConn(d)] = "installed" ELSE FALSE

\* The environment revokes d's id (DIA-5-7/5-10/5-11/5-13, the listen side).
\* The listen row keeps its registration: later connections of the id still
\* reach it, complete their handshake (rutis' hello included) and are
\* rejected with 4003 by Join instead of being installed (revokeInstalls:
\* installed anyway). A revocation while the handshake is under way is
\* caught at Join too (revokeAfterHandshake only looks at what was known
\* when the handshake completed). An installed session unloads; when its
\* channel closes, it is 4003 as well, and the dialer waits as
\* auth-rejected. revokeUnregisters: the row drops its registration too, so
\* later dials are refused at the upgrade (403) and never reach the link.
Revoke(d) ==
    /\ "revoke" \in Faults
    /\ IdOf(d) \notin revoked
    /\ CurrentInstalled(d) \/ dst[d] \in {"connecting", "waiting", "stopped", "ready", "greeting"}
    /\ revoked' = revoked \cup {IdOf(d)}
    /\ IF CurrentInstalled(d)
       THEN hs' = [hs EXCEPT ![CurConn(d)] = "unloading"]
       ELSE UNCHANGED hs
    /\ IF Mutation = "revokeUnregisters"
       THEN registered' = FALSE
       ELSE UNCHANGED registered
    /\ UNCHANGED <<dst, dround, dcur, ndial, paid, free, dstopped, faults, ds,
                   toD, plugins, replaced, live, loop, endings, outcome,
                   helloed, ghost>>

(***************************************************************************)
(* rutis-host/listen.                                                      *)
(***************************************************************************)
\* Send to the dialer of k (dropped once it closed its side).
SendD(k, f) == toD' = [toD EXCEPT ![k] = IF ReadsD(k) THEN Append(@, f) ELSE @]

\* `session::handshake` completes: rutis greets the far end. Nothing of
\* the far end is registered yet. The revocation known at this moment is
\* recorded: a later one is still seen at Join (revokeAfterHandshake is the
\* old behaviour of checking only this snapshot).
HandshakeDone(k) ==
    /\ hs[k] = "handshaking"
    /\ ds[k] = "open"
    /\ hs' = [hs EXCEPT ![k] = "greeted"]
    /\ SendD(k, "hello")
    /\ ghost' = [ghost EXCEPT !.revokedThen =
                   [ghost.revokedThen EXCEPT ![k] = (Id(k) \in revoked)]]
    /\ UNCHANGED <<dialer, faults, ds, plugins, replaced, live, loop,
                   endings, revoked, registered, outcome, helloed>>

\* The handshake fails (the far end left, timed out, spoke the wrong
\* protocol): its channel is closed; `let Ok(greeted) = ... else continue`.
HandshakeFails(k) ==
    /\ hs[k] = "handshaking"
    /\ ds[k] = "closed"
    /\ hs' = [hs EXCEPT ![k] = "failed"]
    /\ SendD(k, "close")
    /\ UNCHANGED <<dialer, faults, ds, plugins, replaced, live, loop,
                   endings, revoked, registered, outcome, helloed, ghost>>

\* The loop takes a finished handshake (`handshakes.join_next()`). A
\* revoked id is not installed: the connection got rutis' hello already and
\* is closed now with 4003, its far plugins never registered (revokeInstalls
\* mutation: installed anyway). Otherwise: no session of its id, install it;
\* or mark the old closer replaced, take the old one out of `live` and start
\* unloading it; the loop then awaits that unload (Replace).
\* acceptBeforeRelease: the new session is installed first, then the old one
\* unloaded.
Join(k) ==
    LET i == Id(k)
        old == live[i]
        refused == IF Mutation = "revokeAfterHandshake"
                   THEN ghost.revokedThen[k]
                   ELSE Id(k) \in revoked /\ Mutation /= "revokeInstalls"
        gone == ds[k] = "closed"
    IN
    /\ loop = Idle
    /\ hs[k] = "greeted"
    /\ IF refused THEN SendD(k, "rejected") ELSE UNCHANGED toD
    /\ IF refused \/ gone
       THEN /\ hs' = [hs EXCEPT ![k] = "unloaded"]
            /\ UNCHANGED <<replaced, live, loop>>
       ELSE /\ IF old = NoConn
               THEN /\ hs' = [hs EXCEPT ![k] = "installed"]
                    /\ live' = [live EXCEPT ![i] = k]
                    /\ UNCHANGED <<replaced, loop>>
               ELSE /\ replaced' = [replaced EXCEPT ![old] = TRUE]
                    /\ loop' = [st |-> "replacing", new |-> k, old |-> old]
                    /\ IF Mutation = "acceptBeforeRelease"
                       THEN /\ hs' = [hs EXCEPT ![old] = IF @ = "installed" THEN "unloading" ELSE @,
                                                ![k] = "installed"]
                            /\ live' = [live EXCEPT ![i] = k]
                       ELSE /\ hs' = [hs EXCEPT ![old] = IF @ = "installed" THEN "unloading" ELSE @]
                            /\ live' = [live EXCEPT ![i] = NoConn]
    /\ UNCHANGED <<dialer, faults, ds, plugins, endings, revoked,
                   registered, outcome, helloed, ghost>>

\* `old.installed.unload().await` has returned: install the new session
\* (`old.closer.close(Replaced)` finds the channel closed already). If the
\* id was revoked while this connection waited for the old one, it is
\* closed with 4003 instead (it had its hello already).
Replace ==
    LET k == loop.new
        i == Id(k)
        refused == IF Mutation = "revokeAfterHandshake"
                   THEN ghost.revokedThen[k]
                   ELSE Id(k) \in revoked /\ Mutation /= "revokeInstalls"
    IN
    /\ loop.st = "replacing"
    /\ hs[loop.old] = "unloaded"
    /\ IF refused THEN SendD(k, "rejected") ELSE UNCHANGED toD
    /\ loop' = Idle
    /\ IF refused \/ ds[k] = "closed"
       THEN /\ hs' = [hs EXCEPT ![k] = "unloaded"]
            /\ UNCHANGED live
       ELSE /\ hs' = [hs EXCEPT ![k] = "installed"]
            /\ live' = [live EXCEPT ![i] = k]
    /\ UNCHANGED <<dialer, faults, ds, plugins, replaced, endings,
                   revoked, registered, outcome, helloed, ghost>>

\* An installed session registers the far end's plugins (`run`, the
\* services it announces).
Register(k) ==
    /\ hs[k] = "installed"
    /\ ~plugins[k]
    /\ plugins' = [plugins EXCEPT ![k] = TRUE]
    /\ UNCHANGED <<dialer, faults, ds, toD, hs, replaced, live, loop,
                   endings, revoked, registered, outcome, helloed, ghost>>

\* A session ends on its own: its far end closed, or (fault) the
\* connection broke. It starts unloading.
EndSelf(k) ==
    /\ hs[k] = "installed"
    /\ ds[k] = "closed"
    /\ hs' = [hs EXCEPT ![k] = "unloading"]
    /\ UNCHANGED <<dialer, faults, ds, toD, plugins, replaced, live, loop,
                   endings, revoked, registered, outcome, helloed, ghost>>

\* The session is unloaded: its far plugins unloaded (in reverse), then its
\* channel closed last, as replaced (4002) if its closer was marked. The
\* watcher reports the ending to the loop.
Unloaded(k) ==
    /\ hs[k] = "unloading"
    /\ hs' = [hs EXCEPT ![k] = "unloaded"]
    /\ plugins' = [plugins EXCEPT ![k] = FALSE]
    /\ SendD(k, IF Id(k) \in revoked THEN "rejected" ELSE "close")
    /\ endings' = endings \cup {k}
    /\ UNCHANGED <<dialer, faults, ds, replaced, live, loop, revoked,
                   registered, outcome, helloed, ghost>>

\* The loop reads an ending (`endings.recv()`): it forgets the session only
\* if it is still the one in `live` (same serial).
ReadEnding(k) ==
    LET i == Id(k) IN
    /\ loop = Idle
    /\ k \in endings
    /\ endings' = endings \ {k}
    /\ live' = [live EXCEPT ![i] = IF @ = k THEN NoConn ELSE @]
    /\ UNCHANGED <<dialer, faults, ds, toD, hs, plugins, replaced, loop,
                   revoked, registered, outcome, helloed, ghost>>

(***************************************************************************)
Init ==
    /\ dst = [d \in Dialers |-> "connecting"]
    /\ dround = [d \in Dialers |-> 1]
    /\ dcur = [d \in Dialers |-> 0]
    /\ ndial = [d \in Dialers |-> 0]
    /\ paid = [d \in Dialers |-> 0]
    /\ free = [d \in Dialers |-> FALSE]
    /\ dstopped = [d \in Dialers |-> FALSE]
    /\ faults = 0
    /\ ds = [k \in Conns |-> "none"]
    /\ toD = [k \in Conns |-> <<>>]
    /\ hs = [k \in Conns |-> "none"]
    /\ plugins = [k \in Conns |-> FALSE]
    /\ replaced = [k \in Conns |-> FALSE]
    /\ live = [i \in Ids |-> NoConn]
    /\ loop = Idle
    /\ endings = {}
    /\ revoked = {}
    /\ registered = TRUE
    /\ outcome = [d \in Dialers |-> "none"]
    /\ helloed = [k \in Conns |-> FALSE]
    /\ ghost = [dialAfterStop |-> FALSE,
                stoppedRounds |-> [d \in Dialers |-> {}],
                final |-> {},
                revokedThen |-> [k \in Conns |-> FALSE]]

Dialer(d) ==
    \/ DoDial(d) \/ DialRefused(d) \/ DialFails(d) \/ Retry(d) \/ Lose(d)
    \/ StopDialer(d) \/ StartDialer(d) \/ Revoke(d)
    \/ \E r \in Rounds, n \in 1..MaxDials : DRecv(d, <<d, r, n>>)

Listen ==
    \/ Replace
    \/ \E k \in Conns :
          \/ HandshakeDone(k) \/ HandshakeFails(k) \/ Join(k) \/ Register(k)
          \/ EndSelf(k) \/ Unloaded(k) \/ ReadEnding(k)

Next == Listen \/ \E d \in Dialers : Dialer(d)

\* Liveness is checked on runs without a stop and with the transient
\* faults bounded by MaxFaults: every behaviour is finite there, so weak
\* fairness of Next makes every enabled step happen.
Spec == Init /\ [][Next]_vars /\ WF_vars(Next)

\* Dialers are interchangeable (safety configurations only).
Symmetry == Permutations(Dialers)

(***************************************************************************)
(* Properties.                                                             *)
(***************************************************************************)
\* Never two sessions of one id with far plugins registered: the new one
\* registers only after the old one fully unloaded.
OnePluginSetPerId ==
    \A i \in Ids : Cardinality({k \in Conns : plugins[k] /\ Id(k) = i}) <= 1

\* A stopped round never dials again and keeps no connection of it open;
\* a dialer that started again dials in its new round only.
StoppedRoundsStayDown ==
    /\ ~ghost.dialAfterStop
    /\ \A d \in Dialers :
          \A r \in ghost.stoppedRounds[d] :
             \A n \in 1..MaxDials : ds[<<d, r, n>>] /= "open"

\* Revoking an id does not unregister the listen row: its registration
\* stays, so later connections of the id reach it and are told 4003.
StaysRegistered == registered

\* A revoked id is never accepted: no session of it is installed after the
\* revocation, a connection revoked during its handshake included.
RevokedStaysOut ==
    \A k \in Conns : Id(k) \in revoked => hs[k] /= "installed"

\* The dialer's `ready` belongs to the current round and the connection
\* whose hello it read: a late hello of an earlier round's connection does
\* not make the new round's connection ready (matchByNumber).
RestartIsFresh ==
    \A d \in Dialers : dst[d] = "ready" =>
        /\ dcur[d] > 0
        /\ ds[CurConn(d)] = "open"
        /\ helloed[CurConn(d)]
        /\ dround[d] \notin ghost.stoppedRounds[d]

\* The stop of the last round is final: no start opens a round after it.
FinalStopStaysDown ==
    \A d \in Dialers : d \in ghost.final => dstopped[d]

\* Liveness: after its id is revoked, a dialer eventually waits as
\* auth-rejected (a later connection is greeted, then closed with 4003).
EventuallyRevokedWaits ==
    \A d \in Dialers :
        IdOf(d) \in revoked => <>(dst[d] = "waiting" /\ outcome[d] = "authRejected")

\* Liveness: each id has a session with its far plugins registered, whose
\* dialer is connected on it.
Held(i) == \E k \in Conns :
              /\ Id(k) = i /\ hs[k] = "installed" /\ plugins[k]
              /\ dst[k[1]] = "ready" /\ CurConn(k[1]) = k
EventuallyHeld == \A i \in Ids : <>Held(i)
EndsHeld == \A i \in Ids : []<>Held(i)
==============================================================================
