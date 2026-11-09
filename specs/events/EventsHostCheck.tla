---------------------------- MODULE EventsHostCheck ----------------------------
(********************************************************************************)
(* Checks the definitions of EventsHost.tla on examples taken word for word from *)
(* the protocol spec, then stays in one state: there is no state to explore.    *)
(*                                                                              *)
(* Design: docs/design-events-protocol-2026-10-18.md §4.3, §4.5, §5.1, §5.4,    *)
(* §6, §9. This is not a protocol model: the three stateful modules next to this *)
(* file are. It pins down what the shared definitions mean.                     *)
(********************************************************************************)
EXTENDS EventsHost, TLC

-------------------------------------------------------------------------------
(* §4.3: by name, or a name that starts with the prefix                       *)
-------------------------------------------------------------------------------

Exact == [text |-> "session/created", prefix |-> FALSE, global |-> FALSE]
Prefix == [text |-> "session/", prefix |-> TRUE, global |-> FALSE]
Other == [text |-> "session/", prefix |-> FALSE, global |-> FALSE]

ASSUME Matches(Exact, "session/created")
ASSUME ~Matches(Exact, "session/update")
ASSUME Matches(Prefix, "session/created")
ASSUME Matches(Prefix, "session/")
ASSUME ~Matches(Prefix, "sessio")
ASSUME ~Matches(Other, "session/created")

ASSUME Matching(<<Exact, Prefix, Other>>, "session/created") = <<Exact, Prefix>>

-------------------------------------------------------------------------------
(* §4.5: elsewhere is by name, and the kernel diagnostics never count          *)
-------------------------------------------------------------------------------

ASSUME Elsewhere(<<Exact, [text |-> "rutis::ServiceChanged",
                           prefix |-> FALSE, global |-> FALSE]>>, {}) =
       {"session/created"}
ASSUME Elsewhere(<<Exact>>, {Exact}) = {}

-------------------------------------------------------------------------------
(* §5.1 / §6: the filter is asked once per non-global entry, in order; a wrong *)
(* number of answers fails the dispatch                                         *)
-------------------------------------------------------------------------------

Global == [text |-> "t", prefix |-> FALSE, global |-> TRUE]
A == [text |-> "t", prefix |-> FALSE, global |-> FALSE]
B == [text |-> "t", prefix |-> FALSE, global |-> FALSE]

ASSUME Asked(<<Global, A, B>>) = <<A, B>>
ASSUME Filtered(<<Global, A, B>>, <<FALSE, TRUE>>) = <<Global, B>>
ASSUME Filtered(<<Global, A, B>>, <<FALSE>>) = <<>>
ASSUME FilterFails(<<TRUE>>, Asked(<<Global, A, B>>))

-------------------------------------------------------------------------------
(* §5.4: when the next call starts                                             *)
-------------------------------------------------------------------------------

ASSUME MayStart("emit", 1, 0, 0, 2)
ASSUME MayStart("parallel", 1, 0, 0, 2)
ASSUME ~MayStart("serial", 1, 0, 0, 2)
ASSUME MayStart("serial", 1, 1, 0, 2)
ASSUME ~MayStart("waterfall", 1, 1, 0, 2)
ASSUME MayStart("waterfall", 1, 1, 1, 2)
ASSUME ~MayStart("bail", 1, 0, 1, 2)

(* "A result" in every language: null / undefined / false are none.           *)
ASSUME ~HasResult("null") /\ ~HasResult("undefined") /\ ~HasResult("false")
ASSUME HasResult("0") /\ HasResult("true")

-------------------------------------------------------------------------------
(* §9: export / import, and narrowing a prefix                                 *)
-------------------------------------------------------------------------------

SessionPrefix == Item("session/", TRUE)
ChatPrefix == Item("chat/", TRUE)
RowPrefix == Item("row/", TRUE)
SessionName == Item("session/created", FALSE)
ASSUME Narrow(SessionPrefix, SessionPrefix) = SessionPrefix
ASSUME Narrow(SessionName, SessionPrefix) = SessionName
ASSUME Narrow(SessionPrefix, SessionName) = SessionName
ASSUME Narrow(ChatPrefix, SessionPrefix) = NoItem
ASSUME Narrowed({ChatPrefix, SessionPrefix, RowPrefix}, {SessionPrefix}) =
       {SessionPrefix}
ASSUME Accepted({SessionPrefix}, SessionName)
ASSUME ~Accepted({RowPrefix}, SessionName)
ASSUME ServiceItem(Item("chat", FALSE)) /\ ~ServiceItem(ChatPrefix)

ASSUME RegistrationAllowed("session/created")
ASSUME ~RegistrationAllowed("internal/dispatch")
ASSUME EmittableByPlugin("session/created")
ASSUME ~EmittableByPlugin("rutis/row")

(* §9: ids `<epoch>.<n>` say their offer; a bare id does not.                 *)
ASSUME AnnouncedEpoch(<<3, 12>>) = 3
ASSUME HasEpoch(<<3, 12>>)
ASSUME ~HasEpoch(<<0, 12>>)

-------------------------------------------------------------------------------
(* The one state: nothing to explore                                          *)
-------------------------------------------------------------------------------

VARIABLE x

vars == <<x>>

Init == x = 0

Next == UNCHANGED x

Spec == Init /\ [][Next]_vars
===============================================================================
