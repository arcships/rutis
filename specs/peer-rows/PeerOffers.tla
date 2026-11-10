--------------------------- MODULE PeerOffers ----------------------------
(***************************************************************************)
(* The loader side of node-hosted rows: the far end's `link.offers`        *)
(* snapshots (versions increase, the receiver keeps the highest, and a     *)
(* family carries the number of its registration, its `since`), the gate   *)
(* that provides `PeerRows` only after the schema refresh, rows that wait *)
(* while the far end offers no `plugins` or is offline, a withdrawal that  *)
(* takes the gate and its rows away while the peer stays, and a new       *)
(* registration (`since` going up) that reloads them.                      *)
(*                                                                         *)
(* Design: docs/design-remote-plugins-2026-10-03.md §5.2 (`link.offers`:  *)
(* version, `since`, an older announcement ignored) and §5.3 (the two-     *)
(* stage gate, offline resolution, withdrawal, per-row one start).         *)
(* Implementation: crates/rutis-bridge/src/peer.rs (`Offers`, `epoch`,    *)
(* `send_if_modified`, `register` keeping the handler with its `since`),   *)
(* and crates/rutis-bridge/src/session/plugin/run.rs (a run row waits for  *)
(* the session and starts once; `run.rs:89` describes the plugin first).   *)
(* The `since` bug is the regression case of `peer.rs`:                 *)
(* `a_family_is_announced_only_with_its_offer`.                            *)
(*                                                                         *)
(* Simplified (README.md): one row, one far end, one family (`plugins`)    *)
(* whose offer gates it, two registrations; a snapshot is a message on one *)
(* wire that may be delivered in any order, versions order them; a link   *)
(* that goes down forgets what the loader saw and, on the way up, carries  *)
(* the far end's current snapshot again.                                   *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
    MaxRegs,       \* registrations of `plugins` the far end may offer
    MaxBreaks,     \* times the link may go down
    MaxWithdrawals,\* withdrawals of `plugins` the far end may make
    Faults,        \* the link may go down and up
    Withdrawals,   \* the far end may withdraw `plugins`
    Mutation       \* "none", or a deliberate bug (README, "Mutations")

ASSUME MaxRegs \in Nat \ {0}
ASSUME MaxBreaks \in Nat
ASSUME MaxWithdrawals \in Nat
ASSUME Faults \in BOOLEAN
ASSUME Withdrawals \in BOOLEAN
ASSUME Mutation \in {"none", "twoOffers", "gateCloseFails", "keepOnWithdraw",
                     "noReload"}

VARIABLES
    bFams,      \* the far end's families: {"rows"} and, when offered, "plugins"
    bSince,     \* the number of the last `plugins` registration (0: never)
    bVersion,   \* the version of the far end's last snapshot
    wire,       \* snapshots on the link, any order
    aSeen,      \* the highest version the loader has applied
    aFams,      \* the families of the snapshot the loader applied
    aEpoch,     \* the `plugins` epoch in it (0: not offered)
    aGate,      \* "closed" | "refreshing" | "open"
    aRow,       \* "idle" | "starting" | "running" | "failed"
    aPeer,      \* the session: "up" | "down"
    breaks,     \* how often the link went down
    withdrawals \* how often `plugins` was withdrawn

vars == <<bFams, bSince, bVersion, wire, aSeen, aFams, aEpoch, aGate, aRow,
          aPeer, breaks, withdrawals>>

Init ==
    /\ bFams = {"rows"}
    /\ bSince = 0
    /\ bVersion = 0
    /\ wire = {}
    /\ aSeen = 0
    /\ aFams = {}
    /\ aEpoch = 0
    /\ aGate = "closed"
    /\ aRow = "idle"
    /\ aPeer = "up"
    /\ breaks = 0
    /\ withdrawals = 0

(***************************************************************************)
(* The far end: registers `plugins` (a new `since`), withdraws it, and     *)
(* announces what it offers.                                               *)
(***************************************************************************)

\* A registration. The family and its `since` go into one snapshot. The
\* `twoOffers` mutation is the old bug: an announcement in progress sees
\* the family without its `since` (epoch 0), and the next one carries the
\* real `since`, so the loader sees two offers of one registration.
BOffer ==
    /\ aPeer = "up"
    /\ bSince < MaxRegs
    /\ "plugins" \notin bFams
    /\ LET r == bSince + 1
           real == [version |-> bVersion + 1,
                    fams |-> bFams \cup {"plugins"},
                    since |-> r]
           fake == [version |-> bVersion + 1,
                    fams |-> bFams \cup {"plugins"},
                    since |-> 0]
       IN /\ bSince' = r
          /\ bFams' = bFams \cup {"plugins"}
          /\ IF Mutation = "twoOffers"
             THEN /\ wire' = wire \cup {fake,
                                        [version |-> bVersion + 2,
                                         fams |-> bFams \cup {"plugins"},
                                         since |-> r]}
                  /\ bVersion' = bVersion + 2
             ELSE /\ wire' = wire \cup {real}
                  /\ bVersion' = bVersion + 1
          /\ UNCHANGED <<aSeen, aFams, aEpoch, aGate, aRow, aPeer, breaks,
                         withdrawals>>

\* The family goes: a snapshot without it. `bSince` is kept, so a later
\* registration is a higher offer.
BWithdraw ==
    /\ Withdrawals
    /\ withdrawals < MaxWithdrawals
    /\ aPeer = "up"
    /\ "plugins" \in bFams
    /\ bFams' = bFams \ {"plugins"}
    /\ bVersion' = bVersion + 1
    /\ withdrawals' = withdrawals + 1
    /\ wire' = wire \cup {[version |-> bVersion + 1,
                           fams |-> bFams \ {"plugins"},
                           since |-> 0]}
    /\ UNCHANGED <<bSince, aSeen, aFams, aEpoch, aGate, aRow, aPeer, breaks>>

(***************************************************************************)
(* The loader: applies the newest snapshot; a new epoch refreshes the      *)
(* schema and opens the gate; a withdrawal closes it and stops the row.    *)
(***************************************************************************)

ADeliver(m) ==
    /\ aPeer = "up"
    /\ m \in wire
    /\ wire' = wire \ {m}
    /\ IF m.version <= aSeen
       THEN UNCHANGED <<aSeen, aFams, aEpoch, aGate, aRow>>
       ELSE LET e == IF "plugins" \in m.fams THEN m.since ELSE 0 IN
            /\ aSeen' = m.version
            /\ aFams' = m.fams
            /\ IF e = aEpoch
               THEN UNCHANGED <<aEpoch, aGate, aRow>>
               ELSE /\ aEpoch' = e
                    /\ aGate' = IF e = 0
                               THEN IF Mutation = "keepOnWithdraw"
                                    THEN aGate ELSE "closed"
                               ELSE IF Mutation = "noReload"
                                    THEN aGate ELSE "refreshing"
                    /\ aRow' = IF Mutation = "keepOnWithdraw" /\ e = 0
                               THEN aRow
                               ELSE IF aRow \in {"starting", "running"}
                                    THEN IF Mutation = "gateCloseFails"
                                            /\ aRow = "starting"
                                         THEN "failed" ELSE "idle"
                                    ELSE aRow
    /\ UNCHANGED <<bFams, bSince, bVersion, aPeer, breaks, withdrawals>>

\* The schema refresh finished: the gate opens and the row starts, once.
ARefresh ==
    /\ aGate = "refreshing"
    /\ aGate' = "open"
    /\ aRow' = IF aRow = "idle" THEN "starting" ELSE aRow
    /\ UNCHANGED <<bFams, bSince, bVersion, wire, aSeen, aFams, aEpoch,
                   aPeer, breaks, withdrawals>>

\* The row finished applying there and runs.
ARowLoaded ==
    /\ aRow = "starting"
    /\ aRow' = "running"
    /\ UNCHANGED <<bFams, bSince, bVersion, wire, aSeen, aFams, aEpoch,
                   aGate, aPeer, breaks, withdrawals>>

(***************************************************************************)
(* The link: down stops the row and forgets the offers; up carries the far *)
(* end's current snapshot again.                                           *)
(***************************************************************************)

ALinkDown ==
    /\ Faults
    /\ breaks < MaxBreaks
    /\ aPeer = "up"
    /\ aPeer' = "down"
    /\ aGate' = "closed"
    /\ aRow' = IF aRow \in {"starting", "running"} THEN "idle" ELSE aRow
    /\ aSeen' = 0
    /\ aFams' = {}
    /\ aEpoch' = 0
    /\ wire' = {}
    /\ breaks' = breaks + 1
    /\ UNCHANGED <<bFams, bSince, bVersion, withdrawals>>

ALinkUp ==
    /\ aPeer = "down"
    /\ aPeer' = "up"
    /\ bVersion' = bVersion + 1
    /\ wire' = {[version |-> bVersion + 1,
                 fams |-> bFams,
                 since |-> IF "plugins" \in bFams THEN bSince ELSE 0]}
    /\ UNCHANGED <<bFams, bSince, aSeen, aFams, aEpoch, aGate, aRow,
                   breaks, withdrawals>>

Next ==
    \/ BOffer
    \/ BWithdraw
    \/ \E m \in wire : ADeliver(m)
    \/ ARefresh
    \/ ARowLoaded
    \/ ALinkDown
    \/ ALinkUp

Spec == Init /\ [][Next]_vars /\ WF_vars(Next)

(***************************************************************************)
(* Properties.                                                             *)
(***************************************************************************)

\* A family appears in a snapshot only with the number of its registration.
EpochWithFamily ==
    ("plugins" \in aFams) => aEpoch # 0

\* The row starts only under an open gate for an offered `plugins` epoch.
GatedStart ==
    (aRow \in {"starting", "running"}) =>
        (aGate = "open" /\ "plugins" \in aFams /\ aEpoch # 0)

\* Withdrawing `plugins` closes the gate and stops the row; the peer stays.
WithdrawalStops ==
    ("plugins" \notin aFams) => (aGate = "closed" /\ aRow # "running")

\* A row that waits for the gate does not fail: the old `PeerRows` bug left
\* it Failed when the gate was withdrawn before it applied (#438).
NoFailedRow ==
    aRow # "failed"

\* With no link, the row waits and the gate is closed (offline resolution:
\* the row waits for `PeerRows`, it is not Unresolved).
OfflineWaits ==
    (aPeer = "down") => (aGate = "closed" /\ aRow # "running")

\* A new registration is followed: once the loader is on, it ends up
\* running the far end's current `plugins` offer.
ReloadsOnOffer ==
    (aPeer = "up" /\ "plugins" \in bFams) ~>
        (aEpoch = bSince /\ aGate = "open" /\ aRow = "running")
===============================================================================
