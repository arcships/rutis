import assert from 'node:assert/strict'
import { test } from 'node:test'
import { Context } from '@deepseek-ai/cordis'
import { ManagedActivation, NativeState } from '../src/managed.ts'

function deferred() {
  let resolve!: () => void
  const promise = new Promise<void>(r => { resolve = r })
  return { promise, resolve }
}

test('M0/T01/T02: two native Cordis plugins install isolated injections', async () => {
  const root = new Context()
  const seen: object[] = []
  const plugin = {
    inject: ['database'],
    apply(ctx: Context) { seen.push((ctx as any).database); ctx.provide('result', {}) },
  }
  const a = new ManagedActivation(root, plugin, {}, { database: { name: 'a' } }, undefined, ['result'])
  const b = new ManagedActivation(root, plugin, {}, { database: { name: 'b' } }, undefined, ['result'])
  await Promise.all([a.ready(), b.ready()])
  assert.equal(a.native.state, NativeState.ACTIVE)
  assert.equal(b.native.state, NativeState.ACTIVE)
  assert.notEqual(seen[0], seen[1])
  assert.equal((seen[0] as any).name, 'a')
  assert.equal((seen[1] as any).name, 'b')
  assert.notEqual(a.native.ctx, b.native.ctx)
  assert.equal(root.get('database'), undefined)
  await a.stop()
  assert.equal(b.native.state, NativeState.ACTIVE)
  await b.stop()
  await root.fiber.dispose()
})

test('M0/T14/T21: dependency loss revokes synchronously and never reapplies an old activation', async () => {
  const root = new Context()
  const remove = root.provide('backend', { version: 1 })
  let count = 0
  let old!: Context
  const reasons: string[] = []
  const activation = new ManagedActivation(root, {
    inject: ['backend'],
    apply(ctx) { old = ctx; count++ },
  }, {}, {}, reason => reasons.push(reason))
  await activation.ready()
  root.reflect.notify(['backend'])
  assert.equal(activation.isOpen, true, 'unchanged dependency refresh keeps the activation')
  const cleanup = remove()
  assert.equal(activation.isOpen, false)
  assert.equal(activation.native.uid, null)
  assert.throws(() => old.effect(() => () => {}), /inactive context/)
  assert.throws(() => old.provide('late', {}), /inactive context/)
  assert.throws(() => old.on('internal/status', () => {}), /inactive context/)
  assert.throws(() => old.plugin({ apply() {} }), /inactive context/)
  root.provide('backend', { version: 2 })
  await cleanup
  await activation.stop()
  assert.equal(count, 1)
  assert.deepEqual(reasons, ['native-invalidated'])
  const next = new ManagedActivation(root, { inject: ['backend'], apply() { count++ } }, {})
  await next.ready()
  assert.equal(count, 2)
  await next.stop()
  await root.fiber.dispose()
})

test('M0/T14: loss while Loading closes admission before user apply returns', async () => {
  const root = new Context()
  const remove = root.provide('backend', {})
  const entered = deferred()
  const finish = deferred()
  let count = 0
  let old!: Context
  const activation = new ManagedActivation(root, {
    inject: ['backend'],
    async apply(ctx) { old = ctx; count++; entered.resolve(); await finish.promise },
  }, {})
  await entered.promise
  const cleanup = remove()
  assert.equal(activation.isOpen, false)
  assert.throws(() => old.provide('late', {}), /inactive context/)
  root.provide('backend', {})
  finish.resolve()
  await cleanup
  await activation.stop()
  assert.equal(count, 1)
  await root.fiber.dispose()
})

test('M0/T15: internal children remain native; losing a necessary export revokes the root', async () => {
  const root = new Context()
  let child!: ReturnType<Context['plugin']>
  let disposed = 0
  const activation = new ManagedActivation(root, {
    apply(ctx) {
      child = ctx.plugin({ apply(sub) {
        sub.provide('session', {})
        sub.effect(() => () => { disposed++ })
      } })
    },
  }, {})
  activation.requireExport('session')
  await activation.ready()
  await child
  const cleanup = child.dispose()
  // Native provider teardown starts on a microtask; await native child cleanup
  // as the causal barrier instead of guessing with a timer.
  await cleanup
  assert.equal(activation.isOpen, false)
  await activation.stop()
  assert.equal(disposed, 1)
  assert.equal(activation.native.state, NativeState.DISPOSED)
  await root.fiber.dispose()
})

test('M0: stop joins slow cleanup and closes admission at the call site', async () => {
  const root = new Context()
  const entered = deferred()
  const finish = deferred()
  const activation = new ManagedActivation(root, {
    apply(ctx) { ctx.effect(() => async () => { entered.resolve(); await finish.promise }) },
  }, {})
  await activation.ready()
  let stopped = false
  const stop = activation.stop().then(() => { stopped = true })
  assert.equal(activation.isOpen, false)
  await entered.promise
  assert.equal(stopped, false)
  finish.resolve()
  await stop
  assert.equal(stopped, true)
  await root.fiber.dispose()
})

test('M0: parallel/serial native listener ownership and short-circuit values', async () => {
  const root = new Context()
  const order: string[] = []
  const activation = new ManagedActivation(root, {
    apply(ctx) {
      ;(ctx.on as any)('query', () => { order.push('continue'); return undefined })
      ;(ctx.on as any)('query', () => { order.push('zero'); return 0 })
      ;(ctx.on as any)('query', () => { order.push('last'); return 'last' })
    },
  }, {})
  await activation.ready()
  assert.equal(await (root.serial as any)('query'), 0)
  assert.deepEqual(order, ['continue', 'zero'])
  order.length = 0
  await (root.parallel as any)('query')
  assert.deepEqual(order, ['continue', 'zero', 'last'])
  await activation.stop()
  order.length = 0
  await (root.parallel as any)('query')
  assert.deepEqual(order, [])
  await root.fiber.dispose()
})

test('M0: generator effects remain native and an initial config failure cannot be retried locally', async () => {
  const root = new Context()
  let disposed = 0
  const activation = new ManagedActivation(root, {
    *apply() { yield () => { disposed++ } },
  }, {})
  await activation.ready()
  await activation.stop()
  assert.equal(disposed, 1)
  let called = false
  const invalid = new ManagedActivation(root, {
    Config: { '~standard': { version: 1, vendor: 'm0', validate() { return { issues: [{ message: 'bad config' }] } } } },
    apply() { called = true },
  }, {})
  await assert.rejects(invalid.ready(), /invalid config/)
  assert.equal(invalid.isOpen, false)
  await assert.rejects(invalid.native.restart(), /inactive context/)
  assert.equal(called, false)
  await invalid.stop()
  await root.fiber.dispose()
})

test('M0/T15: explicit disposal of a necessary service revokes its still-loaded managed root', async () => {
  const root = new Context()
  let remove!: () => void
  const activation = new ManagedActivation(root, {
    apply(ctx) { remove = ctx.provide('connection', {}) },
  }, {}, {}, undefined, ['connection'])
  activation.requireExport('connection')
  await activation.ready()
  assert.equal(activation.native.state, NativeState.ACTIVE)
  await remove()
  assert.equal(activation.isOpen, false)
  assert.equal(activation.native.uid, null)
  await activation.stop()
  await root.fiber.dispose()
})
