# Event close admission: two-phase marker publication (TLA+)

[中文](README.md)

TLC explores every interleaving of a fiber subtree closing and dispatchers admitting listener calls. Issue #1025 (design), #613 (model scope); quality standard Q11.2; design: [event listener close admission without walking the ancestor chain](../../docs/design-event-close-admission-2026-11-08.en.md) §2, §3. This is the first block of `specs/event-close/`: it answers close admission only. The unified event table, emit queues, node links and row lifecycle are later blocks of #613.

## Design and code it corresponds to

| Part of the model | Design | Implementation |
| --- | --- | --- |
| Closer: fix the subtree under admission, write `lineage_closed` for every member in postorder, then publish `closing`, then schedule cleanup | design §2 D1, D2; §3 rules 1-3 | `crates/rutis/src/fiber.rs` `mark_subtree_lineage`, `publish_subtree_closing`, `FiberInner::shutdown`, `begin_subtree_shutdown_locked`; `crates/rutis/src/ctx.rs` `CtxInner::shutdown` |
| Dispatcher: after selecting an entry, the final `Entry::live` reads only the owner's own markers, not the parent chain | design §2 D3; §3 rules 4-5 | `crates/rutis/src/bus/table.rs` `Entry::live`, `Entry::start`, `CallFlight`; `crates/rutis/src/bus.rs` `start` |
| A fiber created under a closed root starts with `lineage_closed = true` | design §2 D1; §3 rule 3 | `crates/rutis/src/fiber.rs` `spawn_fiber_inner` (`is_closed`, `AtomicBool::new(closed)`) |
| An admitted call is waited for; cleanup does not overtake its call flight | existing §7.1; design §3 rules 4-5 | `crates/rutis/src/fiber.rs` `begin_event`, `finish_event`, `wait_events` |

The design document's status is "core implemented, verification in progress" (updated 2026-11-09): the code has `lineage_closed`, the two-phase publication, the deterministic race tests and the shuttle model. This model follows the code.

## The model

- **Participants**: closer, dispatcher, cleanup and creation. One close pass at a time, sharing `Shared.admission` with creation: a pass runs from collecting the subtree to scheduling cleanup, and no `Spawn` or second close happens meanwhile. The dispatcher takes no lock.
- **Tree**: `r → a → {b, c}`, `b → {d, n}`, `r → m`. `r, a, b, c, d` exist initially; `Spawn` creates `n` and `m`. One entry per fiber stands for "the listeners of that fiber"; the model does not care how many listeners a fiber holds.
- **Closer** (`Collect`, `Mark`, `MarkingDone`, `Publish`, `Finish`):
  - `Collect(f)` runs when `f ∈ CloseRoots` and `phase = "idle"` and fixes `opMembers = Below(f) ∩ exists \ cleaned`;
  - `Mark(f)` runs only for a member whose children in `opMembers` are all marked, and writes `lineage_closed`;
  - `MarkingDone` requires every member marked, records `committed`, and publishes `Shared.closing` for a root close. This is the shutdown linearization point; no per-fiber `closing` is visible yet;
  - `Publish(f)` publishes `closing` only in `phase = "publishing"` (a mutation allows it during the marker pass);
  - `Finish` requires every member published and schedules cleanup through the shutdown task slot; a repeated close does not queue a member twice.
- **Dispatcher** (`Select`, `Start`, `Complete`): `Select` holds an entry, which may come from a stale snapshot; `Start` begins the `CallFlight` and then reads the owner's `alive`, `cleaned`, `closing` and `lineage_closed`. Not live: the flight is dropped and the listener is not called. Live: the call is `running` until `Complete`. An admitted call outlives the close commit; cleanup waits for it.
- **Cleanup** (`Cleanup`): takes a fiber from the queue only when it has no `running` call; a repeated close cannot run it twice.
- **Creation** (`Spawn`): adds `n` or `m` to `exists` while `phase = "idle"`; if `Shared.closing` is published or the parent is marked, the new fiber starts with `lineage_closed` set.

Each action is atomic. `Select` may pick an entry after its owner was marked, standing for a dispatcher holding an old snapshot; the final check decides.

## Simplifications

- One entry per fiber. Registration, withdrawal, `once`, generation, `withdrawn` and `live_generation` are left out; entries never withdraw, so `withdrawn` is always false here.
- No `emit` / `serial` / `parallel` / `bail` / `waterfall`, no snapshot list or cache, no filters, no observers; only "select one entry, then start one call of it".
- Each action is atomic; `Shared.admission` shows up only as "no creation and no second pass while a pass runs". The real critical section, `pre_cancel` and the task tree are left out.
- Memory order is not modelled: the first version uses `SeqCst`, and the model assumes the action order for the marker store and the final load. The Rust interleavings and the `crate::sync::AtomicBool` switch points belong to #622's shuttle model.
- The shutdown deadline is not modelled: `wait_events` waits at most `RUTIS_SHUTDOWN_TIMEOUT`, then reports and unloads anyway. The model waits without a bound, so `NoCleanupWithFlight` and `AdmittedComplete` describe the rules before that deadline. What happens to a callback after it is the existing rule, not this model.
- `release_transient` now detaches a child under `Shared.admission` (`fiber.rs`, 2026-11-09): the model counts the detach as part of the lock and does not model it separately. The implementation provides the fixed-subtree premise; the details inside the lock (drain, lock order) are left out.
- No time, faults, remote listeners, process boundaries, payloads or error contents.

## Invariants and liveness

| Name | Meaning |
| --- | --- |
| `ClosedDownward` | Every created descendant of a marked fiber is marked: never an ancestor closed with a subtree still unmarked |
| `ClosingImpliesMarked` | Every published `closing` belongs to a marked fiber: the marker pass precedes public closure |
| `CleanupAfterMarkers` / `CleanupAfterPublish` | Cleanup runs only after the marker and the `closing` flag |
| `NoCleanupWithFlight` | A fiber is cleaned only with no call of it `running`: cleanup does not overtake an admitted call (before the deadline) |
| `CleanupOnce` | Cleanup runs at most once per fiber: repeated closes, of the fiber or an ancestor, reuse the scheduled task |
| `NoAdmitMarkedOwner` | A call admitted by the final check had an owner without `lineage_closed` at that moment |
| `CommittedStaysOut` | After a close pass commits, no new call in its fixed subtree is admitted, even before `closing` is published |
| `SharedClosedImpliesMarked` | `Shared.closing` is published only after the root's marker pass |
| `AdmittedComplete` | Liveness: every admitted call eventually finishes (runs without a deadline or faults only) |

## Results

TLC 2.19 (tla2tools v1.7.4), Java 21.0.12 (Temurin), 32 cores, `-workers auto`. "Pass" means the search completed and every listed property holds. A violated configuration stops at the first counterexample, so its state count depends on the search order and may differ between runs. The whole directory takes about 20 s.

| Configuration | Content | Outcome | Distinct states | Time |
| --- | --- | --- | --- | --- |
| `close-healthy` | Close only the `b` subtree; `n`/`m` may be created; repeated closes; safety invariants | pass | 2,263,040 | 10 s |
| `close-tree` | Close only the root (the whole tree); no creation; repeated closes; safety invariants | pass | 688,580 | 5 s |
| `close-drain` | Close only the `b` subtree; no creation; `AdmittedComplete` only | pass | 31,488 | 1 s |
| `close-mut-early-closing` | Publish `closing` during the marker pass | violates `ClosingImpliesMarked` | 204 (stops at the counterexample) | <1 s |
| `close-mut-missed-member` | The collection of `b`'s subtree misses `d` | violates `CommittedStaysOut` | 824 (stops at the counterexample) | <1 s |
| `close-mut-ignores-marker` | The final check reads `closing` but not `lineage_closed` | violates `CommittedStaysOut` | 718 (stops at the counterexample) | <1 s |
| `close-mut-reclose` | A repeated close queues its members again | violates `CleanupOnce` | 19,780 (stops at the counterexample) | <1 s |

`close-healthy` first included liveness in the same configuration; the state count was the same and it took about 40 s longer. Liveness now has its own small `close-drain`, keeping the pull-request run bounded.

## Mutations and counterexamples

Every checked property has a deliberate error that TLC must find (`violated:<name>` in `expected.txt`):

| Configuration | Mutation | Violated | Counterexample |
| --- | --- | --- | --- |
| `close-mut-early-closing` | `Publish` is allowed in `phase = "marking"` | `ClosingImpliesMarked` | `Collect(a)`, then `Publish(a)`: `closing = {a}` while `marked = {}`. Public closure moves ahead of the marker pass, changing the old close boundary |
| `close-mut-missed-member` | Collecting `b` leaves `d` out of `opMembers` | `CommittedStaysOut` | `Collect(b)`, `Mark(b)`, `MarkingDone` commits `{b}`; `d` has neither marker nor closing, and `Start(d)` after `Select(d)` passes the final check with `afterCommit[d] = TRUE` |
| `close-mut-ignores-marker` | The final check ignores `lineage_closed` | `CommittedStaysOut` | `Collect(b)` marks `{b,d}` and `MarkingDone` commits before `closing` is published; `Select(b)` then `Start(b)` is admitted although `b` is marked and its close committed |
| `close-mut-reclose` | `Finish` ignores the shutdown task slot and queues again | `CleanupOnce` | Close `b` twice; `d`'s queue count becomes 2; `Cleanup(d)` runs twice and `cleanupCount[d] = 2` |

`close-mut-missed-member` also breaks `ClosedDownward` (`b` marked, `d` not); that configuration lists only the intended property. `close-mut-ignores-marker` also breaks `NoAdmitMarkedOwner`; again only the intended one is listed, so each counterexample maps to one conclusion.

## Conclusion

- With design D2's two-phase publication, the correct protocol holds in the explored state space: postorder marking leaves no window with an ancestor closed and a descendant unmarked; the end of the marker pass is the admission boundary, and `closing` may be published later; `Entry::live` reads only its owner's markers and `closing`, with no parent walk; repeated closes reuse the shutdown task, and cleanup runs once.
- The two `CommittedStaysOut` counterexamples pin the two premises of the design: the fixed subtree must contain every member (`release_transient` detaches under admission), and the final check must read `lineage_closed` rather than only the not-yet-published `closing`. `ClosingImpliesMarked` pins the publication order; `CleanupOnce` pins repeated-close idempotence. None of the four properties is empty.
- What the model does not prove: memory orders other than `SeqCst`, real scheduler switch points (#622's shuttle), behaviour after the shutdown deadline, dynamic registration/withdrawal/generations, the other event dispatch modes, and #613's event table, emit queues, node links, row lifecycle and call lifecycle. This directory is the first block of #613, not evidence for the whole event model.

## How to run

Needs Java 11+ and `tla2tools.jar` ([v1.7.4](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4), sha256 `936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`). Do not commit the jar.

```sh
curl -sSLo /tmp/tla2tools.jar https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar
TLA2TOOLS=/tmp/tla2tools.jar specs/event-close/run.sh                    # every configuration
TLA2TOOLS=/tmp/tla2tools.jar specs/event-close/run.sh close-healthy      # one configuration
TLC_WORKERS=4 TLA2TOOLS=/tmp/tla2tools.jar specs/event-close/run.sh      # limit TLC's workers
```

`run.sh` compares each configuration's outcome with `expected.txt` and exits 0 when all match; logs and counterexamples go to `TLC_OUT` (a temporary directory by default). CI's `specs` job matrix needs `event-close` added (the main thread does it; [CI](../../docs/ci.en.md) §4).

## Maintenance

Change this model and `expected.txt` together with design §2 D1-D5, §3 rules 1-5, `mark_subtree_lineage` / `publish_subtree_closing` / `shutdown` in `fiber.rs`, `Entry::live` / `Entry::start` in `bus/table.rs`, or the locking of `release_transient` (standard Q11.2.3).
