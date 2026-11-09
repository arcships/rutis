--------------------------------- MODULE EventsHost ---------------------------------
(*************************************************************************************)
(* Definitions of the host's rules, shared by the event models next to this file.    *)
(* No state: every operator here is a function of its arguments.                     *)
(*                                                                                   *)
(* Design: docs/design-events-protocol-2026-10-18.md §3 (scope is a descriptor),     *)
(* §4.3 (registration order), §4.5 (elsewhere), §5.1 (snapshot, filter, liveness),   *)
(* §5.4 (the five modes and when they start the next call), §6 (filters), §9        *)
(* (export / import, narrowing a prefix announcement).                               *)
(* Implementation: crates/rutis/src/bus/table.rs (`select`),                         *)
(* crates/rutis/src/bus/run.rs (the modes, `filter_each`, `apply_answers`),          *)
(* crates/rutis-bridge/src/events/hub.rs (elsewhere, the selector),                  *)
(* crates/rutis-bridge/src/events/link.rs (accept, `older_than`).                    *)
(*                                                                                   *)
(* The three stateful modules of this directory use a part of these definitions;     *)
(* EventsHostCheck.tla evaluates them on the examples of the spec.                   *)
(*************************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

-------------------------------------------------------------------------------
(* Names, prefixes, registration order (§4.3, §5.1 step 1)                    *)
-------------------------------------------------------------------------------

(* A name / prefix item is a record: `text` is the name, or the part before   *)
(* the `*`, and `prefix` says which. A listener entry has the same fields, so *)
(* matching is one operator. The wire writes a prefix with a trailing `*`.    *)
Item(text, prefix) == [text |-> text, prefix |-> prefix]

NoItem == Item("", FALSE)

IsPrefixItem(item) == item.prefix

PrefixBody(item) == item.text

IsPrefixOf(prefix, name) ==
    Len(prefix) <= Len(name) /\ SubSeq(name, 1, Len(prefix)) = prefix

(* §4.3: an entry matches by its name, or a name that starts with its prefix. *)
Matches(entry, name) ==
    IF entry.prefix THEN IsPrefixOf(entry.text, name) ELSE entry.text = name

(* The entries of the sequence `entries`, as a set.                          *)
Elements(entries) == { entries[i] : i \in 1..Len(entries) }

(* The entries of `entries` (already in registration order) that are in the *)
(* set `keep`, in that order.                                                *)
RECURSIVE Ordered(_, _)
Ordered(entries, keep) ==
    IF entries = <<>> THEN <<>>
    ELSE IF Head(entries) \in keep
         THEN <<Head(entries)>> \o Ordered(Tail(entries), keep)
         ELSE Ordered(Tail(entries), keep)

Matching(entries, name) ==
    Ordered(entries, { entry \in Elements(entries) : Matches(entry, name) })

(* §4.5: an entry is in elsewhere(R) by name; the kernel's process-local     *)
(* `rutis::*` diagnostics are never counted.                                  *)
CountsElsewhere(entry) == ~IsPrefixOf("rutis::", entry.text)

Elsewhere(entries, own) ==
    { entry.text : entry \in { e \in Elements(entries) : CountsElsewhere(e) } \ own }

-------------------------------------------------------------------------------
(* Filters (§6)                                                              *)
-------------------------------------------------------------------------------

(* `filter` is a set of the entries the plugin's predicate keeps: the models *)
(* choose it; `global` entries are not asked and are always kept.            *)
Asked(entries) == Ordered(entries, { entry \in Elements(entries) : ~entry.global })

Kept(entry, keep) == entry.global \/ entry \in keep

(* §5.1 steps 1 and 2: the snapshot, then the filter; `global` entries are    *)
(* not asked and are kept.                                                    *)
Selected(entries, name, keep) ==
    Ordered(entries, { entry \in Elements(entries) : Matches(entry, name) /\ Kept(entry, keep) })

(* §6: one answer per asked entry, in registration order; a wrong number or a  *)
(* non-boolean fails the dispatch (`filter of <name> returned <n> answers for *)
(* <m> listeners`).                                                           *)
AnswersFit(answers, asked) ==
    Len(answers) = Len(asked) /\ \A i \in 1..Len(answers): answers[i] \in BOOLEAN

FilterFails(answers, asked) == ~AnswersFit(answers, asked)

(* Keep `global` entries whatever the answers say, and the asked ones whose    *)
(* answer is TRUE, in registration order.                                     *)
RECURSIVE Apply(_, _)
Apply(entries, answers) ==
    IF entries = <<>> THEN <<>>
    ELSE LET entry == Head(entries)
         IN IF entry.global
            THEN <<entry>> \o Apply(Tail(entries), answers)
            ELSE IF Head(answers)
                 THEN <<entry>> \o Apply(Tail(entries), Tail(answers))
                 ELSE Apply(Tail(entries), Tail(answers))

(* A failed filter keeps no listener: no call starts.                         *)
Filtered(entries, answers) ==
    IF FilterFails(answers, Asked(entries))
    THEN <<>>
    ELSE Apply(entries, answers)

-------------------------------------------------------------------------------
(* The five modes (§5.4)                                                     *)
-------------------------------------------------------------------------------

Modes == {"emit", "parallel", "serial", "bail", "waterfall"}

(* `waterfall_sync` is Rust's synchronous waterfall, not a protocol mode.     *)

StartsAll(mode) == mode \in {"emit", "parallel"}      \* each after the one before started
StartsAfterEnd(mode) == mode \in {"serial", "bail"}    \* the one before ended
StartsAfterNext(mode) == mode = "waterfall"            \* the one before called next
WaitsForEveryCall(mode) == mode \in {"parallel", "serial", "bail", "waterfall"}
RepliesAfterStarts(mode) == mode = "emit"
RepliesAfterAll(mode) == mode = "parallel"
RepliesFirstResult(mode) == mode \in {"serial", "bail"}
RepliesChainValue(mode) == mode = "waterfall"
AggregatesInOrder(mode) == mode = "parallel"
ReportsEachFailure(mode) == mode = "emit"
StopsAtFirstError(mode) == mode \in {"serial", "bail", "waterfall"}
CallsListenersSynchronously(mode) == mode = "bail"
FailsOnAsyncListener(mode) == mode = "bail"

(* §5.4: may the (started+1)-th call start? `ended` is how many calls ended,  *)
(* `nextCalled` how many listeners called next, `total` how many were        *)
(* selected. The first call of every mode starts at once.                    *)
MayStart(mode, started, ended, nextCalled, total) ==
    /\ started < total
    /\ \/ StartsAll(mode)
       \/ /\ StartsAfterEnd(mode)
          /\ started <= ended
       \/ /\ StartsAfterNext(mode)
          /\ started <= nextCalled

(* §5.4: what counts as "a result" (serial, bail, waterfall's terminal):      *)
(* not null / undefined / false; Rust `Some(false)` counts as no result.      *)
HasResult(value) ==
    value \notin {"null", "undefined", "false"}

-------------------------------------------------------------------------------
(* Export / import (§9)                                                      *)
-------------------------------------------------------------------------------

(* §9: an item of an export / import list is a name or a prefix.             *)
ItemMatches(item, name) ==
    IF item.prefix THEN IsPrefixOf(item.text, name) ELSE item.text = name

(* Two items share names.                                                     *)
Overlap(a, b) ==
    CASE a.prefix /\ b.prefix ->
             IsPrefixOf(a.text, b.text) \/ IsPrefixOf(b.text, a.text)
      [] a.prefix -> IsPrefixOf(a.text, b.text)
      [] b.prefix -> IsPrefixOf(b.text, a.text)
      [] OTHER -> a.text = b.text

(* A service item is always a name: prefixes are for events only.            *)
ServiceItem(item) == ~item.prefix

(* §9: what the importing side announces for one of its entries is narrowed   *)
(* to what it and the importing item share: two prefixes -> the longer, one   *)
(* prefix and one name -> the name, nothing -> no announcement (`NoItem`).    *)
Narrow(item, imp) ==
    CASE item.prefix /\ imp.prefix ->
             IF IsPrefixOf(item.text, imp.text) THEN imp
             ELSE IF IsPrefixOf(imp.text, item.text) THEN item
             ELSE NoItem
      [] item.prefix ->
             IF IsPrefixOf(item.text, imp.text) THEN imp ELSE NoItem
      [] imp.prefix ->
             IF IsPrefixOf(imp.text, item.text) THEN item ELSE NoItem
      [] OTHER -> IF item.text = imp.text THEN item ELSE NoItem

Narrowed(items, imports) ==
    { Narrow(item, imp) : item \in items, imp \in imports } \ {NoItem}

(* §9: the receiving side takes an announcement if its own export list picks  *)
(* any of the announced names.                                              *)
Accepted(export, item) ==
    \E one \in export: Overlap(one, item)

-------------------------------------------------------------------------------
(* Namespaces (§4.2, §8)                                                     *)
-------------------------------------------------------------------------------

IsInternal(name) == IsPrefixOf("internal/", name)

IsReserved(name) == IsPrefixOf("rutis/", name)

(* §4.2: a runtime may not register a listener of `internal/*`.              *)
RegistrationAllowed(name) == ~IsInternal(name)

(* §8: only the host emits `rutis/*`.                                        *)
EmittableByPlugin(name) == ~IsReserved(name)

-------------------------------------------------------------------------------
(* Announcement ids (§9)                                                     *)
-------------------------------------------------------------------------------

(* An announced id is `<epoch>.<n>`: the number of the peer's current offer    *)
(* of `events`, and a counter within it. A node of another implementation may  *)
(* announce a bare `<n>`; the models write both as <<epoch, n>>, where epoch   *)
(* 0 means "says no epoch", so its entries are replaced by id only.           *)
AnnouncedEpoch(id) == id[1]

HasEpoch(id) == id[1] > 0

SameId(a, b) == a = b
======================================================================================
