--------------------------- MODULE PeerServices ---------------------------
(***************************************************************************)
(* A service announced and withdrawn across one session. The provider     *)
(* takes versions from the session's single increasing sequence; the       *)
(* importer keeps the newest version it saw, and a withdrawal at a version *)
(* stays a tombstone: an announcement older than the withdrawal must not   *)
(* provide the service again.                                              *)
(*                                                                         *)
(* Design: docs/design-remote-plugins-2026-10-03.md §5.2 (`services.       *)
(* announce`, `services.withdraw`: versions increase, an older announcement *)
(* never overrides a newer one, the importer keeps the withdrawn version). *)
(* Implementation: crates/rutis-bridge/src/services.rs (`ExportOne`,       *)
(* `Peer::next_version`), crates/rutis-bridge/src/session/plugin/          *)
(* services.rs (`Imports::announce` ignores a version <= the current one,  *)
(* `Imports::withdraw` keeps the withdrawn version even without an         *)
(* announcement before it).                                                *)
(*                                                                         *)
(* Simplified (README.md): one name, one session, announcements and        *)
(* withdrawals on one wire that can arrive in any order; a message that is *)
(* older than what the importer saw is ignored, which is what the provider *)
(* guarantees by taking versions in order. Multi-hop is not modelled.      *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
    MaxVersion, \* announcements and withdrawals the provider may make
    Mutation    \* "none", or a deliberate bug (README, "Mutations")

ASSUME MaxVersion \in Nat \ {0}
ASSUME Mutation \in {"none", "staleWins", "noTombstone"}

VARIABLES
    version,    \* the last version the provider took
    wire,       \* messages on the session, any order
    seen,       \* the newest version the importer applied
    withdrawnAt,\* the newest withdrawal the importer applied
    state,      \* the importer: "none" | "provided" | "withdrawn"
    bad         \* ghost: a stale announcement provided the service

vars == <<version, wire, seen, withdrawnAt, state, bad>>

Init ==
    /\ version = 0
    /\ wire = {}
    /\ seen = 0
    /\ withdrawnAt = 0
    /\ state = "none"
    /\ bad = FALSE

(***************************************************************************)
(* The provider: announces the service, withdraws it. Versions increase.   *)
(***************************************************************************)

Announce ==
    /\ version < MaxVersion
    /\ version' = version + 1
    /\ wire' = wire \cup {[kind |-> "announce", version |-> version + 1]}
    /\ UNCHANGED <<seen, withdrawnAt, state, bad>>

Withdraw ==
    /\ version < MaxVersion
    /\ version' = version + 1
    /\ wire' = wire \cup {[kind |-> "withdraw", version |-> version + 1]}
    /\ UNCHANGED <<seen, withdrawnAt, state, bad>>

(***************************************************************************)
(* The importer: applies a message unless it is older than what it saw.    *)
(***************************************************************************)

Deliver(m) ==
    /\ m \in wire
    /\ wire' = wire \ {m}
    /\ IF m.version <= seen
       THEN IF Mutation = "staleWins" /\ m.kind = "announce"
            THEN /\ state' = "provided"
                 /\ bad' = TRUE
                 /\ UNCHANGED <<seen, withdrawnAt>>
            ELSE UNCHANGED <<seen, withdrawnAt, state, bad>>
       ELSE IF Mutation = "noTombstone" /\ m.kind = "withdraw"
            THEN /\ state' = "withdrawn"
                 /\ withdrawnAt' = m.version
                 /\ UNCHANGED <<seen, bad>>
            ELSE /\ seen' = m.version
                 /\ withdrawnAt' = IF m.kind = "withdraw"
                                  THEN m.version ELSE withdrawnAt
                 /\ state' = IF m.kind = "announce"
                             THEN "provided" ELSE "withdrawn"
                 /\ bad' = (bad \/ (m.kind = "announce"
                                     /\ m.version <= withdrawnAt))
    /\ UNCHANGED <<version>>

Next ==
    \/ Announce
    \/ Withdraw
    \/ \E m \in wire : Deliver(m)

Spec == Init /\ [][Next]_vars /\ WF_vars(Next)

(***************************************************************************)
(* Properties.                                                             *)
(***************************************************************************)

\* A withdrawal stays a tombstone: no announcement older than it provides
\* the service again.
NoStaleRestore ==
    bad = FALSE
===============================================================================
