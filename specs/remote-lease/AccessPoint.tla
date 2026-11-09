----------------------------- MODULE AccessPoint -----------------------------
(***************************************************************************)
(* Far ends dialing a `rutis-host/listen` row: handshakes side by side,    *)
(* one session per id, a newer connection of a connected id replacing the  *)
(* older, the dialers' reconnection rules, and the static listen link whose *)
(* id is revoked (the link keeps its registration, waits as auth-rejected, *)
(* and closes later connections of that id with 4003 after their hello).   *)
(*                                                                         *)
(* Design: docs/design-remote-session-2026-10-28.md §4.2 (replacement:     *)
(* unload the old session, close its channel as replaced, then install the *)
(* new one); remote plugins §4.4; remote session §4.3 (the link's status). *)
(* Implementation modelled: crates/rutis-host/src/connect/listen.rs        *)
(* (`Listening::run`, `Replaceable`); the dialers:                         *)
(* python/rutis/rutis/dialing.py `keep_dialing` (and its Go, Node, Bun     *)
(* twins). README.md next to this file lists the simplifications.          *)
(*                                                                         *)
(* Connection <<d, n>> is the n-th connection dialer d made; d greets with *)
(* the id IdOf(d). Frames to the dialer are FIFO and delayed arbitrarily   *)
(* long; a side that closed a connection reads nothing more on it.         *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS
    Dialers,      \* far ends dialing (model values)
    SameId,       \* TRUE: every dialer greets with the same id
    MaxConns,     \* connections each dialer may make (1 + reconnects)
    Faults,       \* what the environment may do: "lose" (failed dial,
                  \* failed handshake, lost connection, a session ending),
                  \* "stop" (a dialer stops), "revoke" (a dialer's id is
                  \* revoked once)
    Mutation      \* "none", or a deliberate bug (README, "Mutations")

ASSUME SameId \in BOOLEAN
ASSUME MaxConns \in Nat \ {0}
ASSUME Faults \subseteq {"lose", "stop", "revoke"}
ASSUME Mutation \in {"none", "acceptBeforeRelease", "stopKeepsRetry",
                     "revokeInstalls", "revokeUnregisters"}

\* The id a dialer greets with: its own name, or one shared by all.
IdOf(d) == IF SameId THEN "one" ELSE d
Ids == {IdOf(d) : d \in Dialers}
Conns == Dialers \X (1..MaxConns)
Id(k) == IdOf(k[1])
NoConn == <<"none", 0>>

VARIABLES
    dst,        \* dst[d]: "connecting" | "greeting" | "ready" | "waiting" | "stopped"
    dcur,       \* dcur[d]: number of d's current connection, 0 before the first
    ndial,      \* ndial[d]: connections d made
    dstopped,   \* dstopped[d]: d was stopped (SIGTERM, `stop()`)
    ds,         \* ds[k]: dialer side of k: "none" | "open" | "closed"
    toD,        \* toD[k]: frames to the dialer: "hello" | "close"
    hs,         \* hs[k]: listen side of k (below)
    plugins,    \* plugins[k]: the far plugins of k's session are registered
    replaced,   \* replaced[k]: k's `Replaceable` closer is marked replaced
    live,       \* live[i]: listen's `live` map: the session of id i
    loop,       \* where `Listening::run` is: [st, new, old]
    endings,    \* sessions unloaded whose ending listen has not read yet
    revoked,    \* ids whose identity was revoked
    registered, \* the listen row's registration on its listener
    outcome,    \* outcome[d]: why d last waited ("none", "authRejected")
    ghost       \* observations for the invariants

vars == <<dst, dcur, ndial, dstopped, ds, toD, hs, plugins, replaced, live,
          loop, endings, revoked, registered, outcome, ghost>>
dialer == <<dst, dcur, ndial, dstopped, ds>>

\* hs[k]: "none"; "handshaking" (in the JoinSet); "greeted" (handshake done,
\* waiting for the loop); "failed" (handshake failed, channel closed);
\* "installed"; "unloading"; "unloaded" (every far plugin unloaded, channel
\* closed).
\* loop.st: "idle" (in `select!`), or "replacing": the arm for loop.new is
\* awaiting the unload of loop.old; nothing else runs in the loop meanwhile.
Idle == [st |-> "idle", new |-> NoConn, old |-> NoConn]
DCur(d) == <<d, dcur[d]>>
ReadsD(k) == ds[k] = "open"

(***************************************************************************)
(* Dialers: the rules of dialing.py `keep_dialing`.                        *)
(***************************************************************************)
\* Close the dialer's side of k.
CloseD(k) ==
    /\ ds' = [ds EXCEPT ![k] = "closed"]
    /\ toD' = [toD EXCEPT ![k] = <<>>]

\* Dial: the router lets the token through, the channel is delivered and
\* its handshake spawned. An unregistered row (the mutation
\* revokeUnregisters) has no entry for the token, so the dial is refused
\* at the upgrade instead.
DoDial(d) ==
    /\ dst[d] = "connecting"
    /\ ndial[d] < MaxConns
    /\ registered
    /\ LET n == ndial[d] + 1
           k == <<d, n>>
       IN /\ ndial' = [ndial EXCEPT ![d] = n]
          /\ dcur' = [dcur EXCEPT ![d] = n]
          /\ ds' = [ds EXCEPT ![k] = "open"]
          /\ hs' = [hs EXCEPT ![k] = "handshaking"]
          /\ dst' = [dst EXCEPT ![d] = "greeting"]
          /\ ghost' = [ghost EXCEPT !.dialAfterStop = @ \/ dstopped[d]]
    /\ UNCHANGED <<dstopped, toD, plugins, replaced, live, loop, endings,
                   revoked, registered, outcome>>

\* The listener has no registration of the row any more: the upgrade is
\* refused (403), which the dialer treats as an authentication failure.
DialRefused(d) ==
    /\ ~registered
    /\ dst[d] = "connecting"
    /\ ndial[d] < MaxConns
    /\ ndial' = [ndial EXCEPT ![d] = @ + 1]
    /\ dst' = [dst EXCEPT ![d] = "waiting"]
    /\ outcome' = [outcome EXCEPT ![d] = "authRejected"]
    /\ ghost' = [ghost EXCEPT !.dialAfterStop = @ \/ dstopped[d]]
    /\ UNCHANGED <<dcur, dstopped, ds, toD, hs, plugins, replaced, live,
                   loop, endings, revoked, registered>>

DialFails(d) ==
    /\ "lose" \in Faults
    /\ dst[d] = "connecting"
    /\ ndial[d] < MaxConns
    /\ ndial' = [ndial EXCEPT ![d] = @ + 1]
    /\ dst' = [dst EXCEPT ![d] = "waiting"]
    /\ ghost' = [ghost EXCEPT !.dialAfterStop = @ \/ dstopped[d]]
    /\ UNCHANGED <<dcur, dstopped, ds, toD, hs, plugins, replaced, live,
                   loop, endings, revoked, registered, outcome>>

Retry(d) ==
    /\ dst[d] = "waiting"
    /\ dst' = [dst EXCEPT ![d] = "connecting"]
    /\ UNCHANGED <<dcur, ndial, dstopped, ds, toD, hs, plugins, replaced,
                   live, loop, endings, revoked, registered, outcome, ghost>>

\* Read the next frame: rutis' hello (connected), the close (replaced 4002,
\* the runtime stopped, a failed handshake: wait, then dial again), or the
\* rejection of a revoked id (4003, after the hello): waiting as an
\* authentication failure.
DRecv(d, n) ==
    LET k == <<d, n>>
        f == Head(toD[k])
    IN
    /\ toD[k] /= <<>>
    /\ ReadsD(k)
    /\ IF f = "hello"
       THEN /\ toD' = [toD EXCEPT ![k] = Tail(@)]
            /\ dst' = [dst EXCEPT ![d] = IF @ = "greeting" /\ dcur[d] = n THEN "ready" ELSE @]
            /\ UNCHANGED <<ds, outcome>>
       ELSE /\ CloseD(k)
            /\ dst' = [dst EXCEPT ![d] = IF dcur[d] = n /\ @ \in {"greeting", "ready"}
                                         THEN "waiting" ELSE @]
            /\ outcome' = [outcome EXCEPT ![d] =
                             IF f = "rejected" THEN "authRejected"
                             ELSE IF Id(k) \in revoked THEN "other" ELSE @]
    /\ UNCHANGED <<dcur, ndial, dstopped, hs, plugins, replaced, live, loop,
                   endings, revoked, registered, ghost>>

\* No greeting in time, or the connection is lost: retryable.
Lose(d) ==
    /\ "lose" \in Faults
    /\ dst[d] \in {"greeting", "ready"}
    /\ ds[DCur(d)] = "open"
    /\ CloseD(DCur(d))
    /\ dst' = [dst EXCEPT ![d] = "waiting"]
    /\ UNCHANGED <<dcur, ndial, dstopped, hs, plugins, replaced, live, loop,
                   endings, revoked, registered, outcome, ghost>>

\* Stopped (SIGTERM): the connection closes, no more dials.
StopDialer(d) ==
    /\ "stop" \in Faults
    /\ ~dstopped[d]
    /\ dstopped' = [dstopped EXCEPT ![d] = TRUE]
    /\ IF dcur[d] > 0 /\ ds[DCur(d)] = "open"
       THEN CloseD(DCur(d)) ELSE UNCHANGED <<ds, toD>>
    /\ dst' = [dst EXCEPT ![d] =
                 IF Mutation = "stopKeepsRetry" /\ @ = "waiting" THEN @ ELSE "stopped"]
    /\ UNCHANGED <<dcur, ndial, hs, plugins, replaced, live, loop, endings,
                   revoked, registered, outcome, ghost>>

\* Whether d's current connection is an installed session.
CurrentInstalled(d) == IF dcur[d] > 0 THEN hs[DCur(d)] = "installed" ELSE FALSE

\* The environment revokes d's id (DIA-5-7/5-10/5-11/5-13). The listen row
\* keeps its registration: later connections of the id still reach it,
\* complete their handshake (rutis' hello included) and are rejected with
\* 4003 by Join instead of being installed (revokeInstalls: installed
\* anyway). An installed session unloads; when its channel closes, it is
\* 4003 as well, and the dialer waits as auth-rejected. revokeUnregisters:
\* the row drops its registration too, so later dials are refused at the
\* upgrade (403) and never reach the link.
Revoke(d) ==
    /\ "revoke" \in Faults
    /\ IdOf(d) \notin revoked
    /\ CurrentInstalled(d) \/ dst[d] \in {"connecting", "waiting", "stopped", "ready"}
    /\ revoked' = revoked \cup {IdOf(d)}
    /\ IF CurrentInstalled(d)
       THEN hs' = [hs EXCEPT ![DCur(d)] = "unloading"]
       ELSE UNCHANGED hs
    /\ IF Mutation = "revokeUnregisters"
       THEN registered' = FALSE
       ELSE UNCHANGED registered
    /\ UNCHANGED <<dst, dcur, ndial, dstopped, ds, toD, plugins, replaced,
                   live, loop, endings, outcome, ghost>>

(***************************************************************************)
(* rutis-host/listen.                                                      *)
(***************************************************************************)
\* Send to the dialer of k (dropped once it closed its side).
SendD(k, f) == toD' = [toD EXCEPT ![k] = IF ReadsD(k) THEN Append(@, f) ELSE @]

\* `session::handshake` completes: rutis greets the far end. Nothing of
\* the far end is registered yet.
HandshakeDone(k) ==
    /\ hs[k] = "handshaking"
    /\ ds[k] = "open"
    /\ hs' = [hs EXCEPT ![k] = "greeted"]
    /\ SendD(k, "hello")
    /\ UNCHANGED <<dialer, plugins, replaced, live, loop, endings, revoked,
                   registered, outcome, ghost>>

\* The handshake fails (the far end left, timed out, spoke the wrong
\* protocol): its channel is closed; `let Ok(greeted) = ... else continue`.
HandshakeFails(k) ==
    /\ hs[k] = "handshaking"
    /\ ds[k] = "closed" \/ "lose" \in Faults
    /\ hs' = [hs EXCEPT ![k] = "failed"]
    /\ SendD(k, "close")
    /\ UNCHANGED <<dialer, plugins, replaced, live, loop, endings, revoked,
                   registered, outcome, ghost>>

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
        refused == Id(k) \in revoked /\ Mutation /= "revokeInstalls"
    IN
    /\ loop = Idle
    /\ hs[k] = "greeted"
    /\ IF refused THEN SendD(k, "rejected") ELSE UNCHANGED toD
    /\ IF refused
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
    /\ UNCHANGED <<dialer, plugins, endings, revoked, registered, outcome, ghost>>

\* `old.installed.unload().await` has returned: install the new session
\* (`old.closer.close(Replaced)` finds the channel closed already). If the
\* id was revoked while this connection waited for the old one, it is
\* closed with 4003 instead (it had its hello already).
Replace ==
    LET k == loop.new
        i == Id(k)
        refused == Id(k) \in revoked /\ Mutation /= "revokeInstalls"
    IN
    /\ loop.st = "replacing"
    /\ hs[loop.old] = "unloaded"
    /\ IF refused THEN SendD(k, "rejected") ELSE UNCHANGED toD
    /\ loop' = Idle
    /\ IF refused
       THEN /\ hs' = [hs EXCEPT ![k] = "unloaded"]
            /\ UNCHANGED live
       ELSE /\ hs' = [hs EXCEPT ![k] = "installed"]
            /\ live' = [live EXCEPT ![i] = k]
    /\ UNCHANGED <<dialer, plugins, replaced, endings, revoked, registered,
                   outcome, ghost>>

\* An installed session registers the far end's plugins (`run`, the
\* services it announces).
Register(k) ==
    /\ hs[k] = "installed"
    /\ ~plugins[k]
    /\ plugins' = [plugins EXCEPT ![k] = TRUE]
    /\ UNCHANGED <<dialer, toD, hs, replaced, live, loop, endings, revoked,
                   registered, outcome, ghost>>

\* A session ends on its own: its far end closed, or (fault) the
\* connection broke. It starts unloading.
EndSelf(k) ==
    /\ hs[k] = "installed"
    /\ ds[k] = "closed" \/ "lose" \in Faults
    /\ hs' = [hs EXCEPT ![k] = "unloading"]
    /\ UNCHANGED <<dialer, toD, plugins, replaced, live, loop, endings,
                   revoked, registered, outcome, ghost>>

\* The session is unloaded: its far plugins unloaded (in reverse), then its
\* channel closed last, as replaced (4002) if its closer was marked. The
\* watcher reports the ending to the loop.
Unloaded(k) ==
    /\ hs[k] = "unloading"
    /\ hs' = [hs EXCEPT ![k] = "unloaded"]
    /\ plugins' = [plugins EXCEPT ![k] = FALSE]
    /\ SendD(k, IF Id(k) \in revoked THEN "rejected" ELSE "close")
    /\ endings' = endings \cup {k}
    /\ UNCHANGED <<dialer, replaced, live, loop, revoked, registered, outcome,
                   ghost>>

\* The loop reads an ending (`endings.recv()`): it forgets the session only
\* if it is still the one in `live` (same serial).
ReadEnding(k) ==
    LET i == Id(k) IN
    /\ loop = Idle
    /\ k \in endings
    /\ endings' = endings \ {k}
    /\ live' = [live EXCEPT ![i] = IF @ = k THEN NoConn ELSE @]
    /\ UNCHANGED <<dialer, toD, hs, plugins, replaced, loop, revoked,
                   registered, outcome, ghost>>

(***************************************************************************)
Init ==
    /\ dst = [d \in Dialers |-> "connecting"]
    /\ dcur = [d \in Dialers |-> 0]
    /\ ndial = [d \in Dialers |-> 0]
    /\ dstopped = [d \in Dialers |-> FALSE]
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
    /\ ghost = [dialAfterStop |-> FALSE]

Dialer(d) ==
    \/ DoDial(d) \/ DialRefused(d) \/ DialFails(d) \/ Retry(d) \/ Lose(d)
    \/ StopDialer(d) \/ Revoke(d)
    \/ \E n \in 1..MaxConns : DRecv(d, n)

Listen ==
    \/ Replace
    \/ \E k \in Conns :
          \/ HandshakeDone(k) \/ HandshakeFails(k) \/ Join(k) \/ Register(k)
          \/ EndSelf(k) \/ Unloaded(k) \/ ReadEnding(k)

Next == Listen \/ \E d \in Dialers : Dialer(d)

\* Liveness is checked only without faults: every behaviour is finite
\* there, so weak fairness of Next makes every enabled step happen.
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

\* A stopped dialer never dials again and keeps no connection open.
StoppedStaysDown ==
    /\ ~ghost.dialAfterStop
    /\ \A d \in Dialers : dstopped[d] =>
          \A n \in 1..MaxConns : ds[<<d, n>>] /= "open"

\* Revoking an id does not unregister the listen row: its registration
\* stays, so later connections of the id reach it and are told 4003.
StaysRegistered == registered

\* A revoked id is never accepted: no session of it is installed after the
\* revocation (a connection that greets later is closed after its hello).
RevokedStaysOut ==
    \A k \in Conns : Id(k) \in revoked => hs[k] /= "installed"

\* Liveness: after its id is revoked, a dialer eventually waits as
\* auth-rejected (a later connection is greeted, then closed with 4003).
EventuallyRevokedWaits ==
    \A d \in Dialers :
        IdOf(d) \in revoked => <>(dst[d] = "waiting" /\ outcome[d] = "authRejected")

\* Liveness: each id has a session with its far plugins registered, whose
\* dialer is connected on it.
Held(i) == \E k \in Conns :
              /\ Id(k) = i /\ hs[k] = "installed" /\ plugins[k]
              /\ dst[k[1]] = "ready" /\ dcur[k[1]] = k[2]
EventuallyHeld == \A i \in Ids : <>Held(i)
EndsHeld == \A i \in Ids : []<>Held(i)
==============================================================================
