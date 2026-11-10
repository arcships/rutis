--------------------------- MODULE PeerPair -----------------------------
(***************************************************************************)
(* Two nodes hosting rows for each other on one connection: both offer     *)
(* `plugins`, both have a row to run on the other, both start cold at the  *)
(* same time, the connection breaks and comes back. The question is        *)
(* REM-5.1-5: a node answers the other's hello without waiting for its own *)
(* row to be loaded, so two nodes that wait for each other cannot          *)
(* deadlock; a broken link stops the rows, the old rows are cleaned before *)
(* the next hello is answered, and both rows run again.                    *)
(*                                                                         *)
(* Design: docs/design-remote-plugins-2026-10-03.md §4.4 (a new session is *)
(* answered only after the old lease's rows are cleaned) and §5.1 (node    *)
(* feature plugins depend on the session, not on remote rows).            *)
(* Implementation: crates/rutis-bridge/src/session/plugin/mod.rs (`mount`  *)
(* registers the rows and answers before it waits for anything it runs),   *)
(* crates/rutis-bridge/src/session/plugin/rows.rs (the session's table),   *)
(* and crates/rutis-bridge/src/session/plugin/run.rs (a run row waits for  *)
(* the session and stops with it; `crates/rutis-loader/tests/            *)
(* session_rows.rs`).                                                      *)
(*                                                                         *)
(* Simplified (README.md): one connection, two rounds at most, one row     *)
(* each, no authentication or handshake deadline; "cleaned" means the      *)
(* host's table of the old session is empty. A node's answer is a step of  *)
(* its own; it does not wait for its outgoing row.                         *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
    MaxRounds,  \* connections this run may have
    Faults,     \* the connection may break
    Mutation    \* "none", or a deliberate bug (README, "Mutations")

ASSUME MaxRounds \in Nat \ {0}
ASSUME Faults \in BOOLEAN
ASSUME Mutation \in {"none", "waitOut", "keepOnBreak", "answerBeforeClean"}

Nodes == {"A", "B"}

Other(n) == IF n = "A" THEN "B" ELSE "A"

VARIABLES
    link,       \* "down" | "up"
    round,      \* how many connections there have been
    answered,   \* per node: it answered the hello of this connection
    out,        \* per node: its row on the other: "idle" | "loading" | "loaded"
    inn,        \* per node: the other's rows loaded here
    waiting,    \* ghost: a node waits for its own row before answering
    badAnswer   \* ghost: a hello was answered with old rows still here

vars == <<link, round, answered, out, inn, waiting, badAnswer>>

Init ==
    /\ link = "down"
    /\ round = 0
    /\ answered = [n \in Nodes |-> FALSE]
    /\ out = [n \in Nodes |-> "idle"]
    /\ inn = [n \in Nodes |-> {}]
    /\ waiting = [n \in Nodes |-> FALSE]
    /\ badAnswer = FALSE

(***************************************************************************)
(* The connection.                                                          *)
(***************************************************************************)

Connect ==
    /\ link = "down"
    /\ round < MaxRounds
    /\ link' = "up"
    /\ round' = round + 1
    /\ answered' = [n \in Nodes |-> FALSE]
    /\ out' = [n \in Nodes |-> "idle"]
    /\ waiting' = [n \in Nodes |-> FALSE]
    /\ UNCHANGED <<inn, badAnswer>>  \* the old session's rows are still here

\* The host finished disposing the rows of the session that ended.
Clean(n) ==
    /\ inn[n] /= {}
    /\ inn' = [inn EXCEPT ![n] = {}]
    /\ UNCHANGED <<link, round, answered, out, waiting, badAnswer>>

\* The hello is answered. It does not wait for the node's own outgoing row
\* (the `waitOut` mutation does, and two cold nodes then deadlock).
Answer(n) ==
    /\ link = "up"
    /\ ~answered[n]
    /\ (Mutation = "answerBeforeClean" \/ inn[n] = {})
    /\ (Mutation # "waitOut" \/ out[n] = "loaded")
    /\ answered' = [answered EXCEPT ![n] = TRUE]
    /\ badAnswer' = (badAnswer \/ inn[n] /= {})
    /\ UNCHANGED <<link, round, out, inn, waiting>>

\* The `waitOut` mutation records the wait: the node has an answer to give
\* and holds it until its own row is loaded.
WaitOut(n) ==
    /\ Mutation = "waitOut"
    /\ link = "up"
    /\ ~answered[n]
    /\ out[n] # "loaded"
    /\ waiting' = [waiting EXCEPT ![n] = TRUE]
    /\ UNCHANGED <<link, round, answered, out, inn, badAnswer>>

(***************************************************************************)
(* The rows.                                                                *)
(***************************************************************************)

\* The far end answered, so its `plugins` offer is there: load ours there.
StartOut(n) ==
    /\ link = "up"
    /\ answered[Other(n)]
    /\ out[n] = "idle"
    /\ n \notin inn[Other(n)]
    /\ out' = [out EXCEPT ![n] = "loading"]
    /\ UNCHANGED <<link, round, answered, inn, waiting, badAnswer>>

Loaded(n) ==
    /\ link = "up"
    /\ out[n] = "loading"
    /\ answered[Other(n)]
    /\ out' = [out EXCEPT ![n] = "loaded"]
    /\ inn' = [inn EXCEPT ![Other(n)] = @ \cup {n}]
    /\ UNCHANGED <<link, round, answered, waiting, badAnswer>>

(***************************************************************************)
(* The environment.                                                         *)
(***************************************************************************)

Break ==
    /\ Faults
    /\ link = "up"
    /\ link' = "down"
    /\ IF Mutation = "keepOnBreak"
       THEN UNCHANGED <<out, inn>>
       ELSE /\ out' = [n \in Nodes |-> "idle"]
            /\ UNCHANGED inn
    /\ answered' = [n \in Nodes |-> FALSE]
    /\ waiting' = [n \in Nodes |-> FALSE]
    /\ UNCHANGED <<round, badAnswer>>

Next ==
    \/ Connect
    \/ \E n \in Nodes : Clean(n) \/ Answer(n) \/ WaitOut(n)
                        \/ StartOut(n) \/ Loaded(n)
    \/ Break

Spec == Init /\ [][Next]_vars /\ WF_vars(Next)

(***************************************************************************)
(* Properties.                                                             *)
(***************************************************************************)

\* A node answers a hello only when the rows of the session it replaces are
\* gone.
CleanedBeforeAnswer ==
    badAnswer = FALSE

\* A break stops the outgoing rows; they wait for the next session.
RowsStopOnBreak ==
    (link = "down") => \A n \in Nodes : out[n] # "loaded"

\* Answering does not wait for the node's own row to be loaded. The ghost
\* is set by the `waitOut` mutation.
NoWaitingForOut ==
    \A n \in Nodes : ~waiting[n]

\* Liveness: with the link up, both rows eventually run (two cold nodes
\* that wait for each other would never get there).
EventuallyBothLoaded ==
    (link = "up") ~> (\A n \in Nodes : out[n] = "loaded")
===============================================================================
