# Event listener close admission without an ancestor walk

[中文](design-event-close-admission-2026-11-08.md)

Status: draft, not implemented. Date: 2026-11-08.
Issue: [#1025](https://github.com/arcships/rutis/issues/1025). Implementation issues: #1025-core, #1025-tests, and #1025-benchmark (to be filed under the 0.22 plan).

## 1. Where things stand / what to solve

Every selected event listener currently asks whether its owner is open by reading the owner's state and then `Ctx::registration_open()`. The latter reads global closure and walks every ancestor (`crates/rutis/src/ctx.rs:191-207`); `Entry::live()` does this in `crates/rutis/src/bus/table.rs:77-90`. On the `close_cpu` workload (1,000 instances × 8 rows × 8,000 listeners), the 2026-11-02 release sample spent about 38.2s creating instances; the registration-open walk was about 9.7s (24%) and each listener call loaded two or three cold ancestor cache lines. Issue #1025's target is ≤30s creation on this workload; acceptance must also meet the existing CREATE_RATIO, CLOSE_RATIO and IDLE_SHARE thresholds without changing event or shutdown semantics.

Shutdown already holds `Shared.admission` while it marks a subtree and starts shutdown (`crates/rutis/src/fiber.rs:1756-1780`); root shutdown holds it while setting `Shared.closing` (`crates/rutis/src/ctx.rs:1318-1338`). Event snapshot/selection does not hold that mutex, and `Entry::start()` deliberately begins a call flight before its final liveness check. A marker added without a publication protocol could let an ancestor be closed while a descendant still appears live.

## 2. Decisions

| Decision | Choice | Why; what is dropped |
| --- | --- | --- |
| D1 | Add permanent `FiberInner::lineage_closed: AtomicBool`. It is true for a permanently closed fiber and every member of a subtree at its shutdown commit; it is initialized true for fibers created after the root is closed and is never reset by a generation reload. | A per-entry marker and an inherited shared marker are dropped: entries are dynamic, and a shared marker would close siblings when only one subtree closes. |
| D2 | Publish markers in a two-phase shutdown pass under `Shared.admission`: collect the fixed subtree; store `lineage_closed=true` for every member first; only after all stores, publish the existing per-fiber `closing=true` flags and (for root) `Shared.closing=true`; then enqueue cleanup. The shutdown linearization point is the completion of the marker pass, not the first ancestor flag. | A naive ancestor-to-child flag propagation is dropped because it creates an ancestor-closed/child-live window. A postorder-only public closing change is dropped because it moves the existing close boundary. |
| D3 | `Entry::live()` reads `withdrawn`, owner upgrade, `alive`, `owner.closing`, `owner.lineage_closed`, and `live_generation`; it no longer walks the context parent chain. `Entry::start()` keeps `CallFlight::begin(owner)` followed by the final `live()` check. | Removing the final check or the event flight is dropped: admitted calls must still drain; no call may be admitted after the marker commit, while an already admitted call may begin after the commit and finish under the existing rules. |
| D4 | Keep `Ctx::registration_open()` and its ancestor walk for registration/service APIs. This optimization changes only listener liveness, not public registration errors or instance-event admission. | Replacing all lifecycle checks with the listener marker is dropped because registration has a separate API contract. |
| D5 | Use `SeqCst` in the first implementation. No event snapshot lock or blocking admission lock is added to the hot path. The two-phase marker stores give the event path a stable commit boundary; `Entry::start` rejects new admission if it observes the marker after commit. | Holding `Shared.admission` while every dispatch selects a table would add a lock to every emit and risks missing the CPU target. A new memory-order proof is deferred. |

## 3. Rules

1. Before a non-root subtree closes, its shutdown path holds `Shared.admission`, collects every currently attached descendant, stores `lineage_closed=true` on all of them, then stores each existing `closing=true` and calls `pre_cancel`; only then calls `begin_subtree_shutdown_locked`. The collector and child links use the same admission lock as fiber creation/detachment, so the set is fixed.
2. Root shutdown performs the same root-plus-descendants marker pass, then stores `Shared.closing=true`, pre-cancels root/descendants and posts the existing shutdown intent. Root descendants must be marked even though old root shutdown used only `Shared.closing`.
3. A fiber created while the shared root is already closing starts disposed with `lineage_closed=true`. A permanently closed marker is never cleared during restart/update; a new fiber receives a new marker.
4. The marker pass is the closure admission boundary: calls whose final `Entry::live` marker load occurs before completion may be admitted and are waited on normally; a check after completion admits no new call. A selected entry may be stale; `Entry::start` is the authoritative final check. An already admitted callback may begin after its owner's marker commit but must drain under the existing rules; no new callback is admitted after the commit.
5. `Entry::start` begins `CallFlight` before `live`; a closed result immediately drops the flight and calls no listener. Accepted calls retain the existing `event_flights` wait and cancellation behaviour. `once` is claimed only after `start`, so a skipped listener is not consumed.
6. The marker is permanent for a subtree shutdown. An ordinary generation unload/reload keeps the fiber available to future generations only when it has not been permanently shut down; `lineage_closed` is not reset by reload. `withdrawn`, `alive`, `closing` and `live_generation` retain their existing meanings.
7. Registration still takes the existing admission lock and repeats its lifecycle check at commit. A preflight that reads open while the marker pass runs cannot commit through the lock; no new registration is admitted into the closing subtree.
8. Existing instance-event rules remain: instance dispatch stops admission, waits related admitted dispatches, and returns `Closed` after closure. This optimization concerns non-instance listener `Entry::live`; it does not alter the instance permit contract.
9. The listener callback, event order, dispatch modes, cross-process protocol, cancellation deadline, and shutdown output do not change. A separate process or runtime sees no new message.
10. If implementation uses a retrying sequence guard rather than only the two-phase marker, it must keep the same rule: an event path may not return a live result from a mixed publication. It may use a `shutdown_sequence` odd/even guard with `SeqCst`, but must benchmark spin cost and prove no unbounded wait. The default implementation is the marker-first two-phase pass.

## 4. What developers see

### Plugin authors

There is no API or output change. A listener registered by a running row receives exactly the same events and errors. When its ancestor is permanently shut down, an event emitted after the marker commit does not call it; an instance event emitted after closure still returns `ErrorKind::Closed`, with the text `root has been shut down` and no new `code`/`data`. In-flight callbacks finish or are cancelled by the existing shutdown deadline.

Concepts added: none. `lineage_closed` is an internal performance marker.

### People assembling plugins with `rutis-host`

There is no configuration change and no new status line. `rutis-host status` continues to print the existing row states (`running`, `stopping`, `stopped`, or the existing failure headline). The event-close optimization has no host error code or migration setting.

Concepts added: none.

### Host owners embedding rutis

Public Rust APIs, event protocol and shutdown results are unchanged. The existing guarantee remains: after subtree admission closes, no new listener callback is admitted; an already admitted call may begin after the commit but drains according to the existing wait/cancel rules. There is no new error code or data field.

Concepts added: none; maintainers gain only an internal owner marker and a closure linearization rule.

### People deploying

No deployment setting, wire version or process boundary changes. The release artifact behaves as before, with lower instance-creation CPU in the stress benchmark. There is no visible success or error output change.

Concepts added: none.

## 5. Compatibility and migration

- The public API, event protocol, listener ordering, shutdown state/output and error codes are unchanged.
- Existing listener code does not need migration. The only compatibility requirement is that post-closure events remain excluded and admitted calls still drain.
- `close_cpu` remains a release benchmark. Compare creation CPU and close CPU against the existing `CREATE_RATIO=300`, `CLOSE_RATIO=200`, `IDLE_SHARE=5%` thresholds; do not trade away close semantics to improve creation.
- No package-version or runtime protocol field is added by this internal change. The 0.22 release notes mention the performance fix and retain the existing event shutdown guarantee.

## 6. Implementation

1. **Core marker and publication (#1025-core).** Add `lineage_closed` to `FiberInner`, initialize it from the initial closed state, centralize non-root and root shutdown marker publication under `Shared.admission`, mark the fixed subtree before existing `closing`/`Shared.closing` publication, and keep cleanup ordering unchanged. Remove `owner.ctx.registration_open()` only from `Entry::live`. Done: source-level lock/order review proves all subtree members are marked before the close boundary; generation reload never clears the marker.
2. **Event race tests (#1025-tests).** Add a deterministic parent→child listener test: complete parent shutdown's marker pass, immediately emit from an unaffected context, and assert zero child calls; add root shutdown, nested subtree, sibling, Loading-owner, repeated-shutdown and remote-entry coverage. Use the testing selection hook to select an entry, shut down before `Entry::start`, and assert the marker final check skips it; verify an accepted in-flight callback still drains. Done: no new callback is admitted after commit in sync, `emit`, `serial`, `parallel`, `bail` and waterfall paths; an admitted callback may begin after commit but drains under the existing rules.
3. **Benchmark and CI (#1025-benchmark).** Run `cargo test -p rutis-loader --release --test close_cpu -- --nocapture`; compare creation and close CPU and queued-emits memory. The acceptance job is the Ubuntu 24.04 release job in `.github/workflows/stress.yml:381-417`, which runs `close_cpu` and then `queued_emits_memory`; its summary must retain both results. Update the 0.22 performance record/CI summary only with measured values. Done: record the issue workload's ≤30s creation-time target; the automated test continues to compare the existing `CREATE_RATIO`, `CLOSE_RATIO` and `IDLE_SHARE` thresholds, with no close-CPU regression and the queued-emits memory result retained.
4. **Docs/release (#1025-docs).** Keep this document and its Chinese counterpart synchronized; add the 0.22 release-note line and link the unchanged event shutdown guarantee. Done: bilingual review confirms no new developer-visible behavior is implied.

## 7. Design-philosophy check

The optimization is implemented once in the Rust core, opens no broad hook, pins the closure boundary and memory ordering in tests, and makes no cross-process promise beyond the existing contract. The only behavior that cannot be made faster without risk—the exact shutdown admission boundary—is stated as a rule rather than silently changed. This follows design-philosophy §7 principles 1, 3, 4, 7 and 10.
