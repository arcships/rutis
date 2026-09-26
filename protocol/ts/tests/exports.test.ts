import assert from 'node:assert/strict'
import { test } from 'node:test'
import { Exports, ObjectIds, type PinKey } from '../src/exports.ts'
import { Context } from '@deepseek-ai/cordis'
import { ManagedActivation } from '../src/managed.ts'
import { CallContext, withNative, type Caller } from '../src/sdk.ts'
import { exportBorrowCallback0 } from '../generated/rpc.ts'

const owner = (activation: string) => ({ runtime: 'owner', epoch: '1', activation })
const delivery = (id: string): PinKey => ({ type: 'delivery', recipient: owner('9'), id })
const execution = (call: string): PinKey => ({ type: 'execution', caller: owner('9'), call })
test('real JS object identity is stable; execution outlives delivery release and close', async () => {
  const ids = new ObjectIds()
  const table = new Exports(owner('1'), ids)
  const other = new Exports(owner('2'), ids)
  const object = { state: 1 }
  const id = table.register(object)
  assert.equal(table.register(object), id)
  assert.notDeepEqual(table.register({ state: 1 }), id)
  assert.ok(BigInt(other.register(object).object) > BigInt(id.object))
  table.pin(id, delivery('1')); table.pin(id, delivery('1')); table.pin(id, delivery('2')); table.pin(id, execution('1'))
  table.release(delivery('1')); table.release(delivery('1')); table.close()
  assert.equal(table.pins(id), 1)
  assert.equal(table.executionObject(execution('1')), object)
  assert.throws(() => table.pin(id, delivery('3')), (e: any) => e.code === 'ScopeClosed')
  let joined = false
  const join = table.join().then(() => { joined = true })
  await Promise.resolve(); assert.equal(joined, false)
  table.release(execution('1')); await join
  assert.throws(() => table.executionObject(execution('1')), (e: any) => e.code === 'StaleObject')
})
test('shared local object keeps identity through unpinned intervals', async () => {
  const table = new Exports(owner('1'), new ObjectIds())
  const object = { live: true }
  const id = table.register(object)
  table.pin(id, delivery('1')); table.release(delivery('1'))
  assert.equal(table.register(object), id)
  table.pin(id, delivery('2')); table.release(delivery('2')); await table.join()
  assert.equal(object.live, true)
})
test('exclusive disposer is awaited once, even after shutdown admission closes', async () => {
  const table = new Exports(owner('1'), new ObjectIds())
  const object = { connection: true }
  let calls = 0
  let finish!: () => void
  const waiting = new Promise<void>(resolve => { finish = resolve })
  const id = table.registerExclusive(object, async actual => {
    assert.equal(actual, object); calls++; await waiting
  })
  assert.equal(table.register(object), id)
  table.pin(id, delivery('1')); table.pin(id, execution('1')); table.close()
  let done = false
  const join = table.join().then(() => { done = true })
  await Promise.resolve(); assert.equal(calls, 0)
  table.release(execution('1')); await Promise.resolve()
  assert.equal(calls, 1); assert.equal(done, false)
  table.release(execution('1')); table.close(); finish(); await join; await table.join()
  assert.equal(calls, 1)
})
test('failed disposer is reported and a disposed identity cannot be exported again', async () => {
  const table = new Exports(owner('1'), new ObjectIds())
  const object = {}
  const error = new Error('failed disposer')
  const id = table.registerExclusive(object, () => { throw error })
  table.pin(id, delivery('1')); table.release(delivery('1'))
  await assert.rejects(table.join(), e => e === error)
  await assert.rejects(table.join(), e => e === error)
  assert.throws(() => table.register(object), (e: any) => e.code === 'StaleObject')
})
test('released pins cannot revive, including after confirmed terminal retirement', async () => {
  const table = new Exports(owner('1'), new ObjectIds())
  const object = {}
  const id = table.register(object)
  table.pin(id, delivery('1')); table.pin(id, delivery('2')); table.release(delivery('2'))
  assert.throws(() => table.pin(id, delivery('2')), (e: any) => e.code === 'StaleObject')
  assert.throws(() => table.retireDeliveries('owner', '1', '2'), (e: any) => e.code === 'InvalidParams')
  table.release(delivery('1')); table.retireDeliveries('owner', '1', '2'); table.retireDeliveries('owner', '1', '2')
  assert.throws(() => table.pin(id, delivery('1')), (e: any) => e.code === 'StaleObject')
  table.release(delivery('1')); table.pin(id, delivery('3')); assert.equal(table.pins(id), 1)
  table.close(); await table.join()
})
test('native Cordis context closes export admission and awaits execution plus disposer', async () => {
  const root = new Context()
  const remove = root.provide('dependency', {})
  let ctx!: Context
  const managed = new ManagedActivation(root, { inject: ['dependency'], apply(current) { ctx = current } }, {})
  await managed.ready()
  const table = Exports.managed(ctx, owner('1'), new ObjectIds(), () => managed.isOpen)
  const object = { connection: true }
  let entered!: () => void
  const started = new Promise<void>(resolve => { entered = resolve })
  let finish!: () => void
  const waiting = new Promise<void>(resolve => { finish = resolve })
  const id = table.registerExclusive(object, async () => { entered(); await waiting })
  table.pin(id, delivery('1')); table.pin(id, execution('1'))
  const dependencyCleanup = remove()
  assert.throws(() => table.register(object), (e: any) => e.code === 'ScopeClosed')
  assert.throws(() => table.pin(id, { type: 'staging', id: '1' }), (e: any) => e.code === 'ScopeClosed')
  assert.equal(table.executionObject(execution('1')), object)
  let stopped = false
  const stop = managed.stop().then(() => { stopped = true })
  await Promise.resolve(); assert.equal(stopped, false)
  table.release(execution('1')); await started
  assert.equal(stopped, false); finish(); await Promise.all([stop, dependencyCleanup])
  await root.fiber.dispose()
})
test('recipient epoch closure reclaims delivery records while keeping executing borrows', async () => {
  const table = new Exports(owner('1'), new ObjectIds())
  const object = {}
  const id = table.register(object)
  table.pin(id, delivery('1')); table.pin(id, execution('1'))
  table.closeRecipientEpoch('owner', '1'); table.closeRecipientEpoch('owner', '1')
  assert.equal(table.pins(id), 1)
  assert.throws(() => table.pin(id, delivery('2')), (e: any) => e.code === 'StaleObject')
  table.release(delivery('1')); assert.equal(table.pins(id), 1)
  table.release(execution('1')); await table.join()
  const next: PinKey = { type: 'delivery', recipient: { ...owner('9'), epoch: '2' }, id: '1' }
  table.pin(id, next); table.release(next); await table.join()
})
test('unpublished exclusive object is cleaned once on owner rollback', async () => {
  const table = new Exports(owner('1'), new ObjectIds())
  const object = { unoffered: true }
  let calls = 0
  const id = table.registerExclusive(object, actual => { assert.equal(actual, object); calls++ })
  assert.equal(table.pins(id), 0)
  table.close(); table.close(); await table.join()
  assert.equal(calls, 1)
})

test('native child creator is immutable across reexports and old objects stay closed', async () => {
  const root = new Context()
  let parent!: Context
  let child!: Context
  let disposeChild!: () => Promise<void>
  const managed = new ManagedActivation(root, { async apply(ctx) {
    parent = ctx
    const fiber = ctx.plugin({ apply(ctx) { child = ctx } })
    disposeChild = () => fiber.dispose()
    await fiber
  } }, {})
  await managed.ready()
  assert.notEqual(child, parent)
  const table = Exports.managed(parent, owner('1'), new ObjectIds(), () => managed.isOpen)
  const callback = { async call(context: CallContext, text: string) { assert.equal(context.native(), child); assert.equal(text, 'child'); return null } }
  const output = withNative(child, exportBorrowCallback0(callback))
  assert.equal(output.kind, 'own'); if (output.kind !== 'own') return
  const native = output.value.register(table)
  const other = withNative(parent, exportBorrowCallback0(callback))
  assert.equal(other.kind, 'own'); if (other.kind !== 'own') return
  assert.deepEqual(other.value.register(table).identity, native.identity)
  const outside = withNative(root, exportBorrowCallback0(callback))
  assert.equal(outside.kind, 'own'); if (outside.kind !== 'own') return
  assert.throws(() => outside.value.register(table), (error: any) => error.code === 'CapabilityDenied')
  const key = execution('1'); table.pin(native.identity, key)
  const creator = table.executionContext(key)!
  assert.equal(creator.context, child)
  const caller: Caller = { async call() { throw new Error('unused') } }
  const context = new CallContext(caller, creator.context, () => creator.isOpen())
  assert.deepEqual(await native.dispatcher.dispatch(key, context, 'call', 'child'), { kind: 'value', value: null })
  await context.finish()
  const stopped = disposeChild()
  assert.equal(managed.isOpen, true)
  assert.throws(() => table.pin(native.identity, delivery('2')), (error: any) => error.code === 'ScopeClosed')
  assert.throws(() => table.executionContext(key), (error: any) => error.code === 'ScopeClosed')
  assert.throws(() => context.outbound({ kind: 'value', value: null }), (error: any) => error.code === 'ScopeClosed')
  assert.throws(() => other.value.register(table), (error: any) => error.code === 'ScopeClosed')
  assert.equal(table.executionObject(key), callback, 'existing execution retains the actual object through native close')
  table.release(key); await stopped; await table.join()
  await managed.stop(); await root.fiber.dispose()
})
