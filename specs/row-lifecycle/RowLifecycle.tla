--------------------------- MODULE RowLifecycle ----------------------------
(***************************************************************************)
(* The rows of a runtime session across a break and a reconnect of the     *)
(* same id, and -- with the constants below switched on -- the host        *)
(* loader's generations, its two-stage resolution and release, service     *)
(* announcements, refusals and versions, host service leases, a runtime's  *)
(* crash and restart, rows waiting for their dependencies, two runtimes    *)
(* that do not wait for each other, a refresh that is interrupted and      *)
(* redone, a volatile update committed in place and a row that ends while  *)
(* it loads.                                                               *)
(*                                                                         *)
(* Design: docs/design-remote-session-2026-10-28.md §4.2 (a replacement    *)
(* unloads the old session first) and §4.3 (rows on a session's far end);  *)
(* docs/design-remote-plugins-2026-10-03.md §4.4 (close the old session,   *)
(* stop reading its frames, clean its lease, answer the new hello);        *)
(* docs/design-multilang-runtimes-2026-10-03.md §四、§五、§六 (two-stage    *)
(* release of a runtime's rows, services by name, host service leases);    *)
(* docs/formal-trace.md §3.0 (the session and row lifecycle).              *)
(* Implementation: crates/rutis-loader/src/runtime.rs (the row's            *)
(* generation key, Refusals, leases, projection),                          *)
(* crates/rutis-loader/src/runtime/sessions.rs (the two-stage release:     *)
(* wait, re-resolve, provide the rows' service),                           *)
(* crates/rutis-bridge/src/runtime/process.rs (`rows.ended`, `rows.failed`,*)
(* `unload_row`, the host service table, `Imports::update`'s versions),    *)
(* crates/rutis-bridge/src/runtime/projection.rs (`withdraw` waits for the *)
(* publication under way),                                                 *)
(* crates/rutis-bridge/src/session/plugin/rows.rs (`Server::unload` is     *)
(* idempotent; `follow` reports one ending), and the runtime's row table   *)
(* (node/rutis-runtime/src/runner.mjs: `unloadRow`, `endRow`, `update`).   *)
(* README.md next to this file lists the simplifications.                  *)
(*                                                                         *)
(* Connection s is the host's s-th dial to a runtime. The host loads its   *)
(* rows through the session it has with the row's runtime; a runtime       *)
(* serves one session at a time and, when a new hello comes, stops reading *)
(* the old session and only answers the new one once the old session's     *)
(* rows are gone. Each direction of a connection is FIFO and delayed       *)
(* arbitrarily long; a side that closed a connection reads nothing more    *)
(* on it.                                                                  *)
(*                                                                         *)
(* The features are off unless a configuration turns them on:              *)
(*   TwoStages     the runtime provides `<language>Runtime`, the host      *)
(*                 re-resolves its rows, then it provides                  *)
(*                 `<language>RuntimeRows` and the rows may load.          *)
(*   Services      rows announce the names they provide; a second live     *)
(*                 provider of a name is refused (a refusal fails one      *)
(*                 restart, its own).                                      *)
(*   Depends       a row waits for the services it injects, and stops      *)
(*                 when one goes away.                                     *)
(*   HostServices  a row leases the host services it injects; its leases   *)
(*                 are released by its cleanup, and the next generation    *)
(*                 waits for that.                                         *)
(*   Crashes       a runtime process dies with its session; the host may   *)
(*                 start it again (Restarts) and re-resolve its rows.      *)
(*   Runtimes      more than one runtime; each has its own sessions,       *)
(*                 stages and rows, and none waits for another.            *)
(*   Versions      a row's services carry a version (the slot counter): a  *)
(*                 `provide` the host learns is applied only when newer,   *)
(*                 so an older announcement never overrides a newer one; a *)
(*                 row may provide a name again (a service update); its    *)
(*                 cleanup withdraws the names at their next version; the  *)
(*                 host publishes an announced name in a step of its own   *)
(*                 (the projection), and the cleanup waits for that        *)
(*                 publication before it withdraws.                        *)
(*   HostFrames    `hosts.provide` / `hosts.withdraw` are frames with      *)
(*                 lease counts: the first lease of a name provides it,    *)
(*                 the last release withdraws it, and the runtime holds    *)
(*                 the shape of the dispatch that provided it.             *)
(*   PreciseRefusal  a refusal is recorded for the row's fiber and fails   *)
(*                 only the apply of the restart it caused; a restart      *)
(*                 that waits for a dependency instead applies later and   *)
(*                 is not failed by that refusal.                          *)
(*   LoadSteps     a load is read (`applying`), then answered; a row whose *)
(*                 plugin disposes itself meanwhile reports its ending     *)
(*                 only after the load's reply.                            *)
(*   Refreshes     the host's re-resolution of a runtime's rows runs as a  *)
(*                 task of its own: a runtime restart cancels it and it is *)
(*                 redone (REM-4.3-3).                                     *)
(*   Updates       `rows.update`: a loaded row's volatile fields change in *)
(*                 place, on the generation it runs (SYN-4-8).             *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS
    Rows,          \* the host's rows on the runtimes ("a", "b", ...)
    MaxConns,      \* connections the host may make to one runtime (1 + reconnects)
    MaxLoads,      \* loads of one row in one session (1 + restarts)
    Faults,        \* what the environment may do: "lose"
    Failures,      \* a loaded row may fail (rows.failed)
    SelfEnds,      \* a loaded row may end on its own (rows.ended)
    Mutation,      \* "none", or a deliberate bug (README, "Mutations")
    RowSet2,       \* the rows on runtime "r2" (empty: one runtime)
    TwoStages,     \* the runtime's two stages and the host's re-resolution
    ServiceName,   \* the service a row provides or injects ("none": none)
    ProviderRows,  \* the rows that provide ServiceName
    ConsumerRows,  \* the rows that inject ServiceName
    GateName,      \* a second service name, for a refused row that waits
    GateProviders, \* the rows that provide GateName
    GateConsumers, \* the rows that inject GateName
    HostServiceName, \* the host service rows lease ("none": none)
    LeaseRows,     \* the rows that inject HostServiceName
    Crashes,       \* a runtime process may crash
    CrashLimit,    \* crashes of one runtime
    CrashR1,       \* runtime "r1" is one that may crash
    Restarts,      \* starts of one runtime process (1 = no restart)
    LoadFaults,    \* a row's load may fail in the runtime
    Versions,      \* a row's services carry versions; publications are steps
    MaxProvides,   \* provides of one name by one generation (Versions)
    HostFrames,    \* hosts.provide / hosts.withdraw with lease counts
    AsyncHosts,    \* the host services whose methods are asynchronous
    AsyncRows,     \* the rows whose services' methods are asynchronous
    PreciseRefusal,\* a refusal fails only the restart it caused
    LoadSteps,     \* a load is read, applies, then replies
    Refreshes,     \* a refresh may be interrupted and redone
    Updates,       \* rows.update commits in place
    MaxUpdates     \* updates of one generation (Updates)

ASSUME Rows /= {}
ASSUME MaxConns \in Nat \ {0}
ASSUME MaxLoads \in Nat \ {0}
ASSUME Faults \subseteq {"lose"}
ASSUME Failures \in BOOLEAN
ASSUME SelfEnds \in BOOLEAN
ASSUME Mutation \in {"none", "acceptBeforeClean", "readOld", "keyedByRow",
                     "reclean", "keepRows", "loadProvisional",
                     "announceAfterLoad", "allowClash", "leaseLeak",
                     "keepOnDepGone", "mutualWait", "refusalSticky",
                     "regardlessOfVersion", "loseWithdrawal",
                     "leaseOrphan", "endedDuringLoad", "noRefreshRedo",
                     "updateReload"}
ASSUME RowSet2 \subseteq Rows
ASSUME ProviderRows \subseteq Rows
ASSUME ConsumerRows \subseteq Rows
ASSUME GateProviders \subseteq Rows
ASSUME GateConsumers \subseteq Rows
ASSUME LeaseRows \subseteq Rows
ASSUME Crashes \in BOOLEAN
ASSUME CrashR1 \in BOOLEAN
ASSUME Restarts \in Nat \ {0}
ASSUME CrashLimit \in Nat
ASSUME LoadFaults \in BOOLEAN
ASSUME TwoStages \in BOOLEAN
ASSUME Versions \in BOOLEAN
ASSUME MaxProvides \in Nat \ {0}
ASSUME HostFrames \in BOOLEAN
ASSUME AsyncHosts \subseteq {HostServiceName}
ASSUME AsyncRows \subseteq Rows
ASSUME PreciseRefusal \in BOOLEAN
ASSUME LoadSteps \in BOOLEAN
ASSUME Refreshes \in BOOLEAN
ASSUME Updates \in BOOLEAN
ASSUME MaxUpdates \in Nat \ {0}
ASSUME HostFrames => HostServiceName /= "none"

\* The configuration names the parts; the model works with the sets.
Runtimes == IF RowSet2 = {} THEN {"r1"} ELSE {"r1", "r2"}
Home(r) == IF r \in RowSet2 THEN "r2" ELSE "r1"
CrashRuntimes == IF CrashR1 THEN {"r1"} ELSE {}
Services == (IF ServiceName = "none" THEN {} ELSE {ServiceName})
            \cup (IF GateName = "none" THEN {} ELSE {GateName})
Provides == {<<r, ServiceName>> : r \in ProviderRows}
            \cup {<<r, GateName>> : r \in GateProviders}
Depends == {<<r, ServiceName>> : r \in ConsumerRows}
           \cup {<<r, GateName>> : r \in GateConsumers}
HostServices == IF HostServiceName = "none" THEN {} ELSE {HostServiceName}
Leases == IF HostServiceName = "none" THEN {}
          ELSE {<<r, HostServiceName>> : r \in LeaseRows}
\* The methods a row's services have, and a host service's.
ShapeOf(r) == IF r \in AsyncRows THEN "async" ELSE "sync"
HostShapeOf(h) == IF h \in AsyncHosts THEN "async" ELSE "sync"

Sessions == 1..MaxConns
Keys == {k \in Runtimes \X Sessions \X Rows \X (1..MaxLoads) :
            Home(k[3]) = k[1]}
NoKey == <<"none", 0, "none", 0>>
NoKey2 == <<NoKey, 0>>
RtOf(k) == k[1]
SessOf(k) == k[2]
RowOf(k) == k[3]
GenOf(k) == k[4]
RowsOfRuntime(rt) == {r \in Rows : Home(r) = rt}
NamesOf(r) == {n \in Services : <<r, n>> \in Provides}
DepsOf(r) == {n \in Services : <<r, n>> \in Depends}
LeasesOf(r) == {h \in HostServices : <<r, h>> \in Leases}

\* The frames of a row's announcements, in some order (sets are unordered).
RECURSIVE AnnounceFrames(_, _)
AnnounceFrames(k, names) ==
    IF names = {} THEN <<>>
    ELSE LET n == CHOOSE x \in names : TRUE
         IN << <<"announce", k, n>> >> \o AnnounceFrames(k, names \ {n})

\* The host services a row takes when it loads, and gives back when it is
\* gone: the first lease provides the name (`hosts.provide`), the last
\* release withdraws it (`hosts.withdraw`). With `HostFrames` off the host
\* keeps only which names each generation leases.
LeasedAfter(hs, names) ==
    [h \in HostServices |->
        IF h \in names
        THEN [reg |-> TRUE,
              count |-> hs[h].count + 1,
              shape |-> IF hs[h].reg THEN hs[h].shape ELSE HostShapeOf(h),
              provides |-> hs[h].provides + (IF hs[h].reg THEN 0 ELSE 1),
              withdraws |-> hs[h].withdraws]
        ELSE hs[h]]

\* Give back `counts[h]` leases of each name.
ReleasedBy(hs, counts) ==
    [h \in HostServices |->
        IF counts[h] > 0 /\ hs[h].count > 0
        THEN LET left == hs[h].count - (IF counts[h] < hs[h].count
                                        THEN counts[h] ELSE hs[h].count)
             IN [reg |-> left > 0,
                 count |-> left,
                 shape |-> hs[h].shape,
                 provides |-> hs[h].provides,
                 withdraws |-> hs[h].withdraws
                              + (IF hs[h].reg /\ left = 0 THEN 1 ELSE 0)]
        ELSE hs[h]]

Released(hs, names) ==
    ReleasedBy(hs, [h \in HostServices |-> IF h \in names THEN 1 ELSE 0])

\* The same, for the lease a row takes and then loses before it gets its
\* `HostLease`: the count is left raised with no owner (the bug of
\* `Process::lease` dropped while it awaits `hosts.provide`).
Orphaned(hs, names) ==
    [h \in HostServices |->
        IF h \in names
        THEN [reg |-> hs[h].reg, count |-> hs[h].count + 1,
              shape |-> hs[h].shape, provides |-> hs[h].provides,
              withdraws |-> hs[h].withdraws]
        ELSE hs[h]]

\* The host's service table: the names a row's cleanup (or its session's
\* end) takes away leave at the name's next version, so a late `provide`
\* of an older version does not bring them back.
Dropped(svc, names) ==
    [n \in Services |->
        IF n \in names
        THEN [known |-> <<NoKey, svc[n].next>>,
              ever |-> svc[n].next,
              next |-> svc[n].next + 1]
        ELSE svc[n]]

VARIABLES
    hst,          \* per runtime, the host: "connecting" | "greeting" | "ready" | "waiting"
    hconn,        \* per runtime: the connection the host is on, 0 when none
    ndial,        \* per runtime: connections the host made
    hrow,         \* per row: "idle" | "loading" | "loaded" | "unloading" | "gone"
    hkey,         \* per row: the key of its current (or last) load
    hpending,     \* per row: the key its unload waits for
    hgen,         \* per row: loads made in the session the host is on
    hEndedFrom,   \* ghost: the key whose ending ended the row
    hFailedFrom,  \* ghost: the key whose failure restarted the row
    cur,          \* per runtime: the session it serves, 0 when none
    pick,         \* per runtime: the session whose hello it took up, awaiting the answer
    rside,        \* per runtime and session: "none" | "open" | "closed"
    answered,     \* per runtime and session: the runtime answered its hello
    ended,        \* per runtime and session: the runtime saw the session end
    loaded,       \* the runtime's rows, by key
    pendingEnding,\* rows whose `rows.ended` the runtime has not sent yet
    reportedFail,\* rows whose `rows.failed` the runtime already sent
    cleanCount,   \* ghost: how often a row's cleanup ran
    wasSwept,     \* ghost: the session was swept at least once
    lostByHost,   \* per runtime and session: the host lost it
    toR,          \* per runtime and session: frames to the runtime, FIFO
    toH,          \* per runtime and session: frames to the host, FIFO
    stage,        \* per runtime: "down" | "up" | "refreshing" | "rows"
    resolved,     \* per row: "provisional" (runtime offline) | "final"
    claimed,      \* per service name: the key whose service is published, or NoKey
    sentAnn,      \* per key: names the runtime announced while it loaded
    processedAnn, \* per key: names of them the host processed
    refused,      \* per key: the host refused its announcement (a name is taken)
    refusedAt,    \* per row: the key whose refusal fails the next restart, or NoKey
    refusalFails, \* ghost: restarts a refused key's refusal failed
    leaseHeld,    \* per key: host services it leases
    crashCount,   \* per runtime: crashes it had
    startCount,   \* per runtime: times its process started
    aux           \* the parts a configuration switches on (see below)

vars == <<hst, hconn, ndial, hrow, hkey, hpending, hgen, hEndedFrom,
          hFailedFrom, cur, pick, rside, answered, ended, loaded,
          pendingEnding, reportedFail, cleanCount, wasSwept, lostByHost,
          toR, toH, stage, resolved, claimed, sentAnn, processedAnn,
          refused, refusedAt, refusalFails, leaseHeld, crashCount, startCount,
          aux>>

\* aux.svc     per name: the host's table (`known`, the announcement it
\*             applied; `ever`, the newest version it applied; `next`, the
\*             next version)
\* aux.prov    per key and name: provides of this generation (`count`,
\*             `live`)
\* aux.pub     per row: a publication is under way
\* aux.applying per key: the runtime read the load and its apply runs
\* aux.replied per key: the load's reply was sent
\* aux.endedSent per key: the ending was sent
\* aux.refusal per row: `armed` (a refusal waits for its restart), `clash`
\*             (refusals recorded), `failed` (the clash whose refusal
\*             failed an apply), `waited` (the clash after which it waited)
\* aux.updated per key: updates committed in place
\* aux.hosts   per host service: `reg`, `count`, `shape`, `provides`,
\*             `withdraws`
\* aux.refresh per runtime: "none" | "running" | "cancelled"

RowsOf(rt, s) == {k \in loaded : RtOf(k) = rt /\ SessOf(k) = s}
\* Key k is the generation its row runs (or loads) now.
LiveKey(k) == hkey[RowOf(k)] = k /\ hrow[RowOf(k)] \in {"loading", "loaded"}

\* The claims left when key k's names go away (its row unloaded), when
\* runtime rt's do (it crashed), or when session (rt, s)'s do (it broke).
ClaimsWithoutKey(k) ==
    [n \in Services |-> IF claimed[n] = k THEN NoKey ELSE claimed[n]]
ClaimsWithoutRuntime(rt) ==
    [n \in Services |->
        IF claimed[n] # NoKey /\ RtOf(claimed[n]) = rt THEN NoKey
        ELSE claimed[n]]
ClaimsWithoutSession(rt, s) ==
    [n \in Services |->
        IF claimed[n] # NoKey /\ RtOf(claimed[n]) = rt
           /\ SessOf(claimed[n]) = s
        THEN NoKey ELSE claimed[n]]
\* The names of a table that those claims were taken from (Versions).
NamesOfClaims(claims) == {n \in Services : claims[n] = NoKey /\ claimed[n] # NoKey}

\* The loaded rows that lose a dependency under `claims`: the host stops
\* them as the service goes, as the loader's gating does.
Starved(claims) ==
    IF Mutation = "keepOnDepGone" THEN {}
    ELSE {r \in Rows : hrow[r] \in {"loading", "loaded"}
                       /\ \E n \in DepsOf(r) : claims[n] = NoKey}

\* The names a row's generation k still has in the host's table or in
\* flight: its cleanup takes them away at their next version.
NamesDropped(k) ==
    IF Versions /\ Mutation = "loseWithdrawal" /\ aux.pub[RowOf(k)]
    THEN {}
    ELSE {n \in Services : claimed[n] = k} \cup sentAnn[k]

\* The names a row's generation provides that the host has not published at
\* their announcement's version.
PendingOf(r) ==
    IF hkey[r] = NoKey THEN {}
    ELSE {n \in Services : aux.prov[hkey[r]][n].live
                          /\ aux.prov[hkey[r]][n].v > aux.prov[hkey[r]][n].pub}

\* The unload frames for the rows of `rows` that are on runtime rt's
\* current session s.
RECURSIVE FramesFor(_)
FramesFor(rows) ==
    IF rows = {} THEN <<>>
    ELSE LET r == CHOOSE x \in rows : TRUE
         IN << <<"unload", hkey[r]>> >> \o FramesFor(rows \ {r})
FramesOn(rt, s, rows) ==
    FramesFor({r \in rows : Home(r) = rt /\ hconn[rt] = s})

Init ==
    /\ hst = [rt \in Runtimes |-> "connecting"]
    /\ hconn = [rt \in Runtimes |-> 0]
    /\ ndial = [rt \in Runtimes |-> 0]
    /\ hrow = [r \in Rows |-> "idle"]
    /\ hkey = [r \in Rows |-> NoKey]
    /\ hpending = [r \in Rows |-> NoKey]
    /\ hgen = [r \in Rows |-> 0]
    /\ hEndedFrom = [r \in Rows |-> NoKey]
    /\ hFailedFrom = [r \in Rows |-> NoKey]
    /\ cur = [rt \in Runtimes |-> 0]
    /\ pick = [rt \in Runtimes |-> 0]
    /\ rside = [rt \in Runtimes |-> [s \in Sessions |-> "none"]]
    /\ answered = [rt \in Runtimes |-> [s \in Sessions |-> FALSE]]
    /\ ended = [rt \in Runtimes |-> [s \in Sessions |-> FALSE]]
    /\ loaded = {}
    /\ pendingEnding = {}
    /\ reportedFail = {}
    /\ cleanCount = [k \in Keys |-> 0]
    /\ wasSwept = [rt \in Runtimes |-> [s \in Sessions |-> FALSE]]
    /\ lostByHost = [rt \in Runtimes |-> [s \in Sessions |-> FALSE]]
    /\ toR = [rt \in Runtimes |-> [s \in Sessions |-> <<>>]]
    /\ toH = [rt \in Runtimes |-> [s \in Sessions |-> <<>>]]
    /\ stage = [rt \in Runtimes |-> IF TwoStages THEN "down" ELSE "rows"]
    /\ resolved = [r \in Rows |-> IF TwoStages THEN "provisional" ELSE "final"]
    /\ claimed = [n \in Services |-> NoKey]
    /\ sentAnn = [k \in Keys |-> {}]
    /\ processedAnn = [k \in Keys |-> {}]
    /\ refused = [k \in Keys |-> FALSE]
    /\ refusedAt = [r \in Rows |-> NoKey]
    /\ refusalFails = [k \in Keys |-> 0]
    /\ leaseHeld = [k \in Keys |-> {}]
    /\ crashCount = [rt \in Runtimes |-> 0]
    /\ startCount = [rt \in Runtimes |-> 0]
    /\ aux = [svc |-> [n \in Services |->
                          [known |-> NoKey2, ever |-> 0, next |-> 1]],
              prov |-> [k \in Keys |->
                          [n \in Services |-> [count |-> 0, live |-> FALSE,
                                               v |-> 0, pub |-> 0]]],
              pub |-> [r \in Rows |-> FALSE],
              applying |-> {},
              replied |-> {},
              endedSent |-> {},
              refusal |-> [r \in Rows |->
                             [armed |-> FALSE, clash |-> 0, failed |-> 0,
                              waited |-> 0]],
              updated |-> [k \in Keys |-> 0],
              hosts |-> [h \in HostServices |->
                           [reg |-> FALSE, count |-> 0, shape |-> "none",
                            provides |-> 0, withdraws |-> 0]],
              refresh |-> [rt \in Runtimes |-> "none"]]

(***************************************************************************)
(* The runtime process: the two stages, and the refresh between them.      *)
(***************************************************************************)

\* The process starts: it provides `<language>Runtime`. Its rows are still
\* resolved provisionally (their inject list is only readable once the
\* runtime is there).
StartRuntime(rt) ==
    /\ TwoStages
    /\ stage[rt] = "down"
    /\ startCount[rt] < Restarts
    /\ stage' = [stage EXCEPT ![rt] = "up"]
    /\ startCount' = [startCount EXCEPT ![rt] = @ + 1]
    /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen, hEndedFrom,
                   hFailedFrom, cur, pick, rside, answered, ended, loaded,
                   pendingEnding, reportedFail, cleanCount, wasSwept, lostByHost,
                   toR, toH, resolved, claimed, sentAnn, processedAnn, refused,
                   refusedAt, refusalFails, leaseHeld, crashCount, aux>>

\* The host re-resolved the rows on this runtime (its `reload`), then the
\* runtime provides `<language>RuntimeRows`: they may load. One step, when
\* the refresh is not a task of its own (`~Refreshes`).
Refresh(rt) ==
    /\ ~Refreshes
    /\ TwoStages
    /\ stage[rt] = "up"
    /\ stage' = [stage EXCEPT ![rt] = "rows"]
    /\ resolved' = [r \in Rows |->
                      IF Home(r) = rt THEN "final" ELSE resolved[r]]
    /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen, hEndedFrom,
                   hFailedFrom, cur, pick, rside, answered, ended, loaded,
                   pendingEnding, reportedFail, cleanCount, wasSwept, lostByHost,
                   toR, toH, claimed, sentAnn, processedAnn, refused, refusedAt,
                   refusalFails, leaseHeld, crashCount, startCount, aux>>

\* The refresh task starts (REM-4.3-3: its own task, not a step inside the
\* reconcile that is already running). One a runtime restart cancelled is
\* started again -- unless the loader never redoes it.
RefreshBegin(rt) ==
    /\ Refreshes
    /\ TwoStages
    /\ stage[rt] = "up"
    /\ (Mutation # "noRefreshRedo" \/ aux.refresh[rt] # "cancelled")
    /\ stage' = [stage EXCEPT ![rt] = "refreshing"]
    /\ aux' = [aux EXCEPT !.refresh = [aux.refresh EXCEPT ![rt] = "running"]]
    /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen, hEndedFrom,
                   hFailedFrom, cur, pick, rside, answered, ended, loaded,
                   pendingEnding, reportedFail, cleanCount, wasSwept, lostByHost,
                   toR, toH, resolved, claimed, sentAnn, processedAnn, refused,
                   refusedAt, refusalFails, leaseHeld, crashCount, startCount>>

\* The refresh task completes: the rows' declarations are complete, the
\* runtime provides `<language>RuntimeRows` and the rows may load.
RefreshDone(rt) ==
    /\ Refreshes
    /\ stage[rt] = "refreshing"
    /\ stage' = [stage EXCEPT ![rt] = "rows"]
    /\ resolved' = [r \in Rows |->
                      IF Home(r) = rt THEN "final" ELSE resolved[r]]
    /\ aux' = [aux EXCEPT !.refresh = [aux.refresh EXCEPT ![rt] = "none"]]
    /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen, hEndedFrom,
                   hFailedFrom, cur, pick, rside, answered, ended, loaded,
                   pendingEnding, reportedFail, cleanCount, wasSwept, lostByHost,
                   toR, toH, claimed, sentAnn, processedAnn, refused, refusedAt,
                   refusalFails, leaseHeld, crashCount, startCount>>

(***************************************************************************)
(* The host: its loader's rows and its dial row, per runtime.              *)
(***************************************************************************)

HDial(rt) ==
    /\ hst[rt] \in {"connecting", "waiting"}
    /\ stage[rt] # "down"
    /\ ndial[rt] < MaxConns
    /\ LET n == ndial[rt] + 1 IN
       /\ ndial' = [ndial EXCEPT ![rt] = n]
       /\ hconn' = [hconn EXCEPT ![rt] = n]
       /\ hst' = [hst EXCEPT ![rt] = "greeting"]
       /\ hgen' = [r \in Rows |-> IF Home(r) = rt THEN 0 ELSE hgen[r]]
       /\ rside' = [rside EXCEPT ![rt][n] = "open"]
       /\ toR' = [toR EXCEPT ![rt][n] = Append(@, <<"hello", NoKey>>)]
       /\ UNCHANGED <<hrow, hkey, hpending, hEndedFrom, hFailedFrom, cur,
                      pick, answered, ended, loaded, pendingEnding,
                      reportedFail, cleanCount, wasSwept, lostByHost, toH,
                      stage, resolved, claimed, sentAnn, processedAnn,
                      refused, refusedAt, refusalFails, leaseHeld, crashCount,
                      startCount, aux>>

\* The runtime's hello: the session is up and may run rows.
HGreeted(rt) ==
    /\ hst[rt] = "greeting"
    /\ hconn[rt] > 0
    /\ toH[rt][hconn[rt]] /= <<>>
    /\ LET f == Head(toH[rt][hconn[rt]]) IN
       /\ f[1] = "hello"
       /\ hst' = [hst EXCEPT ![rt] = "ready"]
       /\ toH' = [toH EXCEPT ![rt][hconn[rt]] = Tail(@)]
       /\ UNCHANGED <<hconn, ndial, hrow, hkey, hpending, hgen, hEndedFrom,
                      hFailedFrom, cur, pick, rside, answered, ended, loaded,
                      pendingEnding, reportedFail, cleanCount, wasSwept,
                      lostByHost, toR, stage, resolved, claimed, sentAnn,
                      processedAnn, refused, refusedAt, refusalFails,
                      leaseHeld, crashCount, startCount, aux>>

\* The loader loads row r: a fresh key (the generation) per load. The row
\* only loads once its runtime finished the two stages and its resolution
\* is final; it leases the host services it injects as it starts. A row
\* whose refusal waits for the restart it caused loads no new generation
\* before that restart has run (HLoadRefused).
HLoad(r) ==
    /\ LET rt == Home(r) IN
       /\ hst[rt] = "ready"
       /\ hconn[rt] > 0
       /\ hrow[r] = "idle"
       /\ hgen[r] < MaxLoads
       /\ ((stage[rt] = "rows" /\ resolved[r] = "final")
           \/ Mutation = "loadProvisional")
       /\ refusedAt[r] = NoKey
       /\ (PreciseRefusal => ~aux.refusal[r].armed)
       /\ \A n \in DepsOf(r) : claimed[n] # NoKey
       /\ LET k == <<rt, hconn[rt], r, hgen[r] + 1>> IN
          /\ hgen' = [hgen EXCEPT ![r] = @ + 1]
          /\ hkey' = [hkey EXCEPT ![r] = k]
          /\ hpending' = [hpending EXCEPT ![r] = NoKey]
          /\ hrow' = [hrow EXCEPT ![r] = "loading"]
          /\ hEndedFrom' = [hEndedFrom EXCEPT ![r] = NoKey]
          /\ hFailedFrom' = [hFailedFrom EXCEPT ![r] = NoKey]
          /\ toR' = [toR EXCEPT
                       ![rt][hconn[rt]] = Append(@, <<"load", k>>)]
          \* The leases: taken now, before the runtime's row is there.
          /\ \/ /\ leaseHeld' = [leaseHeld EXCEPT ![k] = LeasesOf(r)]
                /\ aux' = [aux EXCEPT
                             !.hosts = IF HostFrames
                                       THEN LeasedAfter(aux.hosts, LeasesOf(r))
                                       ELSE aux.hosts]
             \/ Mutation = "leaseOrphan"
                /\ HostFrames
                /\ leaseHeld' = leaseHeld
                /\ aux' = [aux EXCEPT !.hosts = Orphaned(aux.hosts, LeasesOf(r))]
          /\ UNCHANGED <<hst, hconn, ndial, cur, pick, rside, answered, ended,
                         loaded, pendingEnding, reportedFail, cleanCount,
                         wasSwept, lostByHost, toH, stage, resolved, claimed,
                         sentAnn, processedAnn, refused, refusedAt,
                         refusalFails, crashCount, startCount>>

\* The load's reply. A refused row (a name it announces is taken) fails
\* instead: its cleanup unloads it, and the refusal fails its next restart
\* once.
HLoaded(rt, s) ==
    /\ toH[rt][s] /= <<>>
    /\ LET f == Head(toH[rt][s]) IN
       /\ f[1] = "loaded"
       /\ LET k == f[2] r == RowOf(k) IN
          /\ (Versions => PendingOf(r) = {})
          /\ toH' = [toH EXCEPT ![rt][s] = Tail(@)]
          /\ IF refused[k]
             THEN /\ hrow' = [hrow EXCEPT ![r] = "unloading"]
                  /\ hpending' = [hpending EXCEPT ![r] = k]
                  /\ hFailedFrom' = [hFailedFrom EXCEPT ![r] = k]
                  /\ toR' = [toR EXCEPT ![rt][s] = Append(@, <<"unload", k>>)]
                  /\ UNCHANGED <<hkey, hgen, hEndedFrom, leaseHeld, aux>>
             ELSE /\ hrow' = IF hrow[r] = "loading" /\ hkey[r] = k
                             THEN [hrow EXCEPT ![r] = "loaded"] ELSE hrow
                  /\ UNCHANGED <<hpending, hFailedFrom, toR, hkey, hgen,
                                 hEndedFrom, leaseHeld, aux>>
          /\ UNCHANGED <<hst, hconn, ndial, cur, pick, rside, answered, ended,
                         loaded, pendingEnding, reportedFail, cleanCount,
                         wasSwept, lostByHost, stage, resolved, claimed,
                         sentAnn, processedAnn, refused, refusedAt,
                         refusalFails, crashCount, startCount>>

\* The runtime unloaded the row: its cleanup releases the host services it
\* leased, and the restart may load a new generation only after that. Its
\* names leave the host's table before that (Versions): a publication that
\* is under way is waited for first, so the withdrawal is not lost.
HUnloaded(rt, s) ==
    /\ toH[rt][s] /= <<>>
    /\ LET f == Head(toH[rt][s]) IN
       /\ f[1] = "unloaded"
       /\ LET k == f[2] r == RowOf(k)
              waiting == hpending[r] = k
              claims == ClaimsWithoutKey(k)
              starved == Starved(claims)
              released == IF Mutation = "leaseLeak" THEN leaseHeld
                          ELSE [leaseHeld EXCEPT ![k] = {}]
              dropped == Dropped(aux.svc, NamesDropped(k))
          IN /\ (Mutation = "loseWithdrawal" \/ ~aux.pub[r])
             /\ toH' = [toH EXCEPT ![rt][s] = Tail(@)]
             /\ claimed' = claims
             /\ hrow' = [x \in Rows |->
                           IF x \in starved THEN "unloading"
                           ELSE IF waiting /\ x = r /\ hrow[x] = "unloading"
                                THEN "idle"
                           ELSE hrow[x]]
             /\ hpending' = [x \in Rows |->
                               IF x \in starved THEN hkey[x]
                               ELSE IF waiting /\ x = r THEN NoKey
                               ELSE hpending[x]]
             /\ hFailedFrom' = [x \in Rows |->
                                  IF x \in starved THEN hkey[x]
                                  ELSE hFailedFrom[x]]
             /\ leaseHeld' = released
             /\ toR' = [rt2 \in Runtimes |->
                           [s2 \in Sessions |->
                               toR[rt2][s2] \o FramesOn(rt2, s2, starved)]]
             /\ aux' = [aux EXCEPT
                          !.svc = IF Versions THEN dropped ELSE aux.svc,
                          !.hosts = IF HostFrames /\ Mutation # "leaseLeak"
                                    THEN Released(aux.hosts, LeasesOf(r))
                                    ELSE aux.hosts]
             /\ UNCHANGED <<hst, hconn, ndial, hkey, hgen, hEndedFrom,
                            cur, pick, rside, answered, ended, loaded,
                            pendingEnding, reportedFail, cleanCount, wasSwept,
                            lostByHost, stage, resolved, sentAnn,
                            processedAnn, refused, refusedAt, refusalFails,
                            crashCount, startCount>>

\* The runtime could not load the row. Its cleanup releases the lease and
\* the row may try again.
HLoadFailed(rt, s) ==
    /\ toH[rt][s] /= <<>>
    /\ LET f == Head(toH[rt][s]) IN
       /\ f[1] = "loadfailed"
       /\ LET k == f[2] r == RowOf(k) IN
          /\ toH' = [toH EXCEPT ![rt][s] = Tail(@)]
          /\ hrow' = IF hrow[r] = "loading" /\ hkey[r] = k
                     THEN [hrow EXCEPT ![r] = "idle"] ELSE hrow
          /\ hFailedFrom' = IF hrow[r] = "loading" /\ hkey[r] = k
                            THEN [hFailedFrom EXCEPT ![r] = k]
                            ELSE hFailedFrom
          /\ IF Mutation = "leaseLeak"
             THEN UNCHANGED leaseHeld
             ELSE leaseHeld' = [leaseHeld EXCEPT ![k] = {}]
          /\ aux' = [aux EXCEPT
                       !.svc = IF Versions
                               THEN IF Mutation = "loseWithdrawal"
                                       /\ aux.pub[r]
                                    THEN aux.svc
                                    ELSE Dropped(aux.svc, sentAnn[k])
                               ELSE aux.svc,
                       !.hosts = IF HostFrames /\ Mutation # "leaseLeak"
                                 THEN Released(aux.hosts, LeasesOf(r))
                                 ELSE aux.hosts]
          /\ UNCHANGED <<hst, hconn, ndial, hkey, hpending, hgen, hEndedFrom,
                         cur, pick, rside, answered, ended, loaded,
                         pendingEnding, reportedFail, cleanCount, wasSwept,
                         lostByHost, toR, stage, resolved, claimed, sentAnn,
                         processedAnn, refused, refusedAt, refusalFails,
                         crashCount, startCount>>

\* The host applied one service announcement of row k (without versions).
\* A name another live key announced is refused: the later row fails, not
\* the earlier one.
HAnnounce(rt, s) ==
    /\ ~Versions
    /\ toH[rt][s] /= <<>>
    /\ LET f == Head(toH[rt][s]) IN
       /\ f[1] = "announce"
       /\ LET k == f[2] n == f[3] r == RowOf(k)
              other == claimed[n]
              clash == other # NoKey /\ other # k
                       /\ Mutation # "allowClash"
          IN /\ toH' = [toH EXCEPT ![rt][s] = Tail(@)]
             /\ processedAnn' = [processedAnn EXCEPT ![k] = @ \cup {n}]
             /\ aux' = [aux EXCEPT
                          !.refusal = IF PreciseRefusal /\ clash
                                      THEN [aux.refusal EXCEPT
                                              ![r] = [armed |-> TRUE,
                                                      clash |-> aux.refusal[r].clash + 1,
                                                      failed |-> aux.refusal[r].failed,
                                                      waited |-> aux.refusal[r].waited]]
                                      ELSE aux.refusal]
             /\ IF clash
                THEN /\ claimed' = claimed
                     /\ refused' = [refused EXCEPT ![k] = TRUE]
                     /\ refusedAt' = [refusedAt EXCEPT ![r] = k]
                     \* The load is already marked running (the announce came
                     \* after the reply): stop it now.
                     /\ IF hrow[r] = "loaded"
                        THEN /\ hrow' = [hrow EXCEPT ![r] = "unloading"]
                             /\ hpending' = [hpending EXCEPT ![r] = k]
                             /\ hFailedFrom' = [hFailedFrom EXCEPT ![r] = k]
                             /\ toR' = [toR EXCEPT
                                          ![rt][s] = Append(@, <<"unload", k>>)]
                             /\ UNCHANGED <<hkey, hgen, hEndedFrom, leaseHeld>>
                        ELSE /\ UNCHANGED <<hrow, hpending, hFailedFrom,
                                            toR, hkey, hgen, hEndedFrom,
                                            leaseHeld>>
                ELSE /\ claimed' = [claimed EXCEPT ![n] = k]
                     /\ UNCHANGED <<refused, refusedAt, hrow, hpending,
                                    hFailedFrom, toR, hkey, hgen, hEndedFrom,
                                    leaseHeld>>
             /\ UNCHANGED <<hst, hconn, ndial, cur, pick, rside, answered,
                            ended, loaded, pendingEnding, reportedFail,
                            cleanCount, wasSwept, lostByHost, stage, resolved,
                            sentAnn, refusalFails, crashCount, startCount>>

\* The host applied one `provide` of row k, at the name's version v: an
\* announcement older than the one the table holds is ignored (the
\* mutation applies it all the same).
HProvide(rt, s) ==
    /\ Versions
    /\ toH[rt][s] /= <<>>
    /\ Head(toH[rt][s])[1] = "provide"
    /\ LET f == Head(toH[rt][s]) IN
       /\ LET k == f[2] n == f[3] v == f[4]
              newer == v > aux.svc[n].known[2]
                       \/ Mutation = "regardlessOfVersion"
          IN /\ toH' = [toH EXCEPT ![rt][s] = Tail(@)]
             /\ processedAnn' = [processedAnn EXCEPT
                                   ![k] = @ \cup {n}]
             /\ aux' = IF newer
                       THEN [aux EXCEPT
                               !.svc = [aux.svc EXCEPT
                                          ![n] = [known |-> <<k, v>>,
                                                  ever |-> IF v > aux.svc[n].ever
                                                           THEN v
                                                           ELSE aux.svc[n].ever,
                                                  next |-> aux.svc[n].next]]]
                       ELSE aux
             /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen,
                            hEndedFrom, hFailedFrom, cur, pick, rside,
                            answered, ended, loaded, pendingEnding,
                            reportedFail, cleanCount, wasSwept, lostByHost,
                            toR, stage, resolved, claimed, sentAnn, refused,
                            refusedAt, refusalFails, leaseHeld, crashCount,
                            startCount>>

\* The host publishes an announced name of a row: a step of the projection,
\* under way until HPublicationDone. A row's cleanup waits for it.
HPublication(r) ==
    /\ Versions
    /\ hrow[r] \in {"loading", "loaded"}
    /\ hkey[r] # NoKey
    /\ ~aux.pub[r]
    /\ PendingOf(r) # {}
    /\ aux' = [aux EXCEPT !.pub = [aux.pub EXCEPT ![r] = TRUE]]
    /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen,
                   hEndedFrom, hFailedFrom, cur, pick, rside, answered,
                   ended, loaded, pendingEnding, reportedFail, cleanCount,
                   wasSwept, lostByHost, toR, toH, stage, resolved, claimed,
                   sentAnn, processedAnn, refused, refusedAt,
                   refusalFails, leaseHeld, crashCount, startCount>>

\* The publication completes: the names the table holds for the row are
\* registered, or -- a name another live row has -- the row is refused.
HPublicationDone(r) ==
    /\ Versions
    /\ aux.pub[r]
    /\ hkey[r] # NoKey
    /\ LET pending == PendingOf(r)
           clashing == {n \in pending : claimed[n] # NoKey
                                        /\ claimed[n] # hkey[r]
                                        /\ LiveKey(claimed[n])
                                        /\ Mutation # "allowClash"}
       IN /\ claimed' = IF clashing # {}
                         THEN claimed
                         ELSE [n \in Services |->
                                 IF n \in pending THEN hkey[r]
                                 ELSE claimed[n]]
          /\ refused' = IF clashing # {}
                         THEN [refused EXCEPT ![hkey[r]] = TRUE]
                         ELSE refused
          /\ refusedAt' = IF clashing # {}
                           THEN [refusedAt EXCEPT ![r] = hkey[r]]
                           ELSE refusedAt
          /\ aux' = [aux EXCEPT
                        !.pub = [aux.pub EXCEPT ![r] = FALSE],
                        !.prov = [aux.prov EXCEPT
                                    ![hkey[r]] = [n \in Services |->
                                        IF n \in pending
                                        THEN [count |-> aux.prov[hkey[r]][n].count,
                                              live |-> aux.prov[hkey[r]][n].live,
                                              v |-> aux.prov[hkey[r]][n].v,
                                              pub |-> aux.prov[hkey[r]][n].v]
                                        ELSE aux.prov[hkey[r]][n]]],
                        !.refusal = IF PreciseRefusal /\ clashing # {}
                                    THEN [aux.refusal EXCEPT
                                            ![r] = [armed |-> TRUE,
                                                    clash |-> aux.refusal[r].clash + 1,
                                                    failed |-> aux.refusal[r].failed,
                                                    waited |-> aux.refusal[r].waited]]
                                    ELSE aux.refusal]
          \* The load is already marked running (the name was published
          \* after the reply): stop it now.
          /\ hrow' = IF clashing # {} /\ hrow[r] = "loaded"
                      THEN [hrow EXCEPT ![r] = "unloading"]
                      ELSE hrow
          /\ hpending' = IF clashing # {} /\ hrow[r] = "loaded"
                          THEN [hpending EXCEPT ![r] = hkey[r]]
                          ELSE hpending
          /\ hFailedFrom' = IF clashing # {} /\ hrow[r] = "loaded"
                             THEN [hFailedFrom EXCEPT ![r] = hkey[r]]
                             ELSE hFailedFrom
          /\ toR' = IF clashing # {} /\ hrow[r] = "loaded"
                     THEN [toR EXCEPT
                             ![RtOf(hkey[r])][SessOf(hkey[r])] =
                                 Append(@, <<"unload", hkey[r]>>)]
                     ELSE toR
          /\ UNCHANGED <<hst, hconn, ndial, hkey, hgen, hEndedFrom, cur,
                         pick, rside, answered, ended, loaded, pendingEnding,
                         reportedFail, cleanCount, wasSwept, lostByHost, toH,
                         stage, resolved, sentAnn, processedAnn, refusalFails,
                         leaseHeld, crashCount, startCount>>

\* `rows.ended`: the row ended itself. Only the generation it belongs to
\* ends; a late ending of an earlier generation is ignored.
\* A row that ends is disabled: it is not loaded again.
HEnded(rt, s) ==
    /\ hst[rt] = "ready"
    /\ hconn[rt] > 0
    /\ toH[rt][hconn[rt]] /= <<>>
    /\ LET f == Head(toH[rt][hconn[rt]]) IN
       /\ f[1] = "ended"
       /\ LET k == f[2]
              r == RowOf(k)
              acts == hkey[r] = k /\ hrow[r] \in {"loading", "loaded"}
              bug == Mutation = "keyedByRow" /\ hkey[r] # k
                     /\ hrow[r] # "gone"
          IN /\ toH' = [toH EXCEPT ![rt][hconn[rt]] = Tail(@)]
             /\ hrow' = IF acts \/ bug
                        THEN [hrow EXCEPT ![r] = "gone"] ELSE hrow
             /\ hEndedFrom' = IF acts \/ bug
                              THEN [hEndedFrom EXCEPT ![r] = k]
                              ELSE hEndedFrom
             \* The row's disposer unloads it as any disposal does; the
             \* runtime already disposed it.
             /\ toR' = IF acts /\ hrow[r] = "loaded"
                       THEN [toR EXCEPT
                               ![rt][hconn[rt]] = Append(@, <<"unload", k>>)]
                       ELSE toR
             /\ hpending' = IF acts /\ hrow[r] = "loaded"
                             THEN [hpending EXCEPT ![r] = k]
                             ELSE hpending
             /\ UNCHANGED <<hst, hconn, ndial, hkey, hgen,
                            hFailedFrom, cur, pick, rside, answered, ended,
                            loaded, pendingEnding, reportedFail, cleanCount,
                            wasSwept, lostByHost, stage, resolved, claimed,
                            sentAnn, processedAnn, refused, refusedAt,
                            refusalFails, leaseHeld, crashCount, startCount,
                            aux>>

\* `rows.failed`: the row's code failed. The host restarts it: unload the
\* generation, then load a new one. A failure of an earlier generation is
\* ignored.
HFailed(rt, s) ==
    /\ hst[rt] = "ready"
    /\ hconn[rt] > 0
    /\ toH[rt][hconn[rt]] /= <<>>
    /\ LET f == Head(toH[rt][hconn[rt]]) IN
       /\ f[1] = "failed"
       /\ LET k == f[2]
              r == RowOf(k)
              acts == hkey[r] = k /\ hrow[r] = "loaded"
              bug == Mutation = "keyedByRow" /\ hkey[r] # k
                     /\ hkey[r] # NoKey /\ hrow[r] = "loaded"
          IN /\ toH' = [toH EXCEPT ![rt][hconn[rt]] = Tail(@)]
             /\ hrow' = IF acts \/ bug
                        THEN [hrow EXCEPT ![r] = "unloading"] ELSE hrow
             /\ hpending' = IF acts
                            THEN [hpending EXCEPT ![r] = k]
                            ELSE IF bug
                                 THEN [hpending EXCEPT ![r] = hkey[r]]
                                 ELSE hpending
             /\ toR' = IF acts
                       THEN [toR EXCEPT
                               ![rt][hconn[rt]] = Append(@, <<"unload", k>>)]
                       ELSE IF bug
                            THEN [toR EXCEPT
                                    ![rt][hconn[rt]] = Append(@,
                                      <<"unload", hkey[r]>>)]
                            ELSE toR
             /\ hFailedFrom' = IF acts \/ bug
                               THEN [hFailedFrom EXCEPT ![r] = k]
                               ELSE hFailedFrom
             /\ UNCHANGED <<hst, hconn, ndial, hkey, hgen, hEndedFrom,
                            cur, pick, rside, answered, ended, loaded,
                            pendingEnding, reportedFail, cleanCount, wasSwept,
                            lostByHost, stage, resolved, claimed, sentAnn,
                            processedAnn, refused, refusedAt, refusalFails,
                            leaseHeld, crashCount, startCount, aux>>

\* The refusal recorded for row r fails its next restart, once: the apply
\* of the restart fails and no new generation goes to the runtime. With
\* `PreciseRefusal` the refusal is the row's fiber's: it fails the first
\* restart after the clash, and a restart that waits for a dependency
\* instead is not failed (HWaitRefusal).
HLoadRefused(r) ==
    /\ hrow[r] = "idle"
    /\ IF PreciseRefusal
       THEN /\ aux.refusal[r].armed
            /\ LET rt == Home(r) IN
               /\ hst[rt] = "ready"
               /\ hconn[rt] > 0
               /\ ((stage[rt] = "rows" /\ resolved[r] = "final")
                   \/ Mutation = "loadProvisional")
               /\ \A n \in DepsOf(r) : claimed[n] # NoKey
               /\ LET q == hkey[r] IN
                  /\ refusalFails' = [refusalFails EXCEPT ![q] = @ + 1]
                  /\ aux' = [aux EXCEPT
                               !.refusal = [aux.refusal EXCEPT
                                              ![r] = [armed |-> FALSE,
                                                      clash |-> aux.refusal[r].clash,
                                                      failed |-> aux.refusal[r].clash,
                                                      waited |-> aux.refusal[r].waited]]]
                  /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen,
                                 hEndedFrom, hFailedFrom, cur, pick, rside,
                                 answered, ended, loaded, pendingEnding,
                                 reportedFail, cleanCount, wasSwept, lostByHost,
                                 toR, toH, stage, resolved, claimed, sentAnn,
                                 processedAnn, refused, refusedAt, leaseHeld,
                                 crashCount, startCount>>
       ELSE /\ refusedAt[r] # NoKey
            /\ LET q == refusedAt[r] IN
               /\ refusalFails' = [refusalFails EXCEPT ![q] = @ + 1]
               /\ refusedAt' = [refusedAt EXCEPT
                                  ![r] = IF Mutation = "refusalSticky"
                                            THEN q ELSE NoKey]
               /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen,
                              hEndedFrom, hFailedFrom, cur, pick, rside, answered,
                              ended, loaded, pendingEnding, reportedFail, cleanCount,
                              wasSwept, lostByHost, toR, toH, stage, resolved,
                              claimed, sentAnn, processedAnn, refused, leaseHeld,
                              crashCount, startCount, aux>>

\* The restart the refusal waits for has begun but its apply waits for a
\* dependency: the refusal's restart is this one, and it did not apply, so
\* the refusal is forgotten -- it fails no later generation.
HWaitRefusal(r) ==
    /\ PreciseRefusal
    /\ aux.refusal[r].armed
    /\ hrow[r] = "idle"
    /\ \E n \in DepsOf(r) : claimed[n] = NoKey
    /\ aux' = [aux EXCEPT
                 !.refusal = [aux.refusal EXCEPT
                                ![r] = [armed |-> Mutation = "refusalSticky",
                                        clash |-> aux.refusal[r].clash,
                                        failed |-> aux.refusal[r].failed,
                                        waited |-> aux.refusal[r].clash]]]
    /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen, hEndedFrom,
                   hFailedFrom, cur, pick, rside, answered, ended, loaded,
                   pendingEnding, reportedFail, cleanCount, wasSwept, lostByHost,
                   toR, toH, stage, resolved, claimed, sentAnn, processedAnn,
                   refused, refusedAt, refusalFails, leaseHeld, crashCount,
                   startCount>>

\* `rows.update`: a volatile-only change of a loaded row goes to the
\* runtime, which commits it in place. The mutation unloads the row for it
\* and loads it again -- a new generation where none is needed.
HUpdate(r) ==
    /\ Updates
    /\ hrow[r] = "loaded"
    /\ hkey[r] # NoKey
    /\ hconn[Home(r)] > 0
    /\ aux.updated[hkey[r]] < MaxUpdates
    /\ aux' = [aux EXCEPT
                 !.updated = [aux.updated EXCEPT ![hkey[r]] = @ + 1]]
    /\ IF Mutation = "updateReload"
       THEN /\ hrow' = [hrow EXCEPT ![r] = "unloading"]
            /\ hpending' = [hpending EXCEPT ![r] = hkey[r]]
            /\ hFailedFrom' = [hFailedFrom EXCEPT ![r] = hkey[r]]
            /\ toR' = [toR EXCEPT
                        ![Home(r)][hconn[Home(r)]] =
                            Append(@, <<"unload", hkey[r]>>)]
            /\ UNCHANGED <<hkey, hgen, hEndedFrom, leaseHeld>>
       ELSE /\ toR' = [toR EXCEPT
                        ![Home(r)][hconn[Home(r)]] =
                            Append(@, <<"update", hkey[r]>>)]
            /\ UNCHANGED <<hrow, hpending, hFailedFrom, hkey, hgen,
                           hEndedFrom, leaseHeld>>
    /\ UNCHANGED <<hst, hconn, ndial, cur, pick, rside, answered, ended,
                   loaded, pendingEnding, reportedFail, cleanCount, wasSwept,
                   lostByHost, toH, stage, resolved, claimed, sentAnn,
                   processedAnn, refused, refusedAt, refusalFails, crashCount,
                   startCount>>

\* The runtime committed the update: the row keeps running, its generation
\* unchanged.
HUpdated(rt, s) ==
    /\ Updates
    /\ toH[rt][s] /= <<>>
    /\ Head(toH[rt][s])[1] = "updated"
    /\ toH' = [toH EXCEPT ![rt][s] = Tail(@)]
    /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen, hEndedFrom,
                   hFailedFrom, cur, pick, rside, answered, ended, loaded,
                   pendingEnding, reportedFail, cleanCount, wasSwept, lostByHost,
                   toR, stage, resolved, claimed, sentAnn, processedAnn, refused,
                   refusedAt, refusalFails, leaseHeld, crashCount, startCount,
                   aux>>

\* The connection breaks: the host's rows on it stop; it dials again. A row
\* elsewhere that injected a service of this session stops too.
HLose(rt) ==
    /\ "lose" \in Faults
    /\ hst[rt] \in {"greeting", "ready"}
    /\ hconn[rt] > 0
    /\ LET s == hconn[rt]
           claims == ClaimsWithoutSession(rt, s)
           stopped == {r \in Rows : Home(r) = rt /\ SessOf(hkey[r]) = s
                                    /\ hrow[r] # "gone"}
           starved == {r \in Starved(claims) :
                          Home(r) # rt \/ SessOf(hkey[r]) # s}
       IN /\ hst' = [hst EXCEPT ![rt] = "waiting"]
          /\ hconn' = [hconn EXCEPT ![rt] = 0]
          /\ lostByHost' = [lostByHost EXCEPT ![rt][s] = TRUE]
          /\ claimed' = claims
          /\ hrow' = [r \in Rows |->
                        IF r \in stopped THEN "idle"
                        ELSE IF r \in starved THEN "unloading"
                        ELSE hrow[r]]
          /\ hpending' = [r \in Rows |->
                            IF r \in stopped THEN NoKey
                            ELSE IF r \in starved THEN hkey[r]
                            ELSE hpending[r]]
          /\ hFailedFrom' = [r \in Rows |->
                               IF r \in starved THEN hkey[r]
                               ELSE hFailedFrom[r]]
          /\ leaseHeld' = [k \in Keys |->
                             IF RtOf(k) = rt /\ SessOf(k) = s
                             THEN {} ELSE leaseHeld[k]]
          /\ toR' = [rt2 \in Runtimes |->
                       [s2 \in Sessions |->
                           toR[rt2][s2] \o FramesOn(rt2, s2, starved)]]
          /\ aux' = [aux EXCEPT
                       !.svc = IF Versions
                               THEN Dropped(aux.svc, NamesOfClaims(claims))
                               ELSE aux.svc,
                       !.hosts = IF HostFrames
                                 THEN ReleasedBy(aux.hosts,
                                        [h \in HostServices |->
                                            Cardinality({k \in Keys :
                                                           RtOf(k) = rt
                                                           /\ SessOf(k) = s
                                                           /\ h \in leaseHeld[k]})])
                                 ELSE aux.hosts]
          /\ UNCHANGED <<ndial, hkey, hgen, hEndedFrom, cur, pick,
                         rside, answered, ended, loaded, pendingEnding,
                         reportedFail, cleanCount, wasSwept, toH, stage,
                         resolved, sentAnn, processedAnn, refused, refusedAt,
                         refusalFails, crashCount, startCount>>

(***************************************************************************)
(* The runtime: its row table, its services and the listener that replaces *)
(* the session.                                                            *)
(***************************************************************************)

\* A side that closed a connection reads nothing more on it. A read-old
\* mutation keeps reading the old session after the replace.
CanRead(rt, s) ==
    (cur[rt] = s /\ rside[rt][s] = "open")
    \/ (Mutation = "readOld" /\ ended[rt][s])

\* Swept: the session ended and nothing of it is loaded any more.
Swept(rt, s) == ended[rt][s] /\ RowsOf(rt, s) = {}

\* The listener takes up a new connection's hello. It closes the old
\* session and starts cleaning it; the answer waits for that (Answer).
TakeUp(rt, s) ==
    /\ pick[rt] = 0
    /\ rside[rt][s] = "open"
    /\ stage[rt] # "down"
    /\ toR[rt][s] /= <<>>
    /\ Head(toR[rt][s]) = <<"hello", NoKey>>
    /\ LET old == cur[rt] IN
       /\ toR' = [toR EXCEPT ![rt][s] = Tail(@)]
       /\ pick' = [pick EXCEPT ![rt] = s]
       /\ rside' = IF old = 0 THEN rside
                  ELSE [rside EXCEPT ![rt][old] = "closed"]
       /\ ended' = IF old = 0 THEN ended
                   ELSE [ended EXCEPT ![rt][old] = TRUE]
       /\ cur' = [cur EXCEPT ![rt] = 0]
       /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen,
                      hEndedFrom, hFailedFrom, answered, loaded,
                      pendingEnding, reportedFail, cleanCount, wasSwept,
                      lostByHost, toH, stage, resolved, claimed, sentAnn,
                      processedAnn, refused, refusedAt, refusalFails,
                      leaseHeld, crashCount, startCount, aux>>

\* The answer to the new hello: only once every earlier session of this
\* runtime is cleaned, so no two sessions' rows are ever registered
\* together, and -- no runtime waits for another one -- nothing of another
\* runtime is required.
Answer(rt, s) ==
    /\ pick[rt] = s
    /\ (Mutation = "acceptBeforeClean"
        \/ ((\A p \in Sessions : p < s => Swept(rt, p))
            /\ (Mutation # "mutualWait"
                \/ (\A rt2 \in Runtimes : rt2 = rt
                       \/ (stage[rt2] = "rows"
                           /\ \A p \in Sessions : Swept(rt2, p))))))
    /\ pick' = [pick EXCEPT ![rt] = 0]
    /\ cur' = [cur EXCEPT ![rt] = s]
    /\ answered' = [answered EXCEPT ![rt][s] = TRUE]
    /\ toH' = [toH EXCEPT ![rt][s] = Append(@, <<"hello", NoKey>>)]
    /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen,
                   hEndedFrom, hFailedFrom, rside, ended, loaded,
                   pendingEnding, reportedFail, cleanCount, wasSwept,
                   lostByHost, toR, stage, resolved, claimed, sentAnn,
                   processedAnn, refused, refusedAt, refusalFails, leaseHeld,
                   crashCount, startCount, aux>>

\* The runtime notices the host's close: the session is over.
EndSession(rt, s) ==
    /\ lostByHost[rt][s]
    /\ rside[rt][s] = "open"
    /\ rside' = [rside EXCEPT ![rt][s] = "closed"]
    /\ ended' = [ended EXCEPT ![rt][s] = TRUE]
    /\ cur' = IF cur[rt] = s THEN [cur EXCEPT ![rt] = 0] ELSE cur
    /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen,
                   hEndedFrom, hFailedFrom, pick, answered, loaded,
                   pendingEnding, reportedFail, cleanCount, wasSwept,
                   lostByHost, toR, toH, stage, resolved, claimed, sentAnn,
                   processedAnn, refused, refusedAt, refusalFails, leaseHeld,
                   crashCount, startCount, aux>>

\* The runtime read the load frame: with `LoadSteps` its apply runs before
\* the reply.
RLoad(k) ==
    /\ LET rt == RtOf(k) s == SessOf(k) IN
       /\ toR[rt][s] /= <<>>
       /\ Head(toR[rt][s]) = <<"load", k>>
       /\ CanRead(rt, s)
       /\ (Versions => \A n \in NamesOf(RowOf(k)) : aux.prov[k][n].live)
       /\ IF LoadSteps
          THEN /\ toR' = [toR EXCEPT ![rt][s] = Tail(@)]
               /\ aux' = [aux EXCEPT !.applying = aux.applying \cup {k}]
               /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen,
                              hEndedFrom, hFailedFrom, cur, pick, rside, answered,
                              ended, loaded, pendingEnding, reportedFail,
                              cleanCount, wasSwept, lostByHost, toH, stage,
                              resolved, claimed, sentAnn, processedAnn, refused,
                              refusedAt, refusalFails, leaseHeld, crashCount,
                              startCount>>
          ELSE /\ toR' = [toR EXCEPT ![rt][s] = Tail(@)]
               /\ loaded' = loaded \cup {k}
               /\ sentAnn' = IF Versions
                             THEN sentAnn
                             ELSE [sentAnn EXCEPT
                                     ![k] = NamesOf(RowOf(k))]
               /\ aux' = [aux EXCEPT
                            !.replied = aux.replied]
               /\ IF Versions
                  THEN toH' = [toH EXCEPT
                                 ![rt][s] = Append(@, <<"loaded", k>>)]
                  ELSE IF Mutation = "announceAfterLoad"
                  THEN toH' = [toH EXCEPT
                                 ![rt][s] = @ \o (<< <<"loaded", k>> >>
                                                  \o AnnounceFrames(k, NamesOf(RowOf(k))))]
                  ELSE toH' = [toH EXCEPT
                                 ![rt][s] = @ \o (AnnounceFrames(k, NamesOf(RowOf(k)))
                                                  \o << <<"loaded", k>> >>)]
               /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen,
                              hEndedFrom, hFailedFrom, cur, pick, rside, answered,
                              ended, pendingEnding, reportedFail, cleanCount,
                              wasSwept, lostByHost, stage, resolved, claimed,
                              processedAnn, refused, refusedAt, refusalFails,
                              leaseHeld, crashCount, startCount>>

\* The apply finished: the reply goes out. A row that ended while it
\* applied (EndDuringLoad) reports its ending only after this.
RLoadFinish(k) ==
    /\ LoadSteps
    /\ k \in aux.applying
    /\ LET rt == RtOf(k) s == SessOf(k) IN
       /\ CanRead(rt, s)
       /\ loaded' = loaded \cup {k}
       /\ sentAnn' = IF Versions
                     THEN sentAnn
                     ELSE [sentAnn EXCEPT ![k] = NamesOf(RowOf(k))]
       /\ toH' = IF Versions
                 THEN [toH EXCEPT ![rt][s] = Append(@, <<"loaded", k>>)]
                 ELSE IF Mutation = "announceAfterLoad"
                 THEN [toH EXCEPT
                        ![rt][s] = @ \o (<< <<"loaded", k>> >>
                                         \o AnnounceFrames(k, NamesOf(RowOf(k))))]
                 ELSE [toH EXCEPT
                        ![rt][s] = @ \o (AnnounceFrames(k, NamesOf(RowOf(k)))
                                         \o << <<"loaded", k>> >>)]
       /\ aux' = [aux EXCEPT
                    !.applying = aux.applying \ {k},
                    !.replied = aux.replied \cup {k}]
       /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen,
                      hEndedFrom, hFailedFrom, cur, pick, rside, answered,
                      ended, pendingEnding, reportedFail, cleanCount, wasSwept,
                      lostByHost, toR, stage, resolved, claimed, processedAnn,
                      refused, refusedAt, refusalFails, leaseHeld, crashCount,
                      startCount>>

\* The runtime provides one of a row's names, at the name's next version:
\* before the load's reply, or again while the row runs (a service update).
RProvide(k, n) ==
    /\ Versions
    /\ n \in NamesOf(RowOf(k))
    /\ LET rt == RtOf(k) s == SessOf(k) IN
       /\ CanRead(rt, s)
       /\ aux.prov[k][n].count < MaxProvides
       \* The first provide is part of the load (before its reply); a later
       \* one is a service update of a row that runs.
       /\ IF aux.prov[k][n].count = 0
          THEN toR[rt][s] /= <<>> /\ Head(toR[rt][s]) = <<"load", k>>
          ELSE k \in loaded
       /\ LET v == aux.svc[n].next IN
          /\ toH' = [toH EXCEPT
                       ![rt][s] = Append(@, <<"provide", k, n, v>>)]
          /\ sentAnn' = IF aux.prov[k][n].count = 0
                        THEN [sentAnn EXCEPT ![k] = @ \cup {n}]
                        ELSE sentAnn
          /\ aux' = [aux EXCEPT
                       !.svc = [aux.svc EXCEPT ![n].next = v + 1],
                       !.prov = [aux.prov EXCEPT
                                   ![k][n] = [count |-> aux.prov[k][n].count + 1,
                                              live |-> TRUE, v |-> v,
                                              pub |-> aux.prov[k][n].pub]]]
          /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen,
                         hEndedFrom, hFailedFrom, cur, pick, rside, answered,
                         ended, loaded, pendingEnding, reportedFail, cleanCount,
                         wasSwept, lostByHost, toR, stage, resolved, claimed,
                         processedAnn, refused, refusedAt, refusalFails,
                         leaseHeld, crashCount, startCount>>

\* The runtime could not load the row (its code failed to import, say).
RFailLoad(k) ==
    /\ LoadFaults
    /\ LET rt == RtOf(k) s == SessOf(k) IN
       /\ toR[rt][s] /= <<>>
       /\ Head(toR[rt][s]) = <<"load", k>>
       /\ CanRead(rt, s)
       /\ toR' = [toR EXCEPT ![rt][s] = Tail(@)]
       /\ toH' = [toH EXCEPT ![rt][s] = Append(@, <<"loadfailed", k>>)]
       /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen,
                      hEndedFrom, hFailedFrom, cur, pick, rside, answered,
                      ended, loaded, pendingEnding, reportedFail, cleanCount,
                      wasSwept, lostByHost, stage, resolved, claimed, sentAnn,
                      processedAnn, refused, refusedAt, refusalFails,
                      leaseHeld, crashCount, startCount, aux>>

\* `rows.unload`: idempotent. A row still loaded is disposed once; one
\* already gone is only answered.
RUnload(k) ==
    /\ LET rt == RtOf(k) s == SessOf(k) IN
       /\ toR[rt][s] /= <<>>
       /\ Head(toR[rt][s]) = <<"unload", k>>
       /\ CanRead(rt, s)
       /\ ~(LoadSteps /\ k \in aux.applying)
       /\ toR' = [toR EXCEPT ![rt][s] = Tail(@)]
       /\ toH' = [toH EXCEPT ![rt][s] = Append(@, <<"unloaded", k>>)]
       /\ IF k \in loaded
          THEN /\ loaded' = loaded \ {k}
               /\ cleanCount' = [cleanCount EXCEPT ![k] = @ + 1]
          ELSE /\ loaded' = loaded
               /\ cleanCount' = [cleanCount EXCEPT ![k] =
                                   IF Mutation = "reclean" THEN @ + 1 ELSE @]
       /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen,
                      hEndedFrom, hFailedFrom, cur, pick, rside, answered,
                      ended, pendingEnding, reportedFail, wasSwept,
                      lostByHost, stage, resolved, claimed, sentAnn,
                      processedAnn, refused, refusedAt, refusalFails,
                      leaseHeld, crashCount, startCount, aux>>

\* The plugin of a loaded row disposes itself: the row is gone, and its
\* ending is still to be sent. The runtime's disposal counts once.
EndSelf(k) ==
    /\ SelfEnds
    /\ k \in loaded
    /\ loaded' = loaded \ {k}
    /\ pendingEnding' = pendingEnding \cup {k}
    /\ cleanCount' = [cleanCount EXCEPT ![k] = @ + 1]
    /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen,
                   hEndedFrom, hFailedFrom, cur, pick, rside, answered,
                   ended, reportedFail, wasSwept, lostByHost,
                   toR, toH, stage, resolved, claimed, sentAnn, processedAnn,
                   refused, refusedAt, refusalFails, leaseHeld, crashCount,
                   startCount, aux>>

\* The plugin of a row disposes itself while its load applies: the row is
\* still reported as loaded (the load succeeds), and its ending goes out
\* after the reply (54a7e824). The mutation reports it at once.
EndDuringLoad(k) ==
    /\ LoadSteps
    /\ SelfEnds
    /\ k \in aux.applying
    /\ pendingEnding' = pendingEnding \cup {k}
    /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen,
                   hEndedFrom, hFailedFrom, cur, pick, rside, answered,
                   ended, loaded, reportedFail, cleanCount, wasSwept,
                   lostByHost, toR, toH, stage, resolved, claimed, sentAnn,
                   processedAnn, refused, refusedAt, refusalFails, leaseHeld,
                   crashCount, startCount, aux>>

\* The ending goes out. Sent only while the session is up: after it ended
\* the runtime has nothing to send on it. With `LoadSteps` only after the
\* load's reply.
SendEnding(k) ==
    /\ k \in pendingEnding
    /\ LET rt == RtOf(k) s == SessOf(k) IN
       /\ rside[rt][s] = "open"
       /\ answered[rt][s]
       /\ (Mutation = "endedDuringLoad" \/ ~LoadSteps \/ k \in aux.replied)
       /\ pendingEnding' = pendingEnding \ {k}
       /\ toH' = [toH EXCEPT ![rt][s] = Append(@, <<"ended", k>>)]
       /\ aux' = IF LoadSteps
                 THEN [aux EXCEPT !.endedSent = aux.endedSent \cup {k}]
                 ELSE aux
       /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen,
                      hEndedFrom, hFailedFrom, cur, pick, rside, answered,
                      ended, loaded, reportedFail, cleanCount, wasSwept,
                      lostByHost, toR, stage, resolved, claimed, sentAnn,
                      processedAnn, refused, refusedAt, refusalFails,
                      leaseHeld, crashCount, startCount>>

\* The row's code left an unhandled error: the row stays loaded (the host
\* decides what to do with it) and hears `rows.failed`.
RFail(k) ==
    /\ Failures
    /\ k \in loaded
    /\ k \notin reportedFail
    /\ LET rt == RtOf(k) s == SessOf(k) IN
       /\ rside[rt][s] = "open"
       /\ answered[rt][s]
       /\ reportedFail' = reportedFail \cup {k}
       /\ toH' = [toH EXCEPT ![rt][s] = Append(@, <<"failed", k>>)]
       /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen,
                      hEndedFrom, hFailedFrom, cur, pick, rside, answered,
                      ended, loaded, pendingEnding, cleanCount, wasSwept,
                      lostByHost, toR, stage, resolved, claimed, sentAnn,
                      processedAnn, refused, refusedAt, refusalFails,
                      leaseHeld, crashCount, startCount, aux>>

\* The runtime committed `rows.update` on the row it still has.
RUpdate(k) ==
    /\ Updates
    /\ k \in loaded
    /\ LET rt == RtOf(k) s == SessOf(k) IN
       /\ toR[rt][s] /= <<>>
       /\ Head(toR[rt][s]) = <<"update", k>>
       /\ CanRead(rt, s)
       /\ toR' = [toR EXCEPT ![rt][s] = Tail(@)]
       /\ toH' = [toH EXCEPT ![rt][s] = Append(@, <<"updated", k>>)]
       /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen,
                      hEndedFrom, hFailedFrom, cur, pick, rside, answered,
                      ended, loaded, pendingEnding, reportedFail, cleanCount,
                      wasSwept, lostByHost, stage, resolved, claimed, sentAnn,
                      processedAnn, refused, refusedAt, refusalFails,
                      leaseHeld, crashCount, startCount, aux>>

\* The session ended: its rows are disposed, once each.
Sweep(k) ==
    /\ Mutation /= "keepRows"
    /\ k \in loaded
    /\ ended[RtOf(k)][SessOf(k)]
    /\ loaded' = loaded \ {k}
    /\ cleanCount' = [cleanCount EXCEPT ![k] = @ + 1]
    /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen,
                   hEndedFrom, hFailedFrom, cur, pick, rside, answered, ended,
                   pendingEnding, reportedFail, wasSwept, lostByHost,
                   toR, toH, stage, resolved, claimed, sentAnn, processedAnn,
                   refused, refusedAt, refusalFails, leaseHeld, crashCount,
                   startCount, aux>>

\* Ghost: the session was swept at least once (for NoRowsAfterSweep).
MarkSwept(rt, s) ==
    /\ Swept(rt, s)
    /\ ~wasSwept[rt][s]
    /\ wasSwept' = [wasSwept EXCEPT ![rt][s] = TRUE]
    /\ UNCHANGED <<hst, hconn, ndial, hrow, hkey, hpending, hgen,
                   hEndedFrom, hFailedFrom, cur, pick, rside, answered,
                   ended, loaded, pendingEnding, reportedFail, cleanCount,
                   lostByHost, toR, toH, stage, resolved, claimed, sentAnn,
                   processedAnn, refused, refusedAt, refusalFails, leaseHeld,
                   crashCount, startCount, aux>>

\* The runtime process dies, the session with it. Its rows are cleaned
\* once, its leases and services go, and the host may start it again -- a
\* new process re-resolves its rows (they are provisional again), and a
\* refresh it was running is cancelled.
HCrash(rt) ==
    LET gone == {k \in loaded : RtOf(k) = rt}
        claims == ClaimsWithoutRuntime(rt)
        starved == {r \in Starved(claims) : Home(r) # rt}
    IN /\ Crashes
       /\ rt \in CrashRuntimes
       /\ crashCount[rt] < CrashLimit
       /\ stage[rt] # "down"
       /\ loaded' = loaded \ gone
       /\ cleanCount' = [k \in Keys |->
                           IF k \in gone THEN cleanCount[k] + 1
                           ELSE cleanCount[k]]
       /\ pendingEnding' = pendingEnding \ gone
       /\ reportedFail' = reportedFail \ gone
       /\ claimed' = claims
       /\ toR' = [rt2 \in Runtimes |->
                     [s2 \in Sessions |->
                         toR[rt2][s2] \o FramesOn(rt2, s2, starved)]]
       /\ hFailedFrom' = [r \in Rows |->
                             IF r \in starved THEN hkey[r]
                             ELSE hFailedFrom[r]]
       /\ crashCount' = [crashCount EXCEPT ![rt] = @ + 1]
       /\ stage' = [stage EXCEPT ![rt] = "down"]
       /\ resolved' = [r \in Rows |->
                          IF Home(r) = rt /\ TwoStages THEN "provisional"
                          ELSE resolved[r]]
       /\ leaseHeld' = [k \in Keys |->
                          IF RtOf(k) = rt THEN {} ELSE leaseHeld[k]]
       /\ refusedAt' = [r \in Rows |->
                          IF Home(r) = rt THEN NoKey ELSE refusedAt[r]]
       /\ cur' = [cur EXCEPT ![rt] = 0]
       /\ pick' = [pick EXCEPT ![rt] = 0]
       /\ rside' = [rside EXCEPT
                      ![rt] = [s \in Sessions |->
                                 IF rside[rt][s] = "none" THEN "none"
                                 ELSE "closed"]]
       /\ ended' = [ended EXCEPT
                      ![rt] = [s \in Sessions |->
                                 IF rside[rt][s] = "none" THEN ended[rt][s]
                                 ELSE TRUE]]
       /\ hst' = [hst EXCEPT ![rt] = "waiting"]
       /\ hconn' = [hconn EXCEPT ![rt] = 0]
       /\ hrow' = [r \in Rows |->
                     IF Home(r) = rt /\ hrow[r] # "gone" THEN "idle"
                     ELSE IF r \in starved THEN "unloading"
                     ELSE hrow[r]]
       /\ hpending' = [r \in Rows |->
                         IF Home(r) = rt THEN NoKey
                         ELSE IF r \in starved THEN hkey[r]
                         ELSE hpending[r]]
       /\ aux' = [aux EXCEPT
                    !.svc = IF Versions
                            THEN Dropped(aux.svc, NamesOfClaims(claims))
                            ELSE aux.svc,
                    !.hosts = IF HostFrames
                              THEN ReleasedBy(aux.hosts,
                                     [h \in HostServices |->
                                         Cardinality({k \in Keys :
                                                        RtOf(k) = rt
                                                        /\ h \in leaseHeld[k]})])
                              ELSE aux.hosts,
                    !.refresh = IF Refreshes /\ aux.refresh[rt] = "running"
                                THEN [aux.refresh EXCEPT ![rt] = "cancelled"]
                                ELSE aux.refresh]
       /\ UNCHANGED <<ndial, hkey, hgen, hEndedFrom, answered,
                      wasSwept, lostByHost, toH, sentAnn, processedAnn,
                      refused, refusalFails, startCount>>

(***************************************************************************)
(* The specification.                                                      *)
(***************************************************************************)
Next ==
    \/ \E rt \in Runtimes :
         HDial(rt) \/ HGreeted(rt) \/ HLose(rt)
         \/ StartRuntime(rt) \/ Refresh(rt) \/ RefreshBegin(rt)
         \/ RefreshDone(rt) \/ HCrash(rt)
    \/ \E r \in Rows : HLoad(r) \/ HLoadRefused(r) \/ HWaitRefusal(r)
         \/ HUpdate(r) \/ HPublication(r) \/ HPublicationDone(r)
    \/ \E rt \in Runtimes, s \in Sessions :
         HLoaded(rt, s) \/ HUnloaded(rt, s) \/ HLoadFailed(rt, s)
         \/ HAnnounce(rt, s) \/ HProvide(rt, s)
         \/ HEnded(rt, s) \/ HFailed(rt, s) \/ HUpdated(rt, s)
         \/ TakeUp(rt, s) \/ Answer(rt, s) \/ EndSession(rt, s)
         \/ MarkSwept(rt, s)
    \/ \E k \in Keys :
         RLoad(k) \/ RLoadFinish(k) \/ RFailLoad(k) \/ RUnload(k)
         \/ EndSelf(k) \/ EndDuringLoad(k) \/ SendEnding(k) \/ RFail(k)
         \/ Sweep(k) \/ RUpdate(k)
    \/ \E k \in Keys, n \in Services : RProvide(k, n)

Spec == Init /\ [][Next]_vars /\ WF_vars(Next)

(***************************************************************************)
(* Properties.                                                             *)
(***************************************************************************)

\* One row of a name is loaded at the runtime at a time: the old session's
\* rows are cleaned before a new session's load, and a restart follows its
\* unload.
OneRowPerName ==
    \A r \in Rows : Cardinality({k \in loaded : RowOf(k) = r}) <= 1

\* A session is answered only after every earlier session of this runtime
\* is gone.
NewSessionOnlyAfterSweep ==
    \A rt \in Runtimes, s \in Sessions :
        answered[rt][s] => \A p \in Sessions : p < s => Swept(rt, p)

\* Once a session was swept, no frame of it ever starts a row again: the
\* runtime stopped reading it.
NoRowsAfterSweep ==
    \A rt \in Runtimes, s \in Sessions :
        wasSwept[rt][s] => \A k \in loaded : ~(RtOf(k) = rt /\ SessOf(k) = s)

\* The host acts on an ending or a failure only for the generation that
\* row currently is.
OwnFramesOnly ==
    \A r \in Rows :
        /\ (hEndedFrom[r] = NoKey \/ hEndedFrom[r] = hkey[r])
        /\ (hFailedFrom[r] = NoKey \/ hFailedFrom[r] = hkey[r])

\* A row is cleaned once: the sweep, the unload and the row's own end do
\* not dispose the same row twice, and a crash does not clean a row another
\* action already cleaned.
CleanupOnce == \A k \in Keys : cleanCount[k] <= 1

\* A row loads only once its runtime runs fully and its resolution is
\* final: no row runs on a provisional resolution, and none loads before
\* `<language>RuntimeRows` is provided.
NoProvisional ==
    \A r \in Rows :
        hrow[r] \in {"loading", "loaded"} =>
            /\ resolved[r] = "final"
            /\ stage[Home(r)] = "rows"

\* A row is marked Running only after the host answered every service
\* announcement made while it loaded: the announcements arrive in the
\* load's synchronous call chain, before its reply.
ServicesBeforeRunning ==
    \A r \in Rows :
        hrow[r] = "loaded" => processedAnn[hkey[r]] = sentAnn[hkey[r]]

\* Two rows never run with one name: the later announcement is refused and
\* its row's load fails, so the host never marks both Running.
OneProviderPerName ==
    \A n \in Services :
        Cardinality({r \in Rows :
                       IF hrow[r] = "loaded"
                       THEN (IF Versions
                             THEN aux.prov[hkey[r]][n].live
                                  /\ aux.prov[hkey[r]][n].pub
                                     = aux.prov[hkey[r]][n].v
                             ELSE n \in NamesOf(r))
                       ELSE FALSE}) <= 1

\* A refusal fails exactly the one restart it caused; a later generation
\* (one that waited instead of applying, say) is not refused again.
RefusalOnce == \A k \in Keys : refusalFails[k] <= 1

\* A row's host service leases live within its generation: they are held
\* only by the row's current generation, they are released by its cleanup,
\* and the next generation loads only after the release.
LeaseWithinRow ==
    /\ \A k \in Keys :
        leaseHeld[k] # {} =>
            /\ hkey[RowOf(k)] = k
            /\ hrow[RowOf(k)] \in {"loading", "loaded", "unloading", "gone"}
    /\ \A r \in Rows :
        Cardinality({k \in Keys : RowOf(k) = r /\ leaseHeld[k] # {}}) <= 1

\* A session that ended ends with its rows cleaned.
EndedLeadsToSwept ==
    \A rt \in Runtimes, s \in Sessions : (ended[rt][s] ~> Swept(rt, s))

\* After a crash the runtime's rows are loaded again (while the runtime may
\* be started and the host may dial).
RestartRecovers ==
    \A rt \in CrashRuntimes, r \in Rows :
        (Home(r) = rt /\ crashCount[rt] > 0) =>
        (crashCount[rt] > 0 ~> hrow[r] \in {"loaded", "gone"})

\* A running row has the services it injects: when one goes away, the host
\* stops the row as the service goes (it is unloading, not running).
LoadedNeedsDeps ==
    \A r \in Rows :
        hrow[r] = "loaded" => \A n \in DepsOf(r) : claimed[n] # NoKey

\* One runtime's crash does not stop another runtime: its rows still reach
\* Running.
RuntimesDoNotWait ==
    \A rt1 \in CrashRuntimes, rt2 \in Runtimes :
        rt1 # rt2 =>
            (crashCount[rt1] > 0 ~>
                \A r \in RowsOfRuntime(rt2) : hrow[r] \in {"loaded", "gone"})

\* Two runtimes that both start and answer load their rows: neither waits
\* for the other's stages or sessions (SYN-2-14).
ColdStartLoads ==
    \A r \in Rows :
        ((hst[Home(r)] = "ready" /\ stage[Home(r)] = "rows"
          /\ resolved[r] = "final") ~> hrow[r] \in {"loaded", "gone"})

\* The host's table holds the newest version it applied: an older
\* `provide` or `withdraw`, arriving late, never overrides a newer one.
NewerWins ==
    Versions =>
        \A n \in Services : aux.svc[n].ever = aux.svc[n].known[2]

\* A row's service is withdraws before the row is unloaded: once the host
\* is done with the row, no name of it is published any more.
WithdrawBeforeUnload ==
    Versions =>
        \A r \in Rows :
            hrow[r] \in {"idle", "gone"} /\ hkey[r] # NoKey =>
                \A n \in Services : claimed[n] # hkey[r]

\* `hosts.provide` and `hosts.withdraw` are balanced: the runtime holds a
\* name exactly while a generation leases it, the frames are one provide
\* per registered name, and the runtime has a shape for it.
HostLeases ==
    HostFrames =>
        \A h \in HostServices :
            /\ aux.hosts[h].count = Cardinality({k \in Keys : h \in leaseHeld[k]})
            /\ (aux.hosts[h].reg <=> aux.hosts[h].count > 0)
            /\ aux.hosts[h].provides - aux.hosts[h].withdraws
               = IF aux.hosts[h].reg THEN 1 ELSE 0
            /\ (aux.hosts[h].reg => aux.hosts[h].shape # "none")

\* A refusal fails only the restart it caused: a restart that waited for a
\* dependency is not failed by it (d94aae9).
RefusalScope ==
    PreciseRefusal =>
        \A r \in Rows :
            ~(aux.refusal[r].failed > 0
              /\ aux.refusal[r].failed = aux.refusal[r].waited)

\* A row that ends while its load applies reports its ending only after the
\* load's reply (54a7e824).
EndingAfterLoad ==
    LoadSteps => \A k \in aux.endedSent : k \in aux.replied

\* A volatile-only update is committed on the generation that runs: it
\* neither unloads the row nor replaces it (SYN-4-8).
UpdateInPlace ==
    Updates =>
        \A k \in Keys :
            aux.updated[k] > 0 =>
                /\ hkey[RowOf(k)] = k
                /\ hrow[RowOf(k)] = "loaded"

\* A refresh a runtime restart cancelled is run again (REM-4.3-3).
RefreshRedone ==
    Refreshes =>
        \A rt \in Runtimes : (aux.refresh[rt] = "cancelled" ~> stage[rt] = "rows")
===============================================================================
