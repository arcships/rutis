# Cross-runtime synchronous calls and the deadlock policy (TLA+)

[中文](README.md)

TLC explores every interleaving of language runtimes calling each other synchronously through the Rust host, to answer the question left open in §9 of the multi-language design: which of the two candidate policies removes the deadlocks. Issue #208; quality standard Q11.2, Q6.2.8, Q6.8.3; status §7.1 (risks P10, C6).

**Reconciliation (2026-11-09, #614).** The wait graph is now the one `crates/rutis-bridge/src/session/rpc/waits.rs` has: #561's slow path keeps its chain, #597's host-originated synchronous call is a hop, and 854bef5d's asynchronous call on a marked chain is a hop too. The model adds Go / Swift / Kotlin's `Profile` (`stacked = false`, the constant `Threaded`), `bail` / `waterfall-sync` synchronous dispatch, a caller that gives up a synchronous call locally (no `cancel`), Python's synchronous call from another thread (an edge the host cannot see), session end, and SYN-9-3's explicit non-guarantee. What changed since #535 is in "Results" and "Counterexamples".

## Design and code it corresponds to

| Part of the model | Design | Implementation |
| --- | --- | --- |
| Forwarding, `path` rewriting | [Multi-language design](../../docs/design-multilang-runtimes-2026-10-03.en.md) §5; [M1](../../docs/design-multilang-m1-2026-10-04.en.md) §7 | `crates/rutis-bridge/src/session/rpc.rs` `rebase`, `forward`; `rpc/relay.rs`; `runtime/rows.rs` `RowService::invoke` |
| Which Rust thread gets an incoming call | M1 §7.2 | `rpc.rs` `receive`: the last id in `path` that a thread is synchronously waiting for |
| The six operations of the wait table (`profile`, `marked`, `answered`, `returned`, `ended`, `forward`) and the three kinds of call (synchronous, asynchronous on a marked chain, entry) | Multi-language design §9, "implementation" | `crates/rutis-bridge/src/session/rpc/waits.rs`; `rpc.rs` `receive`/`send` |
| Runtimes with a thread per call (Go, Swift, Kotlin): `sync-wait`, `reentrant-sync`, and not `sync-stack` | Multi-language design §9 (2026-10-14) | `waits.rs` `Profile.stacked`; `go/rutis/internal/peer/peer.go`, `swift/rutis/Sources/Rutis/Session/Peer.swift`, `kotlin/rutis/rutis/src/shared/kotlin/arcships/rutis/internal/peer/Peer.kt` |
| A runtime that does not declare `sync-wait` is outside the check | Multi-language design §9, "mixing versions" | `waits.rs` `forward`: a call without `sync` is not recorded; `profile` |
| Synchronous event dispatch: `bail`, `waterfall-sync` | [Events protocol](../../docs/design-events-protocol-2026-10-18.en.md) §5.4 | `crates/rutis-bridge/src/events/hub.rs` `dispatch` (`Mode::Bail`, `Mode::Waterfall if sync`), `call_sync`, `Waits`/`wait_here` |
| Node runtime: non-reentrant | Multi-language design §9; [plugin API](../../docs/guide/plugin-api.en.md), "sync calls and reentrancy" | `node/rutis-runtime/src/session.mjs` `#requestSync`, `#run` |
| Python runtime: reentrant; a synchronous call from another thread | same | `python/rutis/rutis/peer.py` `_call_sync`, `_run`; `run_coroutine_threadsafe` |
| Giving up a synchronous call locally (no `cancel`) | Multi-language design §9 (after #561) | Go's context, Swift's Task, Kotlin's interrupt |
| Policy B (A is not adopted) | Multi-language design §9 | `waits.rs` (#228); the `sync` field of call frames in `session.mjs`, `peer.py`, `bun/rutis-bun/src/session.ts` |

## Model

- **Participants**: the Rust host and 3 runtimes. A runtime is either single-threaded with one stack (outside `Threaded`, declares `sync-stack`) or one thread or task per incoming call (`Threaded`: Go / Swift / Kotlin). One session per runtime; each direction is delivered in order.
- **A stacked runtime**: only the top runs. When the top makes a synchronous call, the thread reads incoming frames while it waits (pumping): a reply marks its call as returned; a call runs at once when idle; while waiting, a reentrant runtime runs it nested and a non-reentrant one puts it aside unless its path names one of its waits; a top that is running reads nothing.
- **A runtime with a thread per call**: every incoming call starts on its own thread at once; nothing waits for anything, there is no stack order and no call is put aside. Its own synchronous calls still wait.
- **Rust** reads each session in order, forwards calls, rewrites `path`, and returns results to their callers; its threads are not a bounded resource (see "Simplifications"). It also makes its own synchronous calls (a Rust plugin's) and runs `bail` / `waterfall-sync` dispatches.
- **How a call is classified** (waits.rs `forward`): the caller waits synchronously and the last call of its chain is marked → a **hop** (`kind` = `"hop"`); it waits synchronously but no marked call is on its chain → a hop with an empty `src` (#597); an asynchronous call whose chain's last call is marked → a hop too (`kind` = `"async"`, though nobody waits for it); otherwise an **entry**. A synchronous call from a runtime that does not declare `sync-wait` (not in `Declared`) is recorded as an entry (`kind` = `"hidden"`): the host does not see that wait.
- **Emits and posts**: a running activation may emit an event (`MaxPosts` in total), an `events.dispatch` nobody waits for, and it goes on. Rust then posts the listener's call to the listener's runtime (an entry). `EmitChain` says whether that call carries the emitter's chain (#561: it does not).
- **Synchronous dispatch**: `bail`, and a Cordis plugin's `waterfall`, make the emitter wait for the whole dispatch; the host calls one listener and waits for it. `Dispatch` happens at a top-level activity only (a plugin's `ctx.emit` / `ctx.bail`); `DispatchHop` says whether Rust records the dispatch itself as a hop. A `bail`'s listener call carries no chain, a `waterfall`'s does (calls back into the emitter's process have to land on the chain it waits for).
- **Session end**: with `Ends`, a session may end while it holds no other session's call: its calls leave the table and calls into it are answered with an error (SYN-9-30).
- **Call ids** are numbered per session; the model also keeps every call's true ancestry (`truth`), apart from the wire `path`.
- **Policies** (`Policy`): `current` detects nothing; `A` refuses synchronous calls between two non-reentrant runtimes; `B` builds a wait graph before each forward and answers `SyncWaitCycle` on a cycle; `Bchain` is B without the stack-order edges.

### Wait graph

Nodes are the calls Rust forwarded and has not answered (`hops`), plus posts nobody waits for (`posts`) and dispatches it does not record (`dispatches`). An edge c → d means c cannot finish before d. A cycle is a deadlock.

| Edge | Meaning | Visible to Rust? |
| --- | --- | --- |
| chain | d was made under c (d's `path` names c) | yes: rewritten paths carry session tags |
| put aside | c entered a non-reentrant, single-stack runtime y that is waiting, c is not on its chain and has not started there; c waits for every call y has out | yes: y's outstanding calls are Rust's own state; `reentrant-sync` and `sync-stack` say which |
| stack order | c and then d entered the same single-stack runtime y and d has started there; c waits for d | yes: frames of a session are ordered |
| dispatch | the host calls a listener inside a synchronous dispatch, and the dispatch waits for that call; a call back into the emitter lands on the chain it waits for | the dispatch itself is not in the table (`DispatchHop` = FALSE); the listener call is the host's own synchronous call (a hop) |
| entry | a call nobody waits for synchronously (`emit`'s `events.call`, an asynchronous call on a marked chain, a synchronous call the host cannot see) is a boundary of chains | it is recorded (`entry`), but it does not count as a call out of its runtime |

The model has two graphs: `TrueSucc`, from the runtimes' real stacks, queues and each node's `real` (someone really waits for it) — only for judging; and `RustSucc`, from Rust's own state and the `entry` flags it recorded — what policy B can do. A node whose `real` is false (a post, an asynchronous call on a marked chain, a call given up locally) is not on a real cycle, but it may be on one in Rust's graph.

## Simplifications

- A runtime's calls may be synchronous, asynchronous (`Async`) or "it waits but the frame does not say so" (`Hidden`); Rust's posts and synchronous dispatches are built as described.
- Calls inside one runtime do not cross Rust and are not modelled.
- Service calls and function (callback) calls are one action; `await`, reference counts and value encoding are not modelled.
- A synchronous dispatch happens at a top-level activity only; `next` (a waterfall's chain) and the terminal are not built: one dispatch has one listener. A listener in the emitter's own runtime is modelled; `next` would add the listener-to-host edge, which the model does not have.
- Cancellation is modelled only as "the caller gives up locally, without `cancel`" (`Abandoners`, open to `Threaded` runtimes only: what Go / Swift / Kotlin do): a late reply is dropped. `cancel` itself, a late `ret` blocking the reader, Node's keyed holds and behind tasks are not modelled.
- A session ends only while it has no call out and takes part in no dispatch (`Ends`); calls into it are answered with an error. A break, a reconnect and `rows.ended` are not modelled.
- Rust threads are not a bounded resource: each forward runs on the thread already waiting for its chain or on a new one (#614's `Workers` and a bounded executor are not modelled).
- Each activation makes one call by default; `policy-b-2node-seq` allows two in sequence.
- Scale: 3 runtimes, chains ≤ 3 hops (`async-policy-b` up to 4), ≤ 2 concurrent top-level activities, ≤ 1 post or dispatch.

## Invariants

| Name | Meaning |
| --- | --- |
| no deadlock | TLC deadlock check: while calls are unfinished, some participant can act. The finished state stutters (`Done`). Every behaviour is finite, so "no deadlock" is equivalent to "every call eventually completes" |
| `RoutingSound` | A call run as part of a waiting chain (nested by a non-reentrant runtime, or handed to a waiting Rust thread) really belongs to that chain by true ancestry, whether or not every node in between is waited for |
| `ChainRecognised` | A non-reentrant runtime never puts aside a call of its own chain: true ancestry, every node of which someone really waits for (posts and the like do not count) |
| `NoFalseCycle` | Policy B answers `SyncWaitCycle` only if the real wait graph plus this call has a cycle |
| `NoLegitimateRefused` | Policy A refuses no legitimate call (forwarding it closes no cycle in the real wait graph) |
| `NeverCycle` | Only to confirm B's check fires (expected to be violated) |
| `NeverNested` | SYN-9-3 (expected to be violated): a reentrant runtime's service is called in the middle of its own synchronous call. Not a defect: an explicit non-guarantee, pinned by a property |

## Results

TLC 2.19 (tla2tools v1.7.4), Temurin 21, 16 cores; `MaxRoots = 2`, `MaxDepth = 3`, `MaxCalls = 1` unless noted.

| Configuration | Runtimes (`n` non-reentrant, `p` reentrant, `g` a thread per call) | Result | Distinct states | Time |
| --- | --- | --- | --- | --- |
| `current-1node-1python` | n, p | pass | 606 | <1 s |
| `current-2node` | n1, n2, p | **deadlock** (counterexample 1) | 1,006 | <1 s |
| `current-1node` | n, p1, p2 | **deadlock** (counterexample 2) | 8,908 | 1 s |
| `current-reentrant` | p1, p2, p3 | pass | 88,824 | <1 s |
| `current-norebase` | n1, n2, p, no `path` rewriting | **`RoutingSound` violated** (counterexample 3) | 268 | <1 s |
| `policy-a-1node` | n, p1, p2 (the combination A intends) | **deadlock** (as counterexample 2) | 9,706 | <1 s |
| `policy-a-2node` | n1, n2, p | **deadlock** (shape of counterexample 2) | 3,066 | <1 s |
| `policy-a-refusals` | n1, n2, p | **`NoLegitimateRefused` violated** (counterexample 4) | 93 | <1 s |
| `policy-bchain-2node` | n1, n2, p | **deadlock** (as counterexample 2) | 5,798 | 1 s |
| `policy-b-2node` | n1, n2, p | pass | 37,311 | 1 s |
| `policy-b-1node` | n, p1, p2 | pass | 55,327 | 1 s |
| `policy-b-nonreentrant` | n1, n2, n3 | pass | 27,000 | 1 s |
| `policy-b-2node-depth4` | n1, n2, p; chains ≤ 4 | pass | 149,659 | 1 s |
| `policy-b-2node-3roots` | n1, n2, p; 3 top-level | pass | 1,190,945 | 8 s |
| `policy-b-1node-3roots` | n, p1, p2; 3 top-level | pass | 2,468,607 | 16 s |
| `policy-b-2node-seq` | n1, n2, p; 2 calls per activation, chains ≤ 2 | pass | 2,549,258 | 19 s |
| `policy-b-acts` | n1, n2, p | `NeverCycle` violated (expected: B fires) | 408 | <1 s |
| `current-posts` | n, p, q (p, q reentrant); 2 top-level calls, 1 emit | **deadlock** (counterexample 5) | 334,773 | 2 s |
| `policy-b-posts` | as `current-posts`, policy B with entries; CI size with `MaxDepth=2` | pass | 1,152,323 | 16 s on 32 cores; estimated 1–2 min on CI 4 cores |
| `policy-b-posts-node` | n, p, q (p non-reentrant, `q` reentrant), policy B; CI size with `MaxDepth=2` | pass | 641,339 | 10 s on 32 cores; estimated 1–2 min on CI 4 cores |
| `threaded-policy-b` | n, g, p (g a thread per call) | pass | 62,128 | <1 s |
| `unmarked-policy-b` | n, g, p; g does not declare `sync-wait` | **deadlock** (counterexample 9, explicit non-guarantee) | 762 | <1 s |
| `bail-policy-b` | r, t; a `bail` dispatch Rust does not record | **deadlock** (counterexample 6) | 449 | <1 s |
| `waterfall-policy-b` | r, t; a `waterfall` dispatch | pass | 3,277 | <1 s |
| `bail-recorded` | r, t; `bail`, Rust records the dispatch as a hop | pass | 3,494 | <1 s |
| `emit-unchained` | n, p, q; an emit's listener call carries no chain (`EmitChain` FALSE), 1 top-level | pass | 69,591 | 2 s |
| `host-call` | n1, p; the host's own synchronous call | pass | 5,208 | <1 s |
| `before-host-call` | as above, before #597 (an entry) | pass | 5,208 | <1 s |
| `abandon-policy-b` | n, g, p; g gives up a synchronous call locally | **`NoFalseCycle` violated** (counterexample 7) | 71,002 | 1 s |
| `hidden-policy-b` | n, p, q; p calls synchronously from another thread | **`ChainRecognised` violated** (counterexample 10, explicit non-guarantee) | 772 | <1 s |
| `async-policy-b` | n, p, q; an asynchronous call on a marked chain | **`NoFalseCycle` violated** (counterexample 8) | 29,660 | 1 s |
| `nested-service` | p1, p2 | `NeverNested` violated (expected, SYN-9-3) | 118 | <1 s |
| `end-session` | n1, n2, p; a session may end | pass | 84,695 | 1 s |

"Pass" means exhaustive: no deadlock, all listed invariants hold; state counts on these rows are exact. On deadlock and violation rows TLC stops at the first counterexample, so the count depends on the order of the search and varies between runs; treat it as indicative.

The first 20 rows are the #535 results and state counts. Records now carry more fields (`kind`, `entry`, `real`, `mk`), so the same configurations take somewhat more states; these were rerun during the extension and every result matched the table; `policy-b-posts` and `policy-b-posts-node` use `MaxDepth=2` for CI and the table shows the reduced counts. Every new configuration was run to completion within a minute.

`current-1node-1python` backs the statement in §9 of the multi-language design that one Node and one Python runtime do not deadlock. With chains ≤ 3, `policy-b-2node-seq` passed 44 million states in 10 minutes and was still growing, so it runs with chains ≤ 2.

### Counterexamples

1. **Two Node runtimes calling each other** (`current-2node`). n1 synchronously calls a service of n2 while n2 synchronously calls a service of n1. Neither call is on the other's chain; each is put aside; both wait forever. The case §9 already knows.
2. **Stack order on a reentrant runtime** (`current-1node`; A and Bchain alike).
   1. n synchronously calls service `s1` of p2;
   2. `s1` synchronously calls p1;
   3. p1 itself (a timer, say) synchronously calls service `s2` of p2. p2 is waiting and reentrant, so it runs `s2` nested above `s1`;
   4. `s2` synchronously calls a service of n. n is waiting for `s1`; the call is not on its chain and is put aside;
   5. `s1`'s call has returned, but `s2` is on top, so `s1` waits for `s2`; `s2` waits for n; n waits for `s1`. Deadlock.

   So "all leaf runtimes reentrant" does not remove deadlocks by itself: reentrancy turns "put aside" into "buried under", and one non-reentrant runtime is enough to close a cycle. It happens with a single Cordis runtime.
3. **No `path` rewriting** (`current-norebase`). n1 and n2 each wait for their own `node:1`; n1's call reaches n2 with `path = ["node:1"]` and n2 takes it for a call on its own `node:1` chain and runs it nested. This is what today's `rebase` prevents.
4. **A refuses a call that cannot deadlock** (`policy-a-refusals`). n1 synchronously calls n2 while n2 is idle and nothing else runs; A refuses it.
5. **A post on top of a wait** (`current-posts`). n synchronously calls `x` in p, and `x` synchronously calls q. Meanwhile q emits an event whose listener is in p, and Rust posts the listener's call to p. p is reentrant, so it runs the call on top of `x`. The listener synchronously calls n; n is waiting for `x` and puts the call aside. `x` is buried: n waits for `x`, `x` for the listener, the listener for n. No caller waits for the post, so the old graph does not have it and misses the cycle; recorded as an entry (`policy-b-posts`), the listener's call is refused. With no chain on the listener call (`emit-unchained`, #561) the cycle is still found at this scale: those edges come from "put aside" and stack order, not from the emitter's chain.
6. **A blind spot on synchronous dispatch** (`bail-policy-b`, 2026-11-09). r (non-reentrant) makes a `bail` dispatch: r waits for it, the host calls a listener in t and waits for it, and the listener synchronously calls r. `waits.rs` does not record the dispatch as a hop, and a `bail`'s listener call carries no chain (the `Mode::Bail` branch pushes no `Waiting`), so r does not recognise the callback and puts it aside: r waits for the dispatch, the dispatch for the listener, the listener for r. Policy B does not see this cycle; deadlock.
   - `waterfall-policy-b` is the same shape with `waterfall-sync`: its calls back into the emitter carry the emitter's wait chain, so r runs the callback nested; it passes.
   - `bail-recorded` is a candidate fix: Rust puts the dispatch's call id on the listener's `path` (as if the dispatch were a forwarded call), so r recognises the callback; it passes. Conclusion: a `bail` dispatch has to be visible in the wait graph, or §5.4's "every synchronous call takes part in the cycle check" does not hold for `bail`.
7. **The edge left after a local give-up** (`abandon-policy-b`). Go's context, Swift's Task and Kotlin's interrupt give up a synchronous call without a `cancel`: the caller's thread goes on, and the call stays in the host's table until the reply arrives. In the graph it is still "a call out of that runtime", so B may refuse a call that would have finished (violating `NoFalseCycle`). The fix is to send `cancel` when giving up (the rule #561 settled), or to accept the false positive.
8. **An asynchronous call on a marked chain** (`async-policy-b`, 854bef5d). The caller does not wait for it, but on a marked chain `waits.rs` still records it as a hop. n is waiting for a call; p's asynchronous call enters n and is put aside, while Rust's graph has it as "a call out of n" plus this new call, so a call that would finish is refused (violating `NoFalseCycle`). The fix needs the call frame to tell "waiting synchronously" from "fired and forgotten".
9. **A runtime without `sync-wait`** (`unmarked-policy-b`, SYN-9-26). g's synchronous calls carry no `sync`, the host does not record them, and the whole session is outside the check; the n → g → p → n cycle is not found and deadlocks. This is an **explicit non-guarantee**: an older SDK can still deadlock as before, but nothing is falsely refused.
10. **A synchronous call from another thread** (`hidden-policy-b`). Python calls synchronously from another thread (`run_coroutine_threadsafe`); the frame carries no `sync`, so the host does not see the wait. The call enters a waiting non-reentrant runtime and is put aside; `ChainRecognised` is violated (the first step of a deadlock). This is an **explicit non-guarantee**.
11. **A reentrant runtime called mid-call** (`nested-service`, SYN-9-3). p1 synchronously calls a service of p2; inside that call p2 synchronously calls p1; another activity of p1 synchronously calls p2, and p2, reentrant, nests it above the call it is running. `NeverNested` is violated: an **explicit non-guarantee** ("do not hold a lock while calling a rutis service" cannot be enforced), pinned by a property that is expected to be violated.

## Recommendation: policy B (with stack order)

Adopted by the maintainer (written into §9 of the [multi-language design](../../docs/design-multilang-runtimes-2026-10-03.en.md); implementation in #228).

The reasons (as of #535): A is not enough; within the model B removes the deadlocks and raises no false alarm; B uses only what Rust has; the stack-order edges cannot be left out; a new language's runtime may choose to be reentrant or not, as long as it says so.

What an implementation of B needs (the model's premises):

1. one host-wide table of every forwarded synchronous call not yet returned (source session and id, target session and id, the chain with tags resolved);
2. replies recorded in frame order: on one session, an earlier reply leaves the table before a later call does;
3. a check before each forward, answering `SyncWaitCycle` with the calls of the cycle instead of forwarding;
4. reliable reentrancy and stack information: `reentrant-sync`, `sync-stack` (Node declares no `reentrant-sync`; Go / Swift / Kotlin declare no `sync-stack`);
5. only **synchronous** waits in the graph (see "Open questions").

## Open questions and what is not covered

- **Telling synchronous from asynchronous (settled, modelled)**: the call frame's `sync` field says the caller waits synchronously; only calls with it, and calls whose chain's last call is marked, enter the graph (#228, 854bef5d). The model shows that recording an asynchronous call on a marked chain as a hop raises false alarms (counterexample 8): either fix the implementation or count only `sync` calls.
- **Synchronous dispatch has to be visible (new, counterexample 6)**: a `bail` dispatch and its listener call must be connected in the graph (recorded as a hop, with the emitter's wait id on the listener's `path`), or a deadlock goes undetected. `waterfall-sync` passes today because calls back into the emitter's process carry its innermost wait chain.
- **#614 still not modelled**: the host's `Workers` (bounded worker threads, and `policy-b-workers1`), the asynchronous `serial` / `parallel` / `waterfall` dispatch (executions on the host as a participant), `cancel` and late `ret`s, Node's keyed holds and behind tasks, a Cordis node's connection session (while waiting on one session it reads no other), cycles across hosts, bounded thread pools on the runtime side (Kotlin's IO pool, Swift's cooperative pool). Also the `before-lost-chain` and `before-cancel` configurations, the differential property test for `cycle_among`, and trace validation of the wait graph.
- **Synchronous calls made by Rust itself** are modelled as the host's calls (`host-call`, `before-host-call`), but the host's thread count is not bounded.
- **B's cost**: a reachability check on the global table before every synchronous forward; the table's size is the number of synchronous calls in progress. The model does not measure time.
- A caller that retries at once after `SyncWaitCycle` may cycle again; whether to back off is the caller's decision, not modelled.

## How to run

Java 11 or later and `tla2tools.jar` ([v1.7.4](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4), sha256 `936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`). Do not commit the jar.

```sh
mise install java@temurin-21
curl -sSLo /tmp/tla2tools.jar https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar
TLA2TOOLS=/tmp/tla2tools.jar mise exec java@temurin-21 -- specs/cross-runtime-sync/run.sh            # every configuration
TLA2TOOLS=/tmp/tla2tools.jar mise exec java@temurin-21 -- specs/cross-runtime-sync/run.sh bail-policy-b   # one
```

`run.sh` compares each configuration's result with `expected.txt` and exits 0 when all match; logs and counterexamples go to `TLC_OUT` (a temporary directory by default). Without the script, in this directory:

```sh
java -cp /tmp/tla2tools.jar tlc2.TLC -workers auto -config bail-policy-b.cfg CrossRuntimeSync.tla
```

Each new configuration finishes within a minute; `policy-b-posts` and `policy-b-posts-node` take about ten seconds locally after reducing to `MaxDepth=2` (estimated 1–2 minutes on CI 4 cores); `policy-b-1node-3roots` and `policy-b-2node-seq` take a dozen or so seconds.

## Maintenance

When §5 or §9 of the multi-language design or §7 of M1 changes, or when `rpc.rs`, `waits.rs`, `events/hub.rs`, `session.mjs` or `peer.py` change which calls run during a synchronous wait, or which calls enter the wait graph, change this model and `expected.txt` together (standard Q11.2.3).
