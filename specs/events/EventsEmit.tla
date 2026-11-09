--------------------------------- MODULE EventsEmit -----------------------------------
(**************************************************************************************)
(* `emit` order at one runtime and one host: a call held during a synchronous wait, *)
(* the reply frame read in place, a later emit of the same name, a filter that may  *)
(* answer late or never, and a row that unloads while an emit waits.                *)
(*                                                                                    *)
(* Design: docs/design-events-protocol-2026-10-18.md §5.2 (fast path), §5.4 (`emit`  *)
(* starts every call after the one before started; the reply after that), §5.5 (the *)
(* order of emits: the host queues a session's emits, the runtime queues its own    *)
(* while a filter answers, a held call is passed by no later emit, the reply is     *)
(* read in place, after the reply the name is off the via-host path), §6 (a filter  *)
(* can answer late), §7.1 (a row's queued and filtering emits go with it).           *)
(* Implementation: crates/rutis/src/bus.rs (`emit_erased`: the per-name tail and    *)
(* the backlog), crates/rutis-bridge/src/events/hub.rs (`SessionEvents::dispatch`,   *)
(* `run_emit`, `next_emit`, `let_go_of_its_emits`, `cancelling`), and the runtimes' *)
(* emit queues (node/rutis-runtime/src/events.mjs, python/rutis/rutis/runner.py,    *)
(* go/rutis/internal/runtime/events.go).                                            *)
(*                                                                                    *)
(* The scenario (README.md, "Model"): R1 emits E1 through the host while its        *)
(* listener L1 is inside a synchronous wait (a service call), so E1's call at L1 is *)
(* held; R1 reads E1's reply in place; R1's wait chain then emits E2, which must    *)
(* start after the held call. E1's filter may be absent, late, or never answered;   *)
(* the emitting row may unload meanwhile. L2 is R2's listener: it is why E1 goes    *)
(* through the host, and it starts E1's call directly.                             *)
(**************************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    L1,           \* R1's listener on the emitting row
    L2,           \* R2's listener (outside R1: E1 goes through the host)
    Filter1,      \* E1's filter: "none" | "late" | "never"
    R2Rule,       \* "a" (starts a call when the frame is read) | "b" (one at a time)
    Mutation      \* "none" | "replyAsync" | "heldEmit" | "unloadQueue"

ASSUME L1 # L2
ASSUME Filter1 \in {"none", "late", "never"}
ASSUME R2Rule \in {"a", "b"}
ASSUME Mutation \in {"none", "replyAsync", "heldEmit", "unloadQueue"}

Listeners == {L1, L2}

Emits == {1, 2}

E1 == 1

E2 == 2

-------------------------------------------------------------------------------
(* Helpers                                                                    *)
-------------------------------------------------------------------------------

Elements(seq) == { seq[i] : i \in 1..Len(seq) }

(* The listeners of `e`'s calls in `seq`, in order.                           *)
(* The calls of `seq` as start order: <<emit, listener>>.                     *)
RECURSIVE StartsOf(_)
StartsOf(seq) ==
    IF seq = <<>> THEN <<>>
    ELSE <<[emit |-> Head(seq)[1], listener |-> Head(seq)[2]]>>
         \o StartsOf(Tail(seq))

RECURSIVE CallsOf(_, _)
CallsOf(seq, e) ==
    IF seq = <<>> THEN <<>>
    ELSE IF Head(seq)[1] = e
         THEN <<Head(seq)[2]>> \o CallsOf(Tail(seq), e)
         ELSE CallsOf(Tail(seq), e)

Before(seq, e, f) ==
    \E i, j \in 1..Len(seq): seq[i] = e /\ seq[j] = f /\ i < j

-------------------------------------------------------------------------------
(* State                                                                      *)
-------------------------------------------------------------------------------

VARIABLES
    phase,     \* emit -> "none" | "queued" | "filtering" | "starting" | "replied" | "dropped"
    path,      \* emit -> "none" | "host" | "fast"
    kept,      \* emit -> the listeners its filter keeps
    left,      \* emit -> the listeners whose call has not started yet, in order
    read,      \* emit -> R1 has read its reply frame
    marked,    \* emit -> R1 recorded the reply (only replyAsync separates the two)
    wait,      \* R1 is in a synchronous wait (a service call, not this emit)
    held,      \* calls held at R1, in arrival order: <<emit, listener>>
    started,   \* emit -> the listeners whose call started, in order (ghost)
    starts,    \* all calls in the order they started (ghost)
    rowDown,   \* the emitting row has unloaded
    atUnload,  \* emit -> its started calls when the row unloaded (ghost)
    afterRead  \* emit -> it was issued after E1's reply was read (ghost)

vars == <<phase, path, kept, left, read, marked, wait, held, started, starts,
          rowDown, atUnload, afterRead>>

-------------------------------------------------------------------------------
(* What R1 does                                                              *)
-------------------------------------------------------------------------------

(* E1 is emitted by the emitting row, through the host (L2 is outside R1).    *)
Emit1 ==
    /\ phase[E1] = "none"
    /\ ~rowDown
    /\ path' = [path EXCEPT ![E1] = "host"]
    /\ phase' = [phase EXCEPT ![E1] = "queued"]
    /\ kept' = [kept EXCEPT ![E1] = {}]
    /\ left' = [left EXCEPT ![E1] = <<>>]
    /\ UNCHANGED <<read, marked, wait, held, started, starts, rowDown, atUnload,
                   afterRead>>

(* R1 waits for a service call (not for this emit).                          *)
BeginWait ==
    /\ ~wait
    /\ wait' = TRUE
    /\ UNCHANGED <<phase, path, kept, left, read, marked, held, started, starts,
                   rowDown, atUnload, afterRead>>

(* §5.5: the reply frame is read in place and the emit recorded as replied;    *)
(* `replyAsync` records it one step later (#513, c2c1b7e8).                  *)
ReadReply ==
    /\ phase[E1] = "replied"
    /\ ~read[E1]
    /\ read' = [read EXCEPT ![E1] = TRUE]
    /\ marked' = [marked EXCEPT ![E1] = Mutation # "replyAsync"]
    /\ UNCHANGED <<phase, path, kept, left, wait, held, started, starts,
                   rowDown, atUnload, afterRead>>

MarkReply ==
    /\ Mutation = "replyAsync"
    /\ read[E1]
    /\ ~marked[E1]
    /\ marked' = [marked EXCEPT ![E1] = TRUE]
    /\ UNCHANGED <<phase, path, kept, left, read, wait, held, started, starts,
                   rowDown, atUnload, afterRead>>

(* R1's wait chain emits E2. Once E1's reply is read, the name is no longer on *)
(* the via-host path (§5.5); `replyAsync` still counts it as unreplied, so E2   *)
(* goes to the host again.                                                   *)
Emit2 ==
    /\ phase[E1] # "none"
    /\ phase[E2] = "none"
    /\ ~rowDown
    /\ LET fast == read[E1] /\ ~(Mutation = "replyAsync" /\ ~marked[E1])
       IN /\ path' = [path EXCEPT ![E2] = IF fast THEN "fast" ELSE "host"]
          /\ phase' = [phase EXCEPT ![E2] = IF fast THEN "starting" ELSE "queued"]
          /\ afterRead' = [afterRead EXCEPT ![E2] = read[E1]]
          /\ kept' = [kept EXCEPT ![E2] = Listeners]
          /\ left' = [left EXCEPT ![E2] =
                        IF fast THEN <<L1>> ELSE <<L1, L2>>]
          /\ UNCHANGED <<read, marked, wait, held, started, starts, rowDown, atUnload>>

(* §5.5: the wait ends. Held calls start in arrival order; the fixed code put  *)
(* a later fast-path call behind them rather than ahead.                     *)
EndWait ==
    /\ wait
    /\ wait' = FALSE
    /\ held' = <<>>
    /\ started' = [e \in Emits |-> started[e] \o CallsOf(held, e)]
    /\ starts' = starts \o StartsOf(held)
    /\ UNCHANGED <<phase, path, kept, left, read, marked, rowDown, atUnload,
                   afterRead>>

(* §7.1 / §5.5: the row unloads. Its queued emit is dropped and the one        *)
(* waiting for its filter is let go (an answer then starts nothing); the calls *)
(* already held are dropped too. `unloadQueue` keeps all of them (#536).      *)
UnloadRow ==
    /\ ~rowDown
    /\ rowDown' = TRUE
    /\ atUnload' = started
    /\ IF Mutation = "unloadQueue"
       THEN UNCHANGED <<phase, left, held, path, kept, read, marked, wait,
                         started, starts, afterRead>>
       ELSE /\ phase' = [e \in Emits |->
                           IF phase[e] \in {"queued", "filtering", "starting"}
                           THEN "dropped" ELSE phase[e]]
            /\ left' = [e \in Emits |-> <<>>]
            /\ held' = <<>>
            /\ UNCHANGED <<path, kept, read, marked, wait, started, starts,
                            afterRead>>

-------------------------------------------------------------------------------
(* What the host does                                                        *)
-------------------------------------------------------------------------------

(* The host takes the session's emit (§5.5: one at a time, in order).         *)
HostTake ==
    /\ phase[E1] = "queued"
    /\ phase' = [phase EXCEPT ![E1] =
                   IF Filter1 = "none" THEN "starting" ELSE "filtering"]
    /\ kept' = [kept EXCEPT ![E1] = IF Filter1 = "none" THEN Listeners ELSE {}]
    /\ left' = [left EXCEPT ![E1] = <<L1, L2>>]
    /\ UNCHANGED <<path, read, marked, wait, held, started, starts, rowDown, atUnload,
                   afterRead>>

(* §6: the selector answers, keeping a set of listeners. `never` never        *)
(* answers; a row that let the emit go makes the answer start nothing.        *)
FilterAnswer ==
    /\ phase[E1] = "filtering"
    /\ Filter1 = "late"
    /\ \E keep \in SUBSET Listeners:
         /\ kept' = [kept EXCEPT ![E1] = keep]
         /\ phase' = [phase EXCEPT ![E1] = "starting"]
         /\ UNCHANGED <<path, left, read, marked, wait, held, started, starts, rowDown,
                        atUnload, afterRead>>

(* One call of E1 starts, in registration order. At L1, while R1 waits, the    *)
(* call is held; L2's call starts at once.                                   *)
StartCallE1 ==
    /\ phase[E1] = "starting"
    /\ left[E1] /= <<>>
    /\ LET l == Head(left[E1])
           keep == l \in kept[E1]
           hold == l = L1 /\ wait
       IN /\ left' = [left EXCEPT ![E1] = Tail(@)]
          /\ IF keep /\ hold
             THEN held' = Append(held, <<E1, l>>)
             ELSE held' = held
          /\ IF keep /\ ~hold
             THEN started' = [started EXCEPT ![E1] = Append(@, l)]
             ELSE started' = started
          /\ starts' = IF keep /\ ~hold THEN Append(starts, [emit |-> E1, listener |-> l]) ELSE starts
          /\ phase' = [phase EXCEPT ![E1] =
                         IF Tail(left[E1]) = <<>> THEN "replied" ELSE "starting"]
          /\ UNCHANGED <<path, kept, read, marked, wait, rowDown, atUnload,
                         afterRead>>

(* E2 was sent to the host; it starts after E1's calls all started.           *)
HostTakeE2 ==
    /\ phase[E2] = "queued"
    /\ phase[E1] \in {"replied", "dropped"}
    /\ phase' = [phase EXCEPT ![E2] = "starting"]
    /\ left' = [left EXCEPT ![E2] = <<L1, L2>>]
    /\ UNCHANGED <<path, kept, read, marked, wait, held, started, starts,
                   rowDown, atUnload, afterRead>>

(* One call of E2: through the host (L1 and L2) or on the fast path (L1 only). *)
StartCallE2 ==
    /\ phase[E2] = "starting"
    /\ left[E2] /= <<>>
    /\ LET l == Head(left[E2])
           keep == l \in kept[E2]
           hold == l = L1 /\ wait
       IN /\ left' = [left EXCEPT ![E2] = Tail(@)]
          /\ IF keep /\ hold /\ Mutation # "heldEmit"
             THEN held' = Append(held, <<E2, l>>)
             ELSE held' = held
          /\ IF keep /\ ~hold
             THEN started' = [started EXCEPT ![E2] = Append(@, l)]
             ELSE IF keep /\ hold /\ Mutation = "heldEmit"
             THEN started' = [started EXCEPT ![E2] = Append(@, l)]
             ELSE started' = started
          /\ starts' = IF keep /\ (~hold \/ (hold /\ Mutation = "heldEmit"))
                        THEN Append(starts, [emit |-> E2, listener |-> l])
                        ELSE starts
          /\ phase' = [phase EXCEPT ![E2] =
                         IF Tail(left[E2]) = <<>> THEN "replied" ELSE "starting"]
          /\ UNCHANGED <<path, kept, read, marked, wait, rowDown, atUnload,
                         afterRead>>

Next ==
    \/ Emit1
    \/ BeginWait
    \/ ReadReply
    \/ MarkReply
    \/ Emit2
    \/ EndWait
    \/ UnloadRow
    \/ HostTake
    \/ FilterAnswer
    \/ StartCallE1
    \/ HostTakeE2
    \/ StartCallE2

Init ==
    /\ phase = [e \in Emits |-> "none"]
    /\ path = [e \in Emits |-> "none"]
    /\ kept = [e \in Emits |-> {}]
    /\ left = [e \in Emits |-> <<>>]
    /\ read = [e \in Emits |-> FALSE]
    /\ marked = [e \in Emits |-> FALSE]
    /\ wait = FALSE
    /\ held = <<>>
    /\ started = [e \in Emits |-> <<>>]
    /\ starts = <<>>
    /\ rowDown = FALSE
    /\ atUnload = [e \in Emits |-> <<>>]
    /\ afterRead = [e \in Emits |-> FALSE]

Spec == Init /\ [][Next]_vars

-------------------------------------------------------------------------------
(* Properties                                                                 *)
-------------------------------------------------------------------------------

(* §5.5: for one emitter and one listener, the calls of its emits start in the  *)
(* order the emits were issued. E1 was issued first, so at no listener may E2's *)
(* call start before E1's.                                                    *)
EmitOrder ==
    \A l \in Listeners:
      \A i, j \in 1..Len(starts):
        (starts[i].emit = E2 /\ starts[i].listener = l
         /\ starts[j].emit = E1 /\ starts[j].listener = l) => ~(i < j)

(* §5.5: once the reply frame was read, a later emit is not left on the        *)
(* via-host path.                                                            *)
FastPathByFrame ==
    afterRead[E2] = FALSE \/ path[E2] = "fast"

(* §5.5 / §7.1: an emit of a row that unloaded starts no call, even after its   *)
(* filter answers.                                                           *)
UnloadedRowEmitsNothing ==
    rowDown = FALSE \/ started = atUnload

=======================================================================================
