--------------------------- MODULE CallLifecycle ----------------------------
(***************************************************************************)
(* Calls on one session between the two endpoints of protocol 4: A calls,  *)
(* B runs the method and replies, A may cancel before the reply arrives,   *)
(* the reply grants a reference that A releases, and either side may close *)
(* the session.                                                            *)
(*                                                                         *)
(* The model also answers three questions of #626:                         *)
(*  - What a reader thread's write-back does when its outbound channel is  *)
(*    full. `ReaderWrites = "direct"` is the shape before #1005: the       *)
(*    reader writes on the channel and blocks in `send`. `"queued"` is     *)
(*    today's: the reader hands the frame to the writer thread (an         *)
(*    unbounded queue) and goes on reading; the writer thread is the one   *)
(*    that blocks. The pre-#1005 shape deadlocks: both readers block on a  *)
(*    full channel while the only frames that would free a channel are     *)
(*    behind the other blocked reader.                                     *)
(*  - Whether a callee answers a call it cancelled while running.          *)
(*    `Replies = "always" | "never"`, both allowed (SESS-61): a call still *)
(*    queued is dropped without a reply in both.                           *)
(*  - What a locally abandoned synchronous call leaves behind. `Abandon =  *)
(*    "keep"` keeps the caller's waiter until the reply (today's sync      *)
(*    wait, which has no drop guard); `"drop"` forgets it without a        *)
(*    cancel, and the reply then is an unknown call and ends the session.  *)
(*                                                                         *)
(* Design: docs/protocol-session.md §5 (call numbers, one settlement),     *)
(* §6 (references), §7 (cancel, "the callee's reply after a cancel is not  *)
(* guaranteed"), §10 (backlog); docs/formal-trace.md §3.4 (cancel) and     *)
(* §3.5 (errors); docs/design-formal-verification-2026-11-08.md §3.4;      *)
(* issue #626.                                                             *)
(* Implementation: crates/rutis-bridge/src/session/rpc.rs —                *)
(* `Calls::admit` (a call number is taken once and only grows),            *)
(* `reply_target` (a reply takes its waiter out; the reply of a cancelled  *)
(* call is discarded and counted in `orphans`), `receive` for              *)
(* `Frame::Cancel` (a queued call is dropped without a reply, a running    *)
(* one still replies), `Frame::Release` and `Frame::Return` (the reader    *)
(* writes back a `throw`/`release`; README "Model"), `Connection::cancel`, *)
(* `Connection::close`, and `Outbound`/`write_frames` (the writer thread   *)
(* #1005: a send never waits for the far end; the channel's `send` blocks  *)
(* on that thread, not on the reader's). README.md next to this file       *)
(* lists the simplifications.                                              *)
(*                                                                         *)
(* A calls; B admits, runs and answers. The session's two directions are   *)
(* FIFO and delayed arbitrarily long, so a reply may be in flight when a   *)
(* cancel is written and the reply of a cancelled call arrives late. With  *)
(* `Bound`, each direction is a channel of `Cap` frames, fed by the writer *)
(* thread from an unbounded queue; a reader's write-back either goes into  *)
(* the channel and blocks it when full (`"direct"`) or into that queue     *)
(* (`"queued"`). Either side may close the session; the other side learns  *)
(* it when the channel ends.                                               *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS
    Calls,       \* the calls A may start ("c1", "c2"), in no fixed order
    MaxCancels,  \* how often A may cancel in the session (1)
    Closes,      \* either side may close the session
    Mutation,    \* "none", or a deliberate bug (README, "Mutations")
    Bound,       \* FALSE: no channel bound (the model without #626)
    Cap,         \* frames a direction holds when Bound
    ReaderWrites,\* "direct" (before #1005) or "queued" (today)
    Rejects,     \* B may refuse a call it read (a `throw` written by the
                 \* reader thread: bad arguments, too deep)
    Replies,     \* after a cancel: "always" or "never" (SESS-61)
    Sync,        \* the calls A makes synchronously (a subset of Calls)
    Abandon      \* a locally abandoned synchronous call: "none", "keep",
                 \* "drop"

ASSUME Calls /= {}
ASSUME MaxCancels \in Nat
ASSUME Closes \in BOOLEAN
ASSUME Mutation \in {"none", "cancelCloses", "lateReplyTwice", "closeLeaks",
                     "reuseId", "abandonLeaks", "lateReplyKeepsGrant"}
ASSUME Bound \in BOOLEAN
ASSUME Cap \in Nat
ASSUME ReaderWrites \in {"direct", "queued"}
ASSUME Rejects \in BOOLEAN
ASSUME Replies \in {"always", "never"}
ASSUME Sync \subseteq Calls
ASSUME Abandon \in {"none", "keep", "drop"}

VARIABLES
    aclosed,     \* A's session is over
    bclosed,     \* B's session is over
    closeCause,  \* ghost: what closed the session: "none" | "a" | "b" |
                 \*   "cancel" | "unknownCall"
    acall,       \* per call at A: "idle" | "waiting" | "settled" |
                 \*   "cancelled" | "abandoned"
    anext,       \* the number A gives its next call
    aid,         \* per call: the number A gave it (0 before it started)
    aTable,      \* per call: what A's table holds for a reply: "none" |
                 \*   "waiting" | "cancelled" | "abandoned" (a call A no
                 \*   longer places makes a reply an unknown call)
    aedge,       \* ghost: per sync call, the wait-graph hop A recorded
    bcall,       \* per call at B: "none" | "queued" | "running" | "done" |
                 \*   "dropped"
    bseq,        \* per call: the number B read with the call
    bmark,       \* ghost: per sync call, B's mark of a call it admitted
    bcancelled,  \* per call: a cancel reached B while it was running it
    cancels,     \* how often A cancelled
    toA,         \* B -> A channel, FIFO: <<"return"|"throw", call, number>>
    toB,         \* A -> B channel, FIFO: <<"invoke", call, number>>,
                 \*   <<"cancel", call>>, <<"release", count>>
    aout,        \* A's writer queue (A -> B), unbounded
    bout,        \* B's writer queue (B -> A), unbounded
    ablock,      \* the frame A's reader is blocked writing (<<>> when none)
    bblock,      \* the frame B's reader is blocked writing
    aheld,       \* reference grants A holds
    bgrants,     \* reference grants B made and nobody released yet
    granted,     \* ghost: how often B granted the reference
    aReleased,   \* ghost: grant units A gave back (a release written, or a
                 \* discarded reply's value released at once)
    aCleared,    \* ghost: grant units A's close dropped
    settles,     \* per call: how often it was settled at A
    outcome,     \* per call: "none" | "ok" | "error" | "cancel" |
                 \*   "abandon" | "closed"
    settledBy,   \* per call: the call whose reply settled it ("none" before)
    orphaned     \* per call: replies discarded after the call was cancelled
                 \*   or abandoned

vars == <<aclosed, bclosed, closeCause, acall, anext, aid, aTable, aedge,
          bcall, bseq, bmark, bcancelled, cancels, toA, toB, aout, bout,
          ablock, bblock, aheld, bgrants, granted, aReleased, aCleared,
          settles, outcome, settledBy, orphaned>>

Init ==
    /\ aclosed = FALSE
    /\ bclosed = FALSE
    /\ closeCause = "none"
    /\ acall = [c \in Calls |-> "idle"]
    /\ anext = 1
    /\ aid = [c \in Calls |-> 0]
    /\ aTable = [c \in Calls |-> "none"]
    /\ aedge = [c \in Calls |-> FALSE]
    /\ bcall = [c \in Calls |-> "none"]
    /\ bseq = [c \in Calls |-> 0]
    /\ bmark = [c \in Calls |-> FALSE]
    /\ bcancelled = [c \in Calls |-> FALSE]
    /\ cancels = 0
    /\ toA = <<>>
    /\ toB = <<>>
    /\ aout = <<>>
    /\ bout = <<>>
    /\ ablock = <<>>
    /\ bblock = <<>>
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
(* The two directions. Unbounded (`~Bound`), a frame goes on the channel   *)
(* as it is made. Bounded, the frame goes into the writer's queue first    *)
(* (the writer thread blocks in the channel's `send`, not the thread that  *)
(* made the frame, #1005). A reader's write-back is the one that can       *)
(* block: before #1005 it went on the channel from the reader thread;      *)
(* today it goes into the writer queue too.                                *)
(***************************************************************************)

SendToB(f) ==
    IF Bound
    THEN aout' = Append(aout, f) /\ UNCHANGED toB
    ELSE toB' = Append(toB, f) /\ UNCHANGED aout

SendToA(f) ==
    IF Bound
    THEN bout' = Append(bout, f) /\ UNCHANGED toA
    ELSE toA' = Append(toA, f) /\ UNCHANGED bout

\* A's reader writes to B: straight on the channel and blocked when it is
\* full (before #1005), or handed to the writer thread (today).
ReaderToB(f) ==
    IF ~Bound \/ ReaderWrites = "queued"
    THEN SendToB(f) /\ UNCHANGED ablock
    ELSE IF Len(toB) < Cap
         THEN toB' = Append(toB, f) /\ UNCHANGED aout /\ UNCHANGED ablock
         ELSE ablock' = f /\ UNCHANGED toB /\ UNCHANGED aout

ReaderToA(f) ==
    IF ~Bound \/ ReaderWrites = "queued"
    THEN SendToA(f) /\ UNCHANGED bblock
    ELSE IF Len(toA) < Cap
         THEN toA' = Append(toA, f) /\ UNCHANGED bout /\ UNCHANGED bblock
         ELSE bblock' = f /\ UNCHANGED toA /\ UNCHANGED bout

(***************************************************************************)
(* A: starting calls, reading replies, abandoning, cancelling, releasing,  *)
(* closing.                                                                *)
(***************************************************************************)

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
    /\ aTable' = [c \in Calls |-> "none"]
    /\ aedge' = [c \in Calls |-> FALSE]
    /\ aheld' = 0
    /\ aCleared' = aCleared + aheld
    /\ toA' = <<>>
    /\ toB' = <<>>
    /\ aout' = <<>>
    /\ bout' = <<>>
    /\ ablock' = <<>>
    /\ bblock' = <<>>
    /\ UNCHANGED <<bclosed, anext, aid, bcall, bseq, bmark, bcancelled,
                   bgrants, granted, aReleased, settledBy, orphaned>>

AClose ==
    /\ Closes
    /\ ~aclosed
    /\ CloseA("a")
    /\ UNCHANGED <<cancels>>


\* A call takes the next number. The mutation keeps the counter, so a new
\* call can take a number an older call already has.
AStart(c) ==
    /\ ~aclosed
    /\ ~bclosed
    /\ acall[c] = "idle"
    /\ acall' = [acall EXCEPT ![c] = "waiting"]
    /\ aid' = [aid EXCEPT ![c] = anext]
    /\ aTable' = [aTable EXCEPT ![c] = "waiting"]
    /\ aedge' = [aedge EXCEPT ![c] = (c \in Sync)]
    /\ SendToB(<<"invoke", c, anext>>)
    /\ anext' = IF Mutation = "reuseId" THEN anext ELSE anext + 1
    /\ UNCHANGED <<aclosed, bclosed, closeCause, bcall, bseq, bmark,
                   bcancelled, cancels, toA, bout, ablock, bblock, aheld,
                   bgrants, granted, aReleased, aCleared, settles, outcome,
                   settledBy, orphaned>>

\* A reply for a call A still waits for: the call whose number the reply
\* carries is settled once, with the reference a `return` also carries. A
\* `throw` settles it with an error and grants nothing.
ADeliverLive(d) ==
    /\ ~aclosed
    /\ ablock = <<>>
    /\ toA /= <<>>
    /\ LET f == Head(toA)
       IN /\ f[1] \in {"return", "throw"}
          /\ aid[d] = f[3]
          /\ acall[d] = "waiting"
          /\ toA' = Tail(toA)
          /\ acall' = [acall EXCEPT ![d] = "settled"]
          /\ aTable' = [aTable EXCEPT ![d] = "none"]
          /\ aedge' = [aedge EXCEPT ![d] = FALSE]
          /\ settles' = [settles EXCEPT ![d] = @ + 1]
          /\ outcome' = [outcome EXCEPT ![d] =
                            IF f[1] = "return" THEN "ok" ELSE "error"]
          /\ settledBy' = [settledBy EXCEPT ![d] = f[2]]
          /\ aheld' = IF f[1] = "return" THEN aheld + 1 ELSE aheld
    /\ UNCHANGED <<aclosed, bclosed, closeCause, anext, aid, bcall, bseq,
                   bmark, bcancelled, cancels, toB, aout, bout, ablock,
                   bgrants, granted, aReleased, aCleared, orphaned, bblock>>

\* A reply for a call A cancelled or abandoned while keeping its entry: it
\* is decoded (so the reference a `return` carries is released at once) and
\* discarded. The session goes on.
\* The mutation applies it as if the call were still waiting: the call is
\* settled a second time and its value kept.
ADeliverLate(d) ==
    /\ ~aclosed
    /\ ablock = <<>>
    /\ toA /= <<>>
    /\ LET f == Head(toA)
           bug == Mutation = "lateReplyTwice"
       IN /\ f[1] \in {"return", "throw"}
          /\ aid[d] = f[3]
          /\ acall[d] \in {"cancelled", "abandoned"}
          /\ aTable[d] # "none"
          /\ toA' = Tail(toA)
          /\ orphaned' = [orphaned EXCEPT ![d] = @ + 1]
          /\ acall' = IF bug THEN [acall EXCEPT ![d] = "settled"] ELSE acall
          /\ aTable' = IF bug THEN aTable
                       ELSE [aTable EXCEPT ![d] = "none"]
          /\ aedge' = IF Mutation = "abandonLeaks" /\ acall[d] = "abandoned"
                      THEN aedge ELSE [aedge EXCEPT ![d] = FALSE]
          /\ settles' = IF bug THEN [settles EXCEPT ![d] = @ + 1] ELSE settles
          /\ outcome' = IF bug THEN [outcome EXCEPT ![d] = "ok"] ELSE outcome
          /\ settledBy' = IF bug
                          THEN [settledBy EXCEPT ![d] = f[2]] ELSE settledBy
          /\ aheld' = IF bug /\ f[1] = "return" THEN aheld + 1 ELSE aheld
          /\ aReleased' = IF ~bug /\ f[1] = "return"
                              /\ Mutation # "lateReplyKeepsGrant"
                          THEN aReleased + 1 ELSE aReleased
          /\ IF bug \/ f[1] = "throw" \/ Mutation = "lateReplyKeepsGrant"
             THEN UNCHANGED <<toB, aout, ablock>>
             ELSE ReaderToB(<<"release", 1>>)
    /\ UNCHANGED <<aclosed, bclosed, closeCause, anext, aid, bcall, bseq,
                   bmark, bcancelled, cancels, bout, bblock, bgrants,
                   granted, aCleared>>

\* A reply for a call A can no longer place: the wait table has neither a
\* waiter nor an entry for it, so it is the protocol error `response for
\* unknown call` and the session ends (SESS-41).
ADeliverStray ==
    /\ ~aclosed
    /\ ablock = <<>>
    /\ toA /= <<>>
    /\ LET f == Head(toA)
       IN /\ f[1] \in {"return", "throw"}
          /\ \E d \in Calls : aid[d] = f[3] /\ acall[d] # "waiting"
                              /\ aTable[d] = "none"
    /\ CloseA("unknownCall")
    /\ UNCHANGED <<cancels>>

\* A gives up on a call: the caller gets its own error, and a reply that was
\* already on its way is discarded. The cancel frame goes out after the
\* invoke, in this direction's order, and does not end the session.
\* The mutation ends A's session right here.
ACancel(c) ==
    /\ ~aclosed
    /\ ~bclosed
    /\ acall[c] = "waiting"
    /\ cancels < MaxCancels
    /\ IF Mutation = "cancelCloses"
       THEN CloseA("cancel") /\ UNCHANGED <<cancels>>
       ELSE /\ cancels' = cancels + 1
            /\ acall' = [acall EXCEPT ![c] = "cancelled"]
            /\ aTable' = [aTable EXCEPT ![c] = "cancelled"]
            /\ aedge' = [aedge EXCEPT ![c] = FALSE]
            /\ settles' = [settles EXCEPT ![c] = @ + 1]
            /\ outcome' = [outcome EXCEPT ![c] = "cancel"]
            /\ SendToB(<<"cancel", c>>)
            /\ UNCHANGED <<aclosed, bclosed, closeCause, anext, aid, bcall,
                           bseq, bmark, bcancelled, toA, bout, ablock,
                           bblock, aheld, bgrants, granted, aReleased,
                           aCleared, settledBy, orphaned>>

\* A synchronous call is given up locally, without a cancel frame: the
\* caller's own wait ends, the callee does not learn it. `keep` leaves the
\* waiter and the wait-graph edge in A's table, so the reply is absorbed as
\* a late reply and cleans both; `drop` forgets them, so the reply is an
\* unknown call (ADeliverStray). This is the shape of a sync wait, which
\* has no drop guard (an async call's future sends `cancel` when dropped).
AAbandon(c) ==
    /\ Abandon # "none"
    /\ ~aclosed
    /\ ~bclosed
    /\ c \in Sync
    /\ acall[c] = "waiting"
    /\ acall' = [acall EXCEPT ![c] = "abandoned"]
    /\ aTable' = [aTable EXCEPT ![c] =
                     IF Abandon = "keep" THEN "abandoned" ELSE "none"]
    /\ aedge' = IF Abandon = "drop"
                THEN [aedge EXCEPT ![c] = FALSE] ELSE aedge
    /\ settles' = [settles EXCEPT ![c] = @ + 1]
    /\ outcome' = [outcome EXCEPT ![c] = "abandon"]
    /\ UNCHANGED <<aclosed, bclosed, closeCause, anext, aid, bcall, bseq,
                   bmark, bcancelled, cancels, toA, toB, aout, bout,
                   ablock, bblock, aheld, bgrants, granted, aReleased,
                   aCleared, settledBy, orphaned>>

\* A drops what it holds and releases it: the count is what A holds now.
ARelease(n) ==
    /\ ~aclosed
    /\ ~bclosed
    /\ n \in 1..aheld
    /\ aheld' = aheld - n
    /\ aReleased' = aReleased + n
    /\ SendToB(<<"release", n>>)
    /\ UNCHANGED <<aclosed, bclosed, closeCause, acall, anext, aid, aTable,
                   aedge, bcall, bseq, bmark, bcancelled, cancels, toA,
                   bout, ablock, bblock, bgrants, granted, aCleared, settles,
                   outcome, settledBy, orphaned>>

\* B's close reached A: the channel ended, so A sees it.
ALearn ==
    /\ bclosed
    /\ ~aclosed
    /\ CloseA("b")
    /\ UNCHANGED <<cancels>>

\* A reader blocked on a full channel writes its frame once the other side
\* read one (before #1005, and only then).
AUnblock ==
    /\ ablock /= <<>>
    /\ Len(toB) < Cap
    /\ toB' = Append(toB, ablock)
    /\ ablock' = <<>>
    /\ UNCHANGED <<aclosed, bclosed, closeCause, acall, anext, aid, aTable,
                   aedge, bcall, bseq, bmark, bcancelled, cancels, toA, aout,
                   bout, bblock, aheld, bgrants, granted, aReleased, aCleared,
                   settles, outcome, settledBy, orphaned>>

BUnblock ==
    /\ bblock /= <<>>
    /\ Len(toA) < Cap
    /\ toA' = Append(toA, bblock)
    /\ bblock' = <<>>
    /\ UNCHANGED <<aclosed, bclosed, closeCause, acall, anext, aid, aTable,
                   aedge, bcall, bseq, bmark, bcancelled, cancels, toB, aout,
                   bout, ablock, aheld, bgrants, granted, aReleased, aCleared,
                   settles, outcome, settledBy, orphaned>>

(***************************************************************************)
(* The writer threads: a frame waits in the queue until the channel takes  *)
(* it. The far end reads one frame at a time.                              *)
(***************************************************************************)

FlushB ==
    /\ Bound
    /\ aout /= <<>>
    /\ Len(toB) < Cap
    /\ toB' = Append(toB, Head(aout))
    /\ aout' = Tail(aout)
    /\ UNCHANGED <<aclosed, bclosed, closeCause, acall, anext, aid, aTable,
                   aedge, bcall, bseq, bmark, bcancelled, cancels, toA,
                   bout, ablock, bblock, aheld, bgrants, granted, aReleased,
                   aCleared, settles, outcome, settledBy, orphaned>>

FlushA ==
    /\ Bound
    /\ bout /= <<>>
    /\ Len(toA) < Cap
    /\ toA' = Append(toA, Head(bout))
    /\ bout' = Tail(bout)
    /\ UNCHANGED <<aclosed, bclosed, closeCause, acall, anext, aid, aTable,
                   aedge, bcall, bseq, bmark, bcancelled, cancels, toB, aout,
                   ablock, bblock, aheld, bgrants, granted, aReleased,
                   aCleared, settles, outcome, settledBy, orphaned>>

(***************************************************************************)
(* B: admitting calls, running them, replying, and reading cancels and     *)
(* releases.                                                               *)
(***************************************************************************)

\* B takes the next call. A call number only grows, so a number an older
\* call already took is not a new call. A synchronous call is marked for the
\* wait graph.
BReadInvoke(c) ==
    /\ ~aclosed
    /\ ~bclosed
    /\ bblock = <<>>
    /\ toB /= <<>>
    /\ Head(toB)[1] = "invoke"
    /\ Head(toB)[2] = c
    /\ bcall[c] = "none"
    /\ bcall' = [bcall EXCEPT ![c] = "queued"]
    /\ bseq' = [bseq EXCEPT ![c] = Head(toB)[3]]
    /\ bmark' = [bmark EXCEPT ![c] = (c \in Sync)]
    /\ toB' = Tail(toB)
    /\ UNCHANGED <<aclosed, bclosed, closeCause, acall, anext, aid, aTable,
                   aedge, bcancelled, cancels, toA, aout, bout, ablock,
                   bblock, aheld, bgrants, granted, aReleased, aCleared,
                   settles, outcome, settledBy, orphaned>>

\* B refuses the call as it reads it: the reader thread writes a `throw`
\* back (a `decode_args`/`too_deep` failure). A `throw` is a reply without a
\* grant; it goes through the same write-back as a discarded reply's
\* release.
BRejectInvoke(c) ==
    /\ Rejects
    /\ ~aclosed
    /\ ~bclosed
    /\ bblock = <<>>
    /\ toB /= <<>>
    /\ Head(toB)[1] = "invoke"
    /\ Head(toB)[2] = c
    /\ bcall[c] = "none"
    /\ bcall' = [bcall EXCEPT ![c] = "dropped"]
    /\ bseq' = [bseq EXCEPT ![c] = Head(toB)[3]]
    /\ toB' = Tail(toB)
    /\ ReaderToA(<<"throw", c, Head(toB)[3]>>)
    /\ UNCHANGED <<aclosed, bclosed, closeCause, acall, anext, aid, aTable,
                   aedge, bmark, bcancelled, cancels, aout, ablock, aheld,
                   bgrants, granted, aReleased, aCleared, settles, outcome,
                   settledBy, orphaned>>

\* The method starts. How long it takes is not modelled: the reply may be
\* written at any later point, also after a cancel arrived.
BRun(c) ==
    /\ bcall[c] = "queued"
    /\ bcall' = [bcall EXCEPT ![c] = "running"]
    /\ UNCHANGED <<aclosed, bclosed, closeCause, acall, anext, aid, aTable,
                   aedge, bseq, bmark, bcancelled, cancels, toA, toB, aout,
                   bout, ablock, bblock, aheld, bgrants, granted, aReleased,
                   aCleared, settles, outcome, settledBy, orphaned>>

\* The method answers: the call is done at B, and the reply (with the one
\* reference it carries) goes out. A running call answers also after a
\* cancel arrived when the rule says so (`Replies = "always"`); a queued one
\* is dropped instead (BReadCancel), and one cancelled while running is
\* dropped without an answer when the rule says `"never"` (BDrop).
BReply(c) ==
    /\ bcall[c] = "running"
    /\ Replies = "always" \/ ~bcancelled[c]
    /\ bcall' = [bcall EXCEPT ![c] = "done"]
    /\ bmark' = [bmark EXCEPT ![c] = FALSE]
    /\ SendToA(<<"return", c, bseq[c]>>)
    /\ bgrants' = bgrants + 1
    /\ granted' = granted + 1
    /\ UNCHANGED <<aclosed, bclosed, closeCause, acall, anext, aid, aTable,
                   aedge, bseq, bcancelled, cancels, toB, aout, ablock,
                   bblock, aheld, aReleased, aCleared, settles, outcome,
                   settledBy, orphaned>>

\* The method finished but the rule is not to answer a call cancelled while
\* it ran: the call is dropped without a frame.
BDrop(c) ==
    /\ Replies = "never"
    /\ bcall[c] = "running"
    /\ bcancelled[c]
    /\ bcall' = [bcall EXCEPT ![c] = "dropped"]
    /\ bmark' = [bmark EXCEPT ![c] = FALSE]
    /\ UNCHANGED <<aclosed, bclosed, closeCause, acall, anext, aid, aTable,
                   aedge, bseq, bcancelled, cancels, toA, toB, aout, bout,
                   ablock, bblock, aheld, bgrants, granted, aReleased,
                   aCleared, settles, outcome, settledBy, orphaned>>

\* A cancel arrives. A call still queued never runs and is not answered; one
\* already running keeps running and answers (or is dropped, by the rule);
\* one already answered is not touched (the cancel crossed its reply).
\* Nothing else changes.
BReadCancel(c) ==
    /\ ~aclosed
    /\ ~bclosed
    /\ bblock = <<>>
    /\ toB /= <<>>
    /\ Head(toB) = <<"cancel", c>>
    /\ toB' = Tail(toB)
    /\ bcall' = IF bcall[c] = "queued" THEN [bcall EXCEPT ![c] = "dropped"]
               ELSE bcall
    /\ bmark' = IF bcall[c] = "queued" THEN [bmark EXCEPT ![c] = FALSE]
               ELSE bmark
    /\ bcancelled' = IF Replies = "never" /\ bcall[c] = "running"
                     THEN [bcancelled EXCEPT ![c] = TRUE] ELSE bcancelled
    /\ UNCHANGED <<aclosed, bclosed, closeCause, acall, anext, aid, aTable,
                   aedge, bseq, cancels, toA, aout, bout, ablock, bblock,
                   aheld, bgrants, granted, aReleased, aCleared, settles,
                   outcome, settledBy, orphaned>>

\* A release arrives: the grants it names are given up. The export goes away
\* once nothing is granted any more.
BReadRelease(n) ==
    /\ ~aclosed
    /\ ~bclosed
    /\ bblock = <<>>
    /\ toB /= <<>>
    /\ Head(toB) = <<"release", n>>
    /\ n \in 1..bgrants
    /\ toB' = Tail(toB)
    /\ bgrants' = bgrants - n
    /\ UNCHANGED <<aclosed, bclosed, closeCause, acall, anext, aid, aTable,
                   aedge, bcall, bseq, bmark, bcancelled, cancels, toA, aout,
                   bout, ablock, bblock, aheld, granted, aReleased, aCleared,
                   settles, outcome, settledBy, orphaned>>

\* B closes its side: the calls it has not answered are dropped, its exports
\* go, and the frames in both directions are lost.
\* The mutation keeps the export table.
CloseB(cause) ==
    /\ bclosed' = TRUE
    /\ closeCause' = IF closeCause = "none" THEN cause ELSE closeCause
    /\ bcall' = [c \in Calls |-> IF bcall[c] \in {"queued", "running"}
                                   THEN "dropped" ELSE bcall[c]]
    /\ bmark' = [c \in Calls |-> FALSE]
    /\ bgrants' = IF Mutation = "closeLeaks" THEN bgrants ELSE 0
    /\ toA' = <<>>
    /\ toB' = <<>>
    /\ aout' = <<>>
    /\ bout' = <<>>
    /\ ablock' = <<>>
    /\ bblock' = <<>>
    /\ UNCHANGED <<aclosed, acall, anext, aid, aTable, aedge, bseq,
                   bcancelled, cancels, aheld, granted, aReleased, aCleared,
                   settles, outcome, settledBy, orphaned>>

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

\* Nothing is left to do: every call is settled on both sides and no frame
\* waits anywhere. Only then may the model stop, so that TLC's deadlock
\* check tells "the work is done" from "both readers are blocked".
AllSettled ==
    /\ \A c \in Calls : acall[c] \in {"idle", "settled", "cancelled",
                                      "abandoned"}
    /\ \A c \in Calls : bcall[c] \in {"none", "done", "dropped"}
    /\ toA = <<>>
    /\ toB = <<>>
    /\ aout = <<>>
    /\ bout = <<>>
    /\ ablock = <<>>
    /\ bblock = <<>>

Done ==
    /\ AllSettled
    /\ UNCHANGED vars

Next ==
    \/ \E c \in Calls : AStart(c) \/ ACancel(c) \/ AAbandon(c)
    \/ \E c \in Calls : ADeliverLive(c) \/ ADeliverLate(c)
    \/ ADeliverStray
    \/ \E n \in 1..aheld : ARelease(n)
    \/ AClose
    \/ ALearn
    \/ AUnblock
    \/ \E c \in Calls : BReadInvoke(c) \/ BRejectInvoke(c) \/ BRun(c)
         \/ BReply(c) \/ BDrop(c) \/ BReadCancel(c)
    \/ \E n \in 1..bgrants : BReadRelease(n)
    \/ BClose
    \/ BLearn
    \/ BUnblock
    \/ FlushA
    \/ FlushB
    \/ Done

Spec == Init /\ [][Next]_vars /\ WF_vars(Next)

(***************************************************************************)
(* Properties.                                                             *)
(***************************************************************************)

\* A cancel never ends the session: the close that ends it is the caller's,
\* the callee's, or the one the channel end brings.
CancelKeepsSession == closeCause # "cancel"

\* A call is settled at most once, whether by its reply, by a cancel, by a
\* local abandonment or by the session ending.
SettlesAtMostOnce == \A c \in Calls : settles[c] <= 1

\* A late reply is discarded: the cancelled (or abandoned) call keeps the
\* outcome it got, and the reply does not settle it a second time. Each call
\* has at most one reply to discard.
LateReplyDropped ==
    \A c \in Calls :
        orphaned[c] > 0 => (outcome[c] \in {"cancel", "abandon"}
                            /\ settles[c] = 1)
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
                                                   "cancelled", "abandoned"})
    /\ (bclosed => \A c \in Calls : bcall[c] \in {"none", "done", "dropped"})

\* Liveness, with no close to fall back on: a call A waits for is answered
\* or cancelled, so the callee answers what it took.
WaitingSettles ==
    \A c \in Calls : (acall[c] = "waiting") ~> (acall[c] \in {"settled",
                                                              "cancelled",
                                                              "abandoned"})

\* A locally abandoned call does not end the session: its reply is absorbed
\* as a late reply, not taken for a reply to an unknown call (SESS-41). A
\* caller that forgets the call must send `cancel` instead.
AbandonKeepsSession == closeCause # "unknownCall"

\* Nothing waits anywhere: no frame on a channel or in a writer queue, no
\* blocked reader, no call in flight at either side.
Quiescent ==
    /\ ~aclosed
    /\ ~bclosed
    /\ toA = <<>>
    /\ toB = <<>>
    /\ aout = <<>>
    /\ bout = <<>>
    /\ ablock = <<>>
    /\ bblock = <<>>
    /\ \A c \in Calls : acall[c] # "waiting"
    /\ \A c \in Calls : bcall[c] \notin {"queued", "running"}

\* The caller's table of cancelled calls drains: no entry waits for a reply
\* to clear it. A callee that never answers a cancelled call leaves one
\* behind (the #626 gap: `call-gap-cancel-table`).
CancelledEntriesDrain ==
    Quiescent => \A c \in Calls : aTable[c] # "cancelled"

\* No grant is lost: at rest every unit B granted is held by A or given
\* back. A discarded late reply that did not release its reference would
\* leave a unit unaccounted (the reference-conservation rule of trace
\* contract §4).
GrantsConserved ==
    Quiescent => aheld + aReleased + aCleared = granted

\* A locally abandoned call leaves no wait-graph edge once the session is
\* quiet: the reply the callee still sends cleans it.
AbandonedCallsClean ==
    Quiescent => \A c \in Calls : ~aedge[c]

=============================================================================
