--------------------------- MODULE CallLifecycle ----------------------------
(***************************************************************************)
(* Calls on one session between the two endpoints of protocol 4: A calls,  *)
(* B runs the method and replies, A may cancel before the reply arrives,   *)
(* the reply grants a reference that A releases, and either side may close *)
(* the session.                                                            *)
(*                                                                         *)
(* Design: docs/protocol-session.md §5 (call numbers, one settlement),     *)
(* §6 (references) and §7 (cancel); docs/formal-trace.md §3.4 (cancel) and *)
(* §3.5 (errors); docs/design-formal-verification-2026-11-08.md §3.4.      *)
(* Implementation: crates/rutis-bridge/src/session/rpc.rs —                *)
(* `Calls::admit` (a call number is taken once and only grows),            *)
(* `reply_target` (a reply takes its waiter out; the reply of a cancelled  *)
(* call is discarded and counted in `orphans`), `receive` for              *)
(* `Frame::Cancel` (a queued call is dropped without a reply, a running    *)
(* one still replies) and `Frame::Release`, `Connection::cancel`, and      *)
(* `Connection::close` (waiting calls fail, the export and import tables   *)
(* are cleared). README.md next to this file lists the simplifications.    *)
(*                                                                         *)
(* A calls; B admits, runs and answers. The session's two directions are   *)
(* FIFO and delayed arbitrarily long, so a reply may be in flight when a   *)
(* cancel is written and the reply of a cancelled call arrives late.       *)
(* Either side may close the session; the other side learns it when the    *)
(* channel ends.                                                           *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS
    Calls,       \* the calls A may start ("c1", "c2"), in no fixed order
    MaxCancels,  \* how often A may cancel in the session (1)
    Closes,      \* either side may close the session
    Mutation     \* "none", or a deliberate bug (README, "Mutations")

ASSUME Calls /= {}
ASSUME MaxCancels \in Nat
ASSUME Closes \in BOOLEAN
ASSUME Mutation \in {"none", "cancelCloses", "lateReplyTwice", "closeLeaks",
                     "reuseId"}

VARIABLES
    aclosed,     \* A's session is over
    bclosed,     \* B's session is over
    closeCause,  \* ghost: what closed the session: "none" | "a" | "b" | "cancel"
    acall,       \* per call at A: "idle" | "waiting" | "settled" | "cancelled"
    anext,       \* the number A gives its next call
    aid,         \* per call: the number A gave it (0 before it started)
    bcall,       \* per call at B: "none" | "queued" | "running" | "done" |
                 \*   "dropped"
    bseq,        \* per call: the number B read with the call
    cancels,     \* how often A cancelled
    toA,         \* B -> A frames, FIFO: <<"return", call, number>>
    toB,         \* A -> B frames, FIFO: <<"invoke", call, number>>,
                 \*   <<"cancel", call>>, <<"release", count>>
    aheld,       \* reference grants A holds
    bgrants,     \* reference grants B made and nobody released yet
    granted,     \* ghost: how often B granted the reference
    aReleased,   \* ghost: grant units A gave back (a release written, or a
                 \* discarded reply's value released at once)
    aCleared,    \* ghost: grant units A's close dropped
    settles,     \* per call: how often it was settled at A
    outcome,     \* per call: "none" | "ok" | "cancel" | "closed"
    settledBy,   \* per call: the call whose reply settled it ("none" before)
    orphaned     \* per call: replies discarded after the call was cancelled

vars == <<aclosed, bclosed, closeCause, acall, anext, aid, bcall, bseq,
          cancels, toA, toB, aheld, bgrants, granted, aReleased, aCleared,
          settles, outcome, settledBy, orphaned>>

Init ==
    /\ aclosed = FALSE
    /\ bclosed = FALSE
    /\ closeCause = "none"
    /\ acall = [c \in Calls |-> "idle"]
    /\ anext = 1
    /\ aid = [c \in Calls |-> 0]
    /\ bcall = [c \in Calls |-> "none"]
    /\ bseq = [c \in Calls |-> 0]
    /\ cancels = 0
    /\ toA = <<>>
    /\ toB = <<>>
    /\ aheld = 0
    /\ bgrants = 0
    /\ granted = 0
    /\ aReleased = 0
    /\ aCleared = 0
    /\ settles = [c \in Calls |-> 0]
    /\ outcome = [c \in Calls |-> "none"]
    /\ settledBy = [c \in Calls |-> "none"]
    /\ orphaned = [c \in Calls |-> 0]

(***************************************************************************)
(* A: starting calls, reading replies, cancelling, releasing, closing.     *)
(***************************************************************************)

\* A call takes the next number. The mutation keeps the counter, so a new
\* call can take a number an older call already has.
AStart(c) ==
    /\ ~aclosed
    /\ ~bclosed
    /\ acall[c] = "idle"
    /\ acall' = [acall EXCEPT ![c] = "waiting"]
    /\ aid' = [aid EXCEPT ![c] = anext]
    /\ toB' = Append(toB, <<"invoke", c, anext>>)
    /\ anext' = IF Mutation = "reuseId" THEN anext ELSE anext + 1
    /\ UNCHANGED <<aclosed, bclosed, closeCause, bcall, bseq, cancels, toA,
                   aheld, bgrants, granted, aReleased, aCleared, settles,
                   outcome, settledBy, orphaned>>

\* A reply for a call A still waits for: the call whose number the reply
\* carries is settled once, with the reference the reply also carries.
ADeliverLive(d) ==
    /\ ~aclosed
    /\ toA /= <<>>
    /\ LET f == Head(toA) IN
       /\ aid[d] = f[3]
       /\ acall[d] = "waiting"
       /\ toA' = Tail(toA)
       /\ acall' = [acall EXCEPT ![d] = "settled"]
       /\ settles' = [settles EXCEPT ![d] = @ + 1]
       /\ outcome' = [outcome EXCEPT ![d] = "ok"]
       /\ settledBy' = [settledBy EXCEPT ![d] = f[2]]
       /\ aheld' = aheld + 1
       /\ UNCHANGED <<aclosed, bclosed, closeCause, anext, aid, bcall, bseq,
                      cancels, toB, bgrants, granted, aReleased, aCleared,
                      orphaned>>

\* A reply for a call A cancelled: it is decoded (so the reference it carries
\* is released at once) and discarded. The session goes on.
\* The mutation applies it as if the call were still waiting: the call is
\* settled a second time and its value kept.
ADeliverLate(d) ==
    /\ ~aclosed
    /\ toA /= <<>>
    /\ LET f == Head(toA)
           bug == Mutation = "lateReplyTwice"
       IN /\ aid[d] = f[3]
          /\ acall[d] = "cancelled"
          /\ toA' = Tail(toA)
          /\ orphaned' = [orphaned EXCEPT ![d] = @ + 1]
          /\ acall' = IF bug THEN [acall EXCEPT ![d] = "settled"] ELSE acall
          /\ settles' = IF bug THEN [settles EXCEPT ![d] = @ + 1] ELSE settles
          /\ outcome' = IF bug THEN [outcome EXCEPT ![d] = "ok"] ELSE outcome
          /\ settledBy' = IF bug
                          THEN [settledBy EXCEPT ![d] = f[2]] ELSE settledBy
          /\ aheld' = IF bug THEN aheld + 1 ELSE aheld
          /\ aReleased' = IF bug THEN aReleased ELSE aReleased + 1
          /\ toB' = IF bug THEN toB ELSE Append(toB, <<"release", 1>>)
          /\ UNCHANGED <<aclosed, bclosed, closeCause, anext, aid, bcall,
                         bseq, cancels, bgrants, granted, aCleared>>

\* A drops what it holds and releases it: the count is what A holds now.
ARelease(n) ==
    /\ ~aclosed
    /\ ~bclosed
    /\ n \in 1..aheld
    /\ aheld' = aheld - n
    /\ aReleased' = aReleased + n
    /\ toB' = Append(toB, <<"release", n>>)
    /\ UNCHANGED <<aclosed, bclosed, closeCause, acall, anext, aid, bcall,
                   bseq, cancels, toA, bgrants, granted, aCleared, settles,
                   outcome, settledBy, orphaned>>

\* A closes its side: the calls it still waits for fail, its imports go, and
\* the frames in both directions are lost.
CloseA(cause) ==
    /\ aclosed' = TRUE
    /\ closeCause' = IF closeCause = "none" THEN cause ELSE closeCause
    /\ acall' = [c \in Calls |-> IF acall[c] = "waiting" THEN "settled"
                                      ELSE acall[c]]
    /\ settles' = [c \in Calls |-> IF acall[c] = "waiting" THEN settles[c] + 1
                                         ELSE settles[c]]
    /\ outcome' = [c \in Calls |-> IF acall[c] = "waiting" THEN "closed"
                                         ELSE outcome[c]]
    /\ aheld' = 0
    /\ aCleared' = aCleared + aheld
    /\ toA' = <<>>
    /\ toB' = <<>>
    /\ UNCHANGED <<bclosed, anext, aid, bcall, bseq, bgrants, granted,
                   aReleased, settledBy, orphaned>>

AClose ==
    /\ Closes
    /\ ~aclosed
    /\ CloseA("a")
    /\ UNCHANGED <<cancels>>

\* B's close reached A: the channel ended, so A sees it.
ALearn ==
    /\ bclosed
    /\ ~aclosed
    /\ CloseA("b")
    /\ UNCHANGED <<cancels>>

\* A gives up on a call: the caller gets its own error, and a reply that was
\* already on its way is discarded. The cancel frame goes out after the
\* invoke, in this direction's order, and does not end the session.
\* The mutation ends A's session right here.
CancelFrame(c) ==
    /\ acall' = [acall EXCEPT ![c] = "cancelled"]
    /\ settles' = [settles EXCEPT ![c] = @ + 1]
    /\ outcome' = [outcome EXCEPT ![c] = "cancel"]
    /\ toB' = Append(toB, <<"cancel", c>>)
    /\ UNCHANGED <<aclosed, bclosed, closeCause, aheld, aCleared, toA>>

ACancel(c) ==
    /\ ~aclosed
    /\ ~bclosed
    /\ acall[c] = "waiting"
    /\ cancels < MaxCancels
    /\ cancels' = cancels + 1
    /\ IF Mutation = "cancelCloses" THEN CloseA("cancel") ELSE CancelFrame(c)
    /\ UNCHANGED <<anext, aid, bcall, bseq, bgrants, granted, aReleased,
                   settledBy, orphaned>>

(***************************************************************************)
(* B: admitting calls, running them, replying, and reading cancels and     *)
(* releases.                                                               *)
(***************************************************************************)

\* B takes the next call. A call number only grows, so a number an older
\* call already took is not a new call.
BReadInvoke(c) ==
    /\ ~aclosed
    /\ ~bclosed
    /\ toB /= <<>>
    /\ Head(toB)[1] = "invoke"
    /\ Head(toB)[2] = c
    /\ bcall[c] = "none"
    /\ bcall' = [bcall EXCEPT ![c] = "queued"]
    /\ bseq' = [bseq EXCEPT ![c] = Head(toB)[3]]
    /\ toB' = Tail(toB)
    /\ UNCHANGED <<aclosed, bclosed, closeCause, acall, anext, aid, cancels,
                   toA, aheld, bgrants, granted, aReleased, aCleared, settles,
                   outcome, settledBy, orphaned>>

\* The method starts. How long it takes is not modelled: the reply may be
\* written at any later point, also after a cancel arrived.
BRun(c) ==
    /\ bcall[c] = "queued"
    /\ bcall' = [bcall EXCEPT ![c] = "running"]
    /\ UNCHANGED <<aclosed, bclosed, closeCause, acall, anext, aid, bseq,
                   cancels, toA, toB, aheld, bgrants, granted, aReleased,
                   aCleared, settles, outcome, settledBy, orphaned>>

\* The method answers: the call is done at B, and the reply (with the one
\* reference it carries) goes out. A running call answers also after a
\* cancel arrived; a queued one is dropped instead (BReadCancel).
BReply(c) ==
    /\ bcall[c] = "running"
    /\ bcall' = [bcall EXCEPT ![c] = "done"]
    /\ toA' = Append(toA, <<"return", c, bseq[c]>>)
    /\ bgrants' = bgrants + 1
    /\ granted' = granted + 1
    /\ UNCHANGED <<aclosed, bclosed, closeCause, acall, anext, aid, bseq,
                   cancels, toB, aheld, aReleased, aCleared, settles, outcome,
                   settledBy, orphaned>>

\* A cancel arrives. A call still queued never runs and is not answered; one
\* already running keeps running and answers; one already answered is not
\* touched (the cancel crossed its reply). Nothing else changes.
BReadCancel(c) ==
    /\ ~aclosed
    /\ ~bclosed
    /\ toB /= <<>>
    /\ Head(toB) = <<"cancel", c>>
    /\ toB' = Tail(toB)
    /\ bcall' = IF bcall[c] = "queued" THEN [bcall EXCEPT ![c] = "dropped"]
               ELSE bcall
    /\ UNCHANGED <<aclosed, bclosed, closeCause, acall, anext, aid, bseq,
                   cancels, toA, aheld, bgrants, granted, aReleased, aCleared,
                   settles, outcome, settledBy, orphaned>>

\* A release arrives: the grants it names are given up. The export goes away
\* once nothing is granted any more.
BReadRelease(n) ==
    /\ ~aclosed
    /\ ~bclosed
    /\ toB /= <<>>
    /\ Head(toB) = <<"release", n>>
    /\ n \in 1..bgrants
    /\ toB' = Tail(toB)
    /\ bgrants' = bgrants - n
    /\ UNCHANGED <<aclosed, bclosed, closeCause, acall, anext, aid, bcall,
                   bseq, cancels, toA, aheld, granted, aReleased, aCleared,
                   settles, outcome, settledBy, orphaned>>

\* B closes its side: the calls it has not answered are dropped, its exports
\* go, and the frames in both directions are lost.
\* The mutation keeps the export table.
CloseB(cause) ==
    /\ bclosed' = TRUE
    /\ closeCause' = IF closeCause = "none" THEN cause ELSE closeCause
    /\ bcall' = [c \in Calls |-> IF bcall[c] \in {"queued", "running"}
                                   THEN "dropped" ELSE bcall[c]]
    /\ bgrants' = IF Mutation = "closeLeaks" THEN bgrants ELSE 0
    /\ toA' = <<>>
    /\ toB' = <<>>
    /\ UNCHANGED <<aclosed, acall, anext, aid, bseq, cancels, aheld, granted,
                   aReleased, aCleared, settles, outcome, settledBy, orphaned>>

BClose ==
    /\ Closes
    /\ ~bclosed
    /\ CloseB("b")

\* A's close reached B.
BLearn ==
    /\ aclosed
    /\ ~bclosed
    /\ CloseB("a")

(***************************************************************************)
(* The specification.                                                      *)
(***************************************************************************)

Next ==
    \/ \E c \in Calls : AStart(c) \/ ACancel(c)
    \/ \E c \in Calls : ADeliverLive(c) \/ ADeliverLate(c)
    \/ \E n \in 1..aheld : ARelease(n)
    \/ AClose
    \/ ALearn
    \/ \E c \in Calls : BReadInvoke(c) \/ BRun(c) \/ BReply(c)
         \/ BReadCancel(c)
    \/ \E n \in 1..bgrants : BReadRelease(n)
    \/ BClose
    \/ BLearn

Spec == Init /\ [][Next]_vars /\ WF_vars(Next)

(***************************************************************************)
(* Properties.                                                             *)
(***************************************************************************)

\* A cancel never ends the session: the close that ends it is the caller's,
\* the callee's, or the one the channel end brings.
CancelKeepsSession == closeCause # "cancel"

\* A call is settled at most once, whether by its reply, by a cancel or by
\* the session ending.
SettlesAtMostOnce == \A c \in Calls : settles[c] <= 1

\* A late reply is discarded: the cancelled call keeps the outcome the cancel
\* gave it, and the reply does not settle it a second time. Each call has at
\* most one reply to discard.
LateReplyDropped ==
    \A c \in Calls :
        orphaned[c] > 0 => (outcome[c] = "cancel" /\ settles[c] = 1)
        /\ orphaned[c] <= 1

\* The reply that settles a call is the reply of that call.
OwnReply == \A c \in Calls : settledBy[c] \in {"none", c}

\* A call number belongs to one call: no new call takes it again.
NoIdReuse ==
    \A c, d \in Calls :
        c # d /\ aid[c] # 0 /\ aid[d] # 0 => aid[c] # aid[d]

\* A gives back only what it was granted: a grant is released, dropped or
\* cleared once, never twice.
NoOverRelease == aheld + aReleased + aCleared <= granted

\* Once A's session is over it holds no reference, and once B's is over it
\* exports none.
ClosedClearsTables ==
    (aclosed => aheld = 0) /\ (bclosed => bgrants = 0)

\* A closed session leaves no call unfinished on either side.
FailUnfinishedOnClose ==
    /\ (aclosed => \A c \in Calls : acall[c] \in {"idle", "settled",
                                                   "cancelled"})
    /\ (bclosed => \A c \in Calls : bcall[c] \in {"none", "done", "dropped"})

\* Liveness, with no close to fall back on: a call A waits for is answered
\* or cancelled, so the callee answers what it took.
WaitingSettles ==
    \A c \in Calls : (acall[c] = "waiting") ~> (acall[c] \in {"settled",
                                                              "cancelled"})
=============================================================================
