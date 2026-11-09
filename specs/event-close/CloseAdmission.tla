---------------------------- MODULE CloseAdmission ----------------------------
(***************************************************************************)
(* Event listener close admission (#1025): a closer closing a fiber subtree *)
(* and dispatchers selecting a listener entry and then checking, once,      *)
(* whether it may still be called.                                          *)
(*                                                                         *)
(* Design: docs/design-event-close-admission-2026-11-08.md §2 (D1-D5) and   *)
(* §3 rules 1-5. Implementation: crates/rutis/src/fiber.rs                  *)
(* `mark_subtree_lineage` / `publish_subtree_closing`, and                   *)
(* crates/rutis/src/bus/table.rs `Entry::live` / `Entry::start`.            *)
(*                                                                         *)
(* A close holds `Shared.admission` for its whole pass: the subtree is      *)
(* fixed, every member gets `lineage_closed` (children before parents),     *)
(* the marker pass completes, then the existing `closing` flags are         *)
(* published (the root also publishes `Shared.closing`). The completion of  *)
(* the marker pass is the linearization point; cleanup is scheduled after   *)
(* the publication. A dispatcher may select an entry at any time; its final *)
(* `Entry::live` reads only the entry's own owner: withdrawn (not modelled: *)
(* entries never withdraw here), owner alive, `closing`,                    *)
(* `lineage_closed`, and the generation (always current here).              *)
(*                                                                         *)
(* The tree is fixed: r -> a -> {b, c}, b -> {d, n}, r -> m; n and m are    *)
(* created by `Spawn` (a creation under the admission lock). One entry per  *)
(* fiber stands for "the entries of that fiber": the model is about where   *)
(* the admission decision reads its markers, not about how many listeners   *)
(* one fiber has. `release_transient` detaches a child under the same lock  *)
(* (fiber.rs); the detach is part of the fixed set and is not a step here.  *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
    Mutation,     \* "none", or a deliberate bug (README, "Mutations")
    CloseRoots,   \* fibers a close pass may be started on
    SpawnAllowed  \* whether new fibers may be created

Fibers == {"r", "a", "b", "c", "d", "n", "m"}
Root == "r"
Parent == [f \in Fibers |->
            CASE f = "r" -> "r"
              [] f = "a" -> "r"
              [] f = "b" -> "a"
              [] f = "c" -> "a"
              [] f = "d" -> "b"
              [] f = "n" -> "b"
              [] f = "m" -> "r"]
Initial == {"r", "a", "b", "c", "d"}
Dormant == Fibers \ Initial

Children(f) == {g \in Fibers : Parent[g] = f /\ g # f}

RECURSIVE Below(_)
Below(f) == {f} \cup UNION {Below(g) : g \in Children(f)}

ASSUME Mutation \in {"none", "earlyClosing", "missedMember",
                     "ignoresMarker", "reclose"}
ASSUME CloseRoots \subseteq Fibers /\ CloseRoots /= {}
ASSUME SpawnAllowed \in BOOLEAN

VARIABLES
    marked,           \* lineage_closed: permanent close marker
    closing,          \* per-fiber closing flag, published after the markers
    sharedClosed,     \* Shared.closing (what a root shutdown publishes)
    cleaned,          \* fibers whose cleanup ran
    queue,            \* cleanup work left, per fiber
    enqueued,         \* fibers ever put on the cleanup queue
    cleanupCount,     \* ghost: how often cleanup ran for a fiber
    exists,           \* fibers created (a dormant one can still be created)
    st,               \* each entry: "idle" | "selected" | "running" | "done"
    admitted,         \* ghost: this entry's call passed the final check
    markedWhenAdmitted, \* ghost: its owner was already marked at that moment
    afterCommit,      \* ghost: a completed close already covered its owner
    committed,        \* roots whose marker pass completed
    phase,            \* "idle" | "marking" | "publishing"
    opRoot,           \* root of the close pass holding admission
    opMembers         \* its fixed subtree

vars == <<marked, closing, sharedClosed, cleaned, queue, enqueued,
          cleanupCount, exists, st, admitted, markedWhenAdmitted,
          afterCommit, committed, phase, opRoot, opMembers>>

Init ==
    /\ marked = {}
    /\ closing = {}
    /\ sharedClosed = FALSE
    /\ cleaned = {}
    /\ queue = [f \in Fibers |-> 0]
    /\ enqueued = {}
    /\ cleanupCount = [f \in Fibers |-> 0]
    /\ exists = Initial
    /\ st = [f \in Fibers |-> "idle"]
    /\ admitted = [f \in Fibers |-> FALSE]
    /\ markedWhenAdmitted = [f \in Fibers |-> FALSE]
    /\ afterCommit = [f \in Fibers |-> FALSE]
    /\ committed = {}
    /\ phase = "idle"
    /\ opRoot = Root
    /\ opMembers = {}

(***************************************************************************)
(* Closer: one pass holds admission from the collection to the marker      *)
(* pass; `Spawn` and the next close wait for it.                           *)
(***************************************************************************)

Collect(f) ==
    /\ phase = "idle"
    /\ f \in CloseRoots
    /\ f \in exists
    /\ f \notin cleaned
    /\ phase' = "marking"
    /\ opRoot' = f
    /\ opMembers' =
         (((Below(f) \cap exists) \ cleaned)
            \ (IF Mutation = "missedMember" /\ f # "d" THEN {"d"} ELSE {}))
    /\ UNCHANGED <<marked, closing, sharedClosed, cleaned, queue, enqueued,
                   cleanupCount, exists, st, admitted, markedWhenAdmitted,
                   afterCommit, committed>>

\* Postorder: a member is marked only after its children in the same pass.
Mark(f) ==
    /\ phase = "marking"
    /\ f \in opMembers
    /\ f \notin marked
    /\ (Children(f) \cap opMembers) \subseteq marked
    /\ marked' = marked \cup {f}
    /\ UNCHANGED <<closing, sharedClosed, cleaned, queue, enqueued,
                   cleanupCount, exists, st, admitted, markedWhenAdmitted,
                   afterCommit, committed, phase, opRoot, opMembers>>

\* The marker pass is complete: this is the shutdown linearization point.
\* The root publishes `Shared.closing` here (ctx.rs does it between the two
\* phases); no per-fiber `closing` flag is visible yet.
MarkingDone ==
    /\ phase = "marking"
    /\ opMembers \subseteq marked
    /\ committed' = committed \cup {opRoot}
    /\ sharedClosed' = (sharedClosed \/ (opRoot = Root))
    /\ phase' = "publishing"
    /\ UNCHANGED <<marked, closing, cleaned, queue, enqueued, cleanupCount,
                   exists, st, admitted, markedWhenAdmitted, afterCommit,
                   opRoot, opMembers>>

Publish(f) ==
    /\ phase \in {"marking", "publishing"}
    /\ f \in opMembers
    /\ f \notin closing
    /\ (phase = "publishing" \/ Mutation = "earlyClosing")
    /\ closing' = closing \cup {f}
    /\ UNCHANGED <<marked, sharedClosed, cleaned, queue, enqueued,
                   cleanupCount, exists, st, admitted, markedWhenAdmitted,
                   afterCommit, committed, phase, opRoot, opMembers>>

\* The pass is over: schedule cleanup. The task slot of an already scheduled
\* fiber is reused, so a repeated close does not queue its members again.
Finish ==
    /\ phase = "publishing"
    /\ opMembers \subseteq closing
    /\ queue' = [f \in Fibers |->
                   queue[f] + (IF f \in opMembers
                                 /\ (Mutation = "reclose" \/ f \notin enqueued)
                               THEN 1 ELSE 0)]
    /\ enqueued' = enqueued \cup opMembers
    /\ phase' = "idle"
    /\ opMembers' = {}
    /\ UNCHANGED <<marked, closing, sharedClosed, cleaned, cleanupCount,
                   exists, st, admitted, markedWhenAdmitted, afterCommit,
                   committed, opRoot>>

\* A fiber created under the admission lock while its parent's subtree is
\* closed starts with the marker already set (design rule 3).
Spawn ==
    /\ SpawnAllowed
    /\ phase = "idle"
    /\ \E f \in Fibers \ exists :
         /\ Parent[f] \in exists
         /\ LET closed == sharedClosed \/ Parent[f] \in marked
            IN /\ exists' = exists \cup {f}
               /\ marked' = IF closed THEN marked \cup {f} ELSE marked
    /\ UNCHANGED <<closing, sharedClosed, cleaned, queue, enqueued,
                   cleanupCount, st, admitted, markedWhenAdmitted,
                   afterCommit, committed, phase, opRoot, opMembers>>

(***************************************************************************)
(* Cleanup: asynchronous, after the publication, and only once there is no *)
(* call of the fiber still running (the call flight holds the fiber).      *)
(***************************************************************************)
Cleanup(f) ==
    /\ queue[f] > 0
    /\ st[f] # "running"
    /\ queue' = [queue EXCEPT ![f] = @ - 1]
    /\ cleanupCount' = [cleanupCount EXCEPT ![f] = @ + 1]
    /\ cleaned' = cleaned \cup {f}
    /\ UNCHANGED <<marked, closing, sharedClosed, enqueued, exists, st,
                   admitted, markedWhenAdmitted, afterCommit, committed,
                   phase, opRoot, opMembers>>

(***************************************************************************)
(* Dispatcher: select an entry (a snapshot may be stale), then start it.   *)
(* The final check uses only the entry's own owner; a closed entry is      *)
(* skipped as if it had never been selected.                               *)
(***************************************************************************)
Select(e) ==
    /\ st[e] = "idle"
    /\ e \in exists
    /\ st' = [st EXCEPT ![e] = "selected"]
    /\ UNCHANGED <<marked, closing, sharedClosed, cleaned, queue, enqueued,
                   cleanupCount, exists, admitted, markedWhenAdmitted,
                   afterCommit, committed, phase, opRoot, opMembers>>

Start(e) ==
    /\ st[e] = "selected"
    /\ LET open == (e \in exists
                    /\ e \notin cleaned
                    /\ e \notin closing
                    /\ (Mutation = "ignoresMarker" \/ e \notin marked))
       IN /\ st' = [st EXCEPT ![e] = IF open THEN "running" ELSE "done"]
          /\ admitted' = [admitted EXCEPT ![e] = open]
          /\ markedWhenAdmitted' =
               [markedWhenAdmitted EXCEPT ![e] = open /\ e \in marked]
          /\ afterCommit' =
               [afterCommit EXCEPT ![e] =
                  open /\ (\E a \in committed : e \in Below(a))]
    /\ UNCHANGED <<marked, closing, sharedClosed, cleaned, queue, enqueued,
                   cleanupCount, exists, committed, phase, opRoot, opMembers>>

Complete(e) ==
    /\ st[e] = "running"
    /\ st' = [st EXCEPT ![e] = "done"]
    /\ UNCHANGED <<marked, closing, sharedClosed, cleaned, queue, enqueued,
                   cleanupCount, exists, admitted, markedWhenAdmitted,
                   afterCommit, committed, phase, opRoot, opMembers>>

CompleteSome == \E e \in Fibers : Complete(e)

Next ==
    \/ \E f \in Fibers : Collect(f)
    \/ \E f \in Fibers : Mark(f)
    \/ MarkingDone
    \/ \E f \in Fibers : Publish(f)
    \/ Finish
    \/ Spawn
    \/ \E e \in Fibers : Select(e)
    \/ \E e \in Fibers : Start(e)
    \/ CompleteSome
    \/ \E f \in Fibers : Cleanup(f)

Spec == Init /\ [][Next]_vars /\ WF_vars(CompleteSome)

(***************************************************************************)
(* Properties                                                              *)
(***************************************************************************)

\* No ancestor is marked while one of its created descendants is not: the
\* postorder pass plus closed spawns are the only ways to set the marker.
ClosedDownward ==
    \A a \in exists :
        \A d \in exists :
            (a \in marked /\ d \in Below(a)) => d \in marked

\* The marker pass precedes every publication, member by member.
ClosingImpliesMarked == closing \subseteq marked

CleanupAfterMarkers == cleaned \subseteq marked
CleanupAfterPublish == cleaned \subseteq closing

\* An admitted call keeps its fiber until it completes; cleanup cannot run
\* while the call's flight is held.
NoCleanupWithFlight == \A f \in cleaned : st[f] # "running"

\* A repeated close of a fiber (or of an ancestor) schedules its cleanup once.
CleanupOnce == \A f \in Fibers : cleanupCount[f] <= 1

\* The caller's final check never admits a call once its owner's marker is
\* set: the marker load is the admission boundary.
NoAdmitMarkedOwner ==
    \A e \in Fibers : admitted[e] => ~markedWhenAdmitted[e]

\* After a close pass commits, nobody in its fixed subtree is admitted, even
\* while the per-fiber `closing` flags are not published yet.
CommittedStaysOut ==
    \A e \in Fibers : admitted[e] => ~afterCommit[e]

SharedClosedImpliesMarked == sharedClosed => Root \in marked

\* Every admitted call eventually completes: the close and its cleanup never
\* take an accepted call away.
AdmittedComplete == []<>(\A e \in Fibers : st[e] # "running")

======================================================================
