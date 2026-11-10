--------------------------------- MODULE EventsLink -----------------------------------
(**************************************************************************************)
(* A node link: the peer's `link.offers` snapshots (versions increase, the receiver  *)
(* keeps the highest), its announcements and withdrawals of listener entries with   *)
(* `<epoch>.<n>` ids (or the current bare-id rule from another implementation), an  *)
(* announcement that arrives before or after the snapshot that lists its offer, the *)
(* three steps the receiver takes for one announcement (check the epoch, register   *)
(* the entry, insert it), another family's change causing a further snapshot (#593), *)
(* a peer with an entry imported from a node behind it (not to be forwarded), and   *)
(* the host's own announcement to the peer (not an imported entry echoed back).     *)
(*                                                                                    *)
(* Design: docs/design-events-protocol-2026-10-18.md §9 (what a link carries, the    *)
(* announced ids, what a newer offer withdraws, an announcement that arrives before  *)
(* its offer: "对端的 link.offers 不再列出它列出过的 events 提供时，撤下在那次   *)
(* 提供及更早的提供下宣告的条目；还没列出的那次提供的条目留着"), EVP-9-17 (a bare *)
(* id is replaced by id only), and the one-hop rule ("只通告它自己的条目 ... 从不  *)
(* 通告经任何连接引进的条目"). Implementation:                                        *)
(* crates/rutis-bridge/src/events/link.rs (`accept`, `announced_under`,              *)
(* `peer_offer`, `offer_now`, `older_than`), crates/rutis-bridge/src/peer.rs         *)
(* (`Offers`, `since`, the version of each announcement; `send_if_modified` keeps    *)
(* the highest).                                                                     *)
(*                                                                                    *)
(* Simplified (README.md): one peer (plus the entry it imported from a node behind  *)
(* it), two of its own entries, two families (events and another), two offers of    *)
(* events; an offer announces the entries it still has, and the offer boundary -    *)
(* not an `events.off` - is what withdraws what an older offer announced; the link  *)
(* carries one host entry the other way.                                            *)
(**************************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets, EventsHost

CONSTANTS
    MaxEpoch,     \* events offers the peer may make (2)
    MaxVersion,   \* snapshots the peer may send before the run stops (4)
    PeerIds,      \* "epoch" (ids `<epoch>.<n>`) | "bare" (ids without an epoch)
    PeerImports,  \* the peer has an entry imported from a node behind it
    Mutation      \* "none" | "acceptUnlocked" | "offerRace" | "echo" | "forward"

ASSUME MaxEpoch \in {1, 2}
ASSUME MaxVersion \in Nat \ {0}
ASSUME PeerIds \in {"epoch", "bare"}
ASSUME Mutation \in {"none", "acceptUnlocked", "offerRace", "echo", "forward"}
ASSUME Mutation = "forward" => PeerImports

Epochs == 0..MaxEpoch

Nums == {1, 2}

PeerName == "p"

HostName == "h"

BackName == "q"

BarePeer == PeerIds = "bare"

-------------------------------------------------------------------------------
(* Helpers                                                                    *)
-------------------------------------------------------------------------------

(* §9: a bare id says no epoch, and the receiver replaces by id only.          *)
EpochOf(id) == IF BarePeer THEN 0 ELSE id[1]

RECURSIVE RemoveAt(_, _)
RemoveAt(seq, i) ==
    IF seq = <<>> THEN <<>>
    ELSE IF i = 1 THEN Tail(seq)
    ELSE <<Head(seq)>> \o RemoveAt(Tail(seq), i - 1)

Msg(k, id, version, events, other, name) ==
    [k |-> k, id |-> id, version |-> version, events |-> events,
     other |-> other, name |-> name]

OwnId(epoch, n) == IF BarePeer THEN BareId(n) ELSE AnnouncedId(epoch, n)

ImportedId(epoch) == IF BarePeer THEN BareId(3) ELSE AnnouncedId(epoch, 3)

-------------------------------------------------------------------------------
(* State                                                                      *)
-------------------------------------------------------------------------------

VARIABLES
    epoch,      \* the peer's current offer of `events` (0 = not offering)
    announced,  \* the ids its current offer announced
    imported,   \* the ids it announced although they came from another node
    other,      \* the peer offers the other family too
    version,    \* the version of the peer's last snapshot
    pending,    \* announcements and withdrawals the peer will send, in order
    wire,       \* announcements and withdrawals on the link, in order
    snapshots,  \* snapshot messages on the link, in any order
    toPeer,     \* messages from the host to the peer
    shownVer,   \* the highest snapshot version the host kept
    shownEvents,\* the events epoch of the snapshot it kept (0 = not offered)
    lastEvents, \* the last events epoch any snapshot showed
    seen,       \* the newest events epoch the host has seen
    accepted,   \* the peer's ids in the host's table
    accepting,  \* an announcement whose check passed, not inserted (mutation)
    hostSaid    \* names the host announced to the peer (ghost)

vars == <<epoch, announced, imported, other, version, pending, wire, snapshots,
          toPeer, shownVer, shownEvents, lastEvents, seen, accepted, accepting,
          hostSaid>>

-------------------------------------------------------------------------------
(* The peer                                                                   *)
-------------------------------------------------------------------------------

(* A new offer of `events`: the entries the peer still has are announced again *)
(* under the new epoch (their ids restart at 1), and a snapshot follows. The    *)
(* entries it no longer has are withdrawn by the offer boundary, not by an     *)
(* `events.off` (§9). The peer's imported entry is never part of its own offer. *)
OfferEvents ==
    /\ epoch < MaxEpoch
    /\ version < MaxVersion
    /\ epoch' = epoch + 1
    /\ version' = version + 1
    /\ \E living \in SUBSET Nums:
         LET id1 == OwnId(epoch + 1, 1)
             id2 == OwnId(epoch + 1, 2)
         IN /\ announced' = (IF 1 \in living THEN {id1} ELSE {})
                             \cup (IF 2 \in living THEN {id2} ELSE {})
            /\ imported' = {}
            /\ pending' = pending
                           \o (IF 1 \in living
                               THEN <<Msg("on", id1, 0, 0, FALSE, "")>> ELSE <<>>)
                           \o (IF 2 \in living
                               THEN <<Msg("on", id2, 0, 0, FALSE, "")>> ELSE <<>>)
            /\ snapshots' = Append(snapshots,
                                    Msg("offers", <<0, 0>>, version + 1,
                                        epoch + 1, other, ""))
    /\ UNCHANGED <<imported, other, wire, toPeer, shownVer, shownEvents,
                   lastEvents, seen, accepted, accepting, hostSaid>>

(* Another family changes: a further snapshot with the same events offer, on a *)
(* task, so it may come after anything (#593).                               *)
OtherChanges ==
    /\ version < MaxVersion
    /\ version' = version + 1
    /\ other' = ~other
    /\ snapshots' = Append(snapshots,
                            Msg("offers", <<0, 0>>, version + 1, epoch, ~other, ""))
    /\ UNCHANGED <<epoch, announced, imported, pending, wire, toPeer, shownVer,
                   shownEvents, lastEvents, seen, accepted, accepting, hostSaid>>

(* §9: the peer withdraws one of its own entries with `events.off`, not with   *)
(* an offer boundary.                                                        *)
WithdrawOn ==
    /\ announced /= {}
    /\ \E id \in announced:
         /\ announced' = announced \ {id}
         /\ pending' = Append(pending, Msg("off", id, 0, 0, FALSE, ""))
         /\ UNCHANGED <<epoch, imported, other, version, wire, snapshots, toPeer,
                        shownVer, shownEvents, lastEvents, seen, accepted,
                        accepting, hostSaid>>

(* The peer holds an entry it imported from the node behind it. It is not    *)
(* part of the peer's own offer.                                             *)
PeerTakesImported ==
    /\ PeerImports
    /\ imported = {}
    /\ epoch > 0
    /\ imported' = {ImportedId(epoch)}
    /\ UNCHANGED <<epoch, announced, other, version, pending, wire, snapshots,
                   toPeer, shownVer, shownEvents, lastEvents, seen, accepted,
                   accepting, hostSaid>>

(* The peer has an entry imported from the node behind it. §9 forbids passing *)
(* it on: the mutation sends it, the healthy configurations do not.          *)
ForwardImported ==
    /\ Mutation = "forward"
    /\ epoch > 0
    /\ \E id \in imported:
         /\ id \notin announced
         /\ announced' = announced \cup {id}
         /\ pending' = Append(pending, Msg("on", id, 0, 0, FALSE, ""))
         /\ UNCHANGED <<epoch, imported, other, version, wire, snapshots, toPeer,
                        shownVer, shownEvents, lastEvents, seen, accepted,
                        accepting, hostSaid>>

(* The peer sends its next announcement, in order.                            *)
Send ==
    /\ pending /= <<>>
    /\ wire' = Append(wire, Head(pending))
    /\ pending' = Tail(pending)
    /\ UNCHANGED <<epoch, announced, imported, other, version, snapshots, toPeer,
                   shownVer, shownEvents, lastEvents, seen, accepted, accepting,
                   hostSaid>>

-------------------------------------------------------------------------------
(* The host                                                                   *)
-------------------------------------------------------------------------------

(* §9: an announcement. The base model checks the epoch, registers and inserts  *)
(* in one step; `acceptUnlocked` splits them, as link.rs `accept` releases the  *)
(* lock between the check and the insert.                                    *)
DeliverOn ==
    /\ Mutation # "acceptUnlocked"
    /\ wire /= <<>>
    /\ Head(wire).k = "on"
    /\ LET id == Head(wire).id
           e == EpochOf(id)
       IN /\ seen' = IF BarePeer \/ e < seen THEN seen ELSE e
          /\ accepted' =
                 IF BarePeer THEN accepted \cup {id}
                 ELSE IF e < seen THEN accepted
                 ELSE KeptAfterEpoch(accepted, e) \cup {id}
          /\ wire' = Tail(wire)
    /\ UNCHANGED <<epoch, announced, imported, other, version, pending, snapshots,
                   toPeer, shownVer, shownEvents, lastEvents, accepting, hostSaid>>

AcceptCheck ==
    /\ Mutation = "acceptUnlocked"
    /\ accepting = {}
    /\ wire /= <<>>
    /\ Head(wire).k = "on"
    /\ LET id == Head(wire).id
           e == EpochOf(id)
       IN /\ seen' = IF BarePeer \/ e < seen THEN seen ELSE e
          /\ accepted' =
                 IF BarePeer THEN accepted
                 ELSE IF e < seen THEN accepted
                 ELSE KeptAfterEpoch(accepted, e)
          /\ accepting' =
                 IF ~BarePeer /\ e < seen THEN {} ELSE {id}
          /\ wire' = Tail(wire)
    /\ UNCHANGED <<epoch, announced, imported, other, version, pending, snapshots,
                   toPeer, shownVer, shownEvents, lastEvents, hostSaid>>

AcceptInsert ==
    /\ Mutation = "acceptUnlocked"
    /\ accepting /= {}
    /\ accepted' = accepted \cup accepting
    /\ accepting' = {}
    /\ UNCHANGED <<epoch, announced, imported, other, version, pending, wire,
                   snapshots, toPeer, shownVer, shownEvents, lastEvents, seen,
                   hostSaid>>

(* §9: an `events.off` withdraws the id; an id not in the table is ignored.   *)
DeliverOff ==
    /\ wire /= <<>>
    /\ Head(wire).k = "off"
    /\ LET id == Head(wire).id
       IN /\ accepted' = accepted \ {id}
          /\ wire' = Tail(wire)
    /\ UNCHANGED <<epoch, announced, imported, other, version, pending, snapshots,
                   toPeer, shownVer, shownEvents, lastEvents, seen, accepting,
                   hostSaid>>

(* §9: only the highest version counts; a newer offer withdraws what older     *)
(* offers announced; an announcement of an offer not shown yet stays (#593).  *)
(* `offerRace` is the behaviour before #593: a snapshot without events        *)
(* withdraws everything. The bare-id peer keeps its entries: the host has no  *)
(* epoch to compare (EVP-9-17).                                               *)
DeliverOffers ==
    /\ \E i \in 1..Len(snapshots):
         LET m == snapshots[i]
             newer == m.version > shownVer
         IN /\ snapshots' = RemoveAt(snapshots, i)
            /\ seen' = IF ~newer \/ BarePeer THEN seen
                       ELSE IF m.events > seen THEN m.events ELSE seen
            /\ shownVer' = IF newer THEN m.version ELSE shownVer
            /\ shownEvents' = IF newer THEN m.events ELSE shownEvents
            /\ lastEvents' = IF ~newer THEN lastEvents
                             ELSE IF m.events > 0 THEN m.events ELSE lastEvents
            /\ accepted' =
                   IF ~newer THEN accepted
                   ELSE IF BarePeer THEN accepted
                   ELSE IF m.events > 0
                        THEN KeptAfterEpoch(accepted, m.events)
                   ELSE IF Mutation = "offerRace" THEN {}
                   ELSE KeptAfterOffer(accepted, lastEvents)
    /\ UNCHANGED <<epoch, announced, imported, other, version, pending, wire,
                   toPeer, accepting, hostSaid>>

(* The host announces its own entry to the peer; `echo` announces an imported   *)
(* one too, which §9 forbids (one hop).                                      *)
HostAnnounce ==
    /\ hostSaid = {}
    /\ \/ /\ hostSaid' = {HostName}
          /\ toPeer' = toPeer \cup {Msg("announce", <<0, 0>>, 0, 0, FALSE, HostName)}
       \/ /\ Mutation = "echo"
          /\ accepted /= {}
          /\ hostSaid' = {PeerName}
          /\ toPeer' = toPeer \cup {Msg("announce", <<0, 0>>, 0, 0, FALSE, PeerName)}
    /\ UNCHANGED <<epoch, announced, imported, other, version, pending, wire,
                   snapshots, shownVer, shownEvents, lastEvents, seen, accepted,
                   accepting>>

(* The peer reads the host's message; it changes nothing here.                *)
PeerTake ==
    /\ toPeer /= {}
    /\ \E m \in toPeer: toPeer' = toPeer \ {m}
    /\ UNCHANGED <<epoch, announced, imported, other, version, pending, wire,
                   snapshots, shownVer, shownEvents, lastEvents, seen, accepted,
                   accepting, hostSaid>>

(* The link is quiet and nothing is left to send. A stuttering step, so TLC    *)
(* can check for deadlocks without a terminal state of its own.               *)
Quiet ==
    /\ wire = <<>>
    /\ pending = <<>>
    /\ snapshots = <<>>
    /\ toPeer = {}
    /\ accepting = {}

Done ==
    /\ Quiet
    /\ UNCHANGED vars

Next ==
    \/ OfferEvents
    \/ OtherChanges
    \/ WithdrawOn
    \/ PeerTakesImported
    \/ ForwardImported
    \/ Send
    \/ DeliverOn
    \/ AcceptCheck
    \/ AcceptInsert
    \/ DeliverOff
    \/ DeliverOffers
    \/ HostAnnounce
    \/ PeerTake
    \/ Done

Init ==
    /\ epoch = 0
    /\ announced = {}
    /\ imported = {}
    /\ other = FALSE
    /\ version = 0
    /\ pending = <<>>
    /\ wire = <<>>
    /\ snapshots = <<>>
    /\ toPeer = {}
    /\ shownVer = 0
    /\ shownEvents = 0
    /\ lastEvents = 0
    /\ seen = 0
    /\ accepted = {}
    /\ accepting = {}
    /\ hostSaid = {}

Spec == Init /\ [][Next]_vars

-------------------------------------------------------------------------------
(* Properties                                                                 *)
-------------------------------------------------------------------------------

(* §9: with nothing on the link and nothing left to send, the peer's ids in the  *)
(* host's table are exactly those its current offer announced.                *)
Idle == Quiet

CurrentOffer ==
    Idle => accepted = announced

(* §9: entries taken over a link are never announced back (one hop).          *)
NoEcho ==
    PeerName \notin hostSaid

(* §9: a peer never announces the entry it imported from the node behind it.   *)
NoForward ==
    announced \cap imported = {}

=======================================================================================
