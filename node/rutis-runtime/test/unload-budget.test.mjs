// PoC tests for the opt-in per-row unload cleanup budget in Node (leaf plugins
// and native Cordis fibers; docs/design-row-unload-budget-2026-11-20.md, #1323).
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
// The budget timers are unref'd (they must not pin a real runtime's exit), so
// between a test's own awaits the loop may hold no handle at all: Node 22's
// runner then ends the file before the next test starts ("Promise resolution
// is still pending but the event loop has already resolved"). One ref'd
// interval keeps the loop alive for the file; each test still waits for what
// it waits for.
const keepalive = setInterval(() => {}, 1_000)
test.after(() => clearInterval(keepalive))
import { Context } from '@deepseek-ai/cordis'
import { definePlugin } from '../../rutis/src/index.mjs'
import { rowUnloadDeadline, rowUnloadTimeoutText, supported, toCordis } from '../src/leaf.mjs'
import { run } from '../src/runner.mjs'
import { ENDPOINT_PROTOCOL, Session } from '../src/session.mjs'
import { within } from './support.mjs'

const SDK_URL = new URL('../../rutis/src/index.mjs', import.meta.url).href
const greeting = { op: 'hello', version: ENDPOINT_PROTOCOL, endpoint: 'rust', capabilities: ['objects'] }
const data = value => ({ type: 'data', value })
const invoke = (id, target, method, args) => ({ op: 'invoke', id, path: [], target, method, args })

// Capture stderr writes synchronously while `body` runs.
async function captureStderr(body) {
  const chunks = []
  const original = process.stderr.write
  process.stderr.write = chunk => { chunks.push(String(chunk)); return true }
  try {
    await body()
  } finally {
    process.stderr.write = original
  }
  return chunks.join('')
}

// Run one Node runtime in-process via the `run()` factory (as runner-factory.test.mjs).
function inProcess(anchor, { env = {}, rowUnloadTimeout } = {}) {
  const sent = [], waiting = []
  let session, ready, finish, failure, status
  let calls = 0
  const allocated = () => `rust:${++calls}`
  const opened = new Promise(resolve => { ready = resolve })
  const closed = new Promise(resolve => { finish = resolve })
  const deliver = frame => session.receive(frame)
  const send = line => {
    const frame = JSON.parse(line)
    sent.push(frame)
    if (frame.op === 'invoke' && String(frame.id).startsWith('node:')) {
      setImmediate(() => deliver({ op: 'return', id: frame.id, value: data(null) }))
    }
    for (const check of waiting) check()
  }
  const connect = async (_channel, dispatch, _settled, endpoint) => {
    session = new Session({
      endpoint,
      dispatch: (target, method, args) => dispatch(target, method, args),
      send,
      pump: () => { throw new Error('unexpected sync pump') },
      abort: error => { failure = error },
    })
    session.receive(greeting)
    ready()
    return {
      call: (target, method, args, described) => session.invoke(target, method, args, described),
      callAsync: (target, method, args, described) => session.invokeAsync(target, method, args, described),
      callAnswered: (target, method, args, answered, described) => session.invokeAnswered(target, method, args, answered, described),
      callFunctionAsync: (fn, args) => Promise.resolve().then(() => fn(...args)),
      callFunctionSync: (fn, args) => fn(...args),
      withinSync: fn => session.withinSync(fn),
      closed: () => closed,
      end: () => finish(),
      drain: () => session.drain(),
      fault: undefined,
    }
  }

  const running = run({
    argv: ['budget-test', '--id', 'node', '--peer', 'rust', anchor],
    env,
    connect,
    exit: code => { status = code },
    handlers: false,
    watchdog: false,
    rowUnloadTimeout,
  })

  const next = (predicate, what) => within(`runtime node: ${what}`, new Promise(resolve => {
    const look = () => {
      const index = sent.findIndex(predicate)
      return index < 0 ? undefined : sent.splice(index, 1)[0]
    }
    const found = look()
    if (found) return resolve(found)
    waiting.push(() => {
      const frame = look()
      if (frame) resolve(frame)
    })
  }))

  const settle = async frame => {
    while (frame.op === 'return' && frame.value?.type === 'reference' && frame.value.value?.kind === 'future') {
      const reference = frame.value.value
      const id = allocated()
      const reply = next(candidate => candidate.id === id, 'the awaited result')
      deliver({ op: 'await', id, path: [...(reference.origin ?? [])], reference: reference.id })
      frame = await reply
    }
    if (frame.op === 'throw') {
      const err = new Error(frame.error?.message ?? JSON.stringify(frame.error))
      err.remote = frame.error
      throw err
    }
    return frame
  }

  const ask = async (method, args) => {
    const id = allocated()
    const reply = next(frame => frame.id === id, `reply to ${method}`)
    deliver(invoke(id, '', method, args))
    return settle(await reply)
  }

  return {
    started: async () => { await opened; await new Promise(resolve => setImmediate(resolve)) },
    mount: () => ask('mount', data({ plugins: [], announce: true, scope: [] })),
    load: (key, file, id = key, config = {}, scope = [], isolate = []) =>
      ask('rows.load', data([key, file, config, isolate, [], {}, { scope, id }])),
    unload: key => ask('rows.unload', data([key])),
    inspect: async () => (await ask('inspect', data(null))).value.value,
    stop: async () => {
      finish()
      await within('runtime node to end', running)
      assert.equal(failure, undefined)
      return status
    },
  }
}

test('RUTIS_ROW_UNLOAD_TIMEOUT parses positive finite seconds and defaults to disabled', () => {
  assert.equal(rowUnloadDeadline({}), null)
  assert.equal(rowUnloadDeadline({ RUTIS_ROW_UNLOAD_TIMEOUT: '' }), null)
  assert.equal(rowUnloadDeadline({ RUTIS_ROW_UNLOAD_TIMEOUT: '5' }), 5_000)
  assert.equal(rowUnloadDeadline({ RUTIS_ROW_UNLOAD_TIMEOUT: '0.25' }), 250)
  for (const invalid of ['0', '-1', 'NaN', 'Infinity', 'abc']) {
    assert.equal(rowUnloadDeadline({ RUTIS_ROW_UNLOAD_TIMEOUT: invalid }), null, invalid)
  }
})

test('default (unset) leaf unload waits for delayed cleanups and emits no diagnostic', async () => {
  const root = new Context()
  const log = []
  const spec = definePlugin({
    name: 'slow-ok',
    apply(ctx) {
      ctx.effect(() => { log.push('earlier') })
      ctx.provide('tally', { count: () => 7 })
      ctx.effect(async () => {
        await new Promise(resolve => setTimeout(resolve, 40))
        log.push('delayed')
      })
    },
  })
  const stderr = await captureStderr(async () => {
    const plugin = root.plugin(toCordis(supported(spec, 'slow-ok'), [], undefined, { rowUnloadTimeout: null }))
    await plugin.await()
    await plugin.dispose()
  })
  assert.deepEqual(log, ['delayed', 'earlier'])
  assert.equal(stderr, '')
})

test('standalone leaf timeout skips later cleanups, withdraws services, isolates late provide, and prints verbatim line', async () => {
  const root = new Context()
  const log = []
  let leakedCtx
  let releaseStuck
  const spec = definePlugin({
    name: 'weather',
    apply(ctx) {
      leakedCtx = ctx
      ctx.effect(() => { log.push('earlier-should-be-skipped') })
      ctx.provide('forecast', { today: () => 'sunny' })
      ctx.effect(() => new Promise(resolve => { releaseStuck = resolve }))
    },
  })
  const stderr = await captureStderr(async () => {
    const plugin = root.plugin(toCordis(supported(spec, 'weather'), [], undefined, { rowUnloadTimeout: 50 }))
    await plugin.await()
    const started = performance.now()
    await plugin.dispose()
    assert.ok(performance.now() - started < 1_000, 'dispose finished promptly after budget')
  })
  const expected = `${rowUnloadTimeoutText({ runtime: 'node', target: 'plugin weather', budgetMs: 50, native: false })}\n`
  assert.equal(
    expected,
    'runtime node: cleanup of plugin weather did not finish within 0.05 s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it, or isolate this row in its own runtime)\n',
  )
  assert.equal(stderr, expected)
  assert.deepEqual(log, [], 'user cleanups after the timed-out cleanup must not run')
  assert.throws(() => leakedCtx.use('forecast'), { code: 'not-declared' }, 'provided service was withdrawn')
  // Late provide from the lingering context is refused cleanly.
  assert.throws(() => leakedCtx.provide('forecast', { today: () => 'stale' }), /not active/)
  // Another plugin can immediately provide the same service name without clash.
  const replacement = definePlugin({
    name: 'metrics',
    apply(ctx) { ctx.provide('forecast', { today: () => 'rainy' }) },
  })
  const second = root.plugin(toCordis(supported(replacement, 'metrics')))
  await second.await()
  assert.equal(root.get('forecast').today(), 'rainy')
  releaseStuck?.()
  await second.dispose()
})

test('runner leaf row timeout withdraws services, keeps healthy rows unaffected, isolates late actions, and lets the row reload', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'rutis-node-budget-leaf-'))
  const file = name => join(dir, name)
  writeFileSync(file('anchor.mjs'), 'export function apply() {}\n')
  writeFileSync(file('hang-leaf.mjs'), `
import { definePlugin } from '${SDK_URL}'
export default definePlugin({
  name: 'weather',
  apply(ctx, config) {
    globalThis.__leafBudgetCtx = ctx
    ctx.effect(() => { globalThis.__leafBudgetLog.push('earlier') })
    ctx.provide('forecast', { today: () => config.label })
    if (config.hang) {
      ctx.effect(() => new Promise((resolve, reject) => {
        globalThis.__leafRelease = () => {
          try { ctx.provide('forecast', { today: () => 'stale' }) } catch (e) { globalThis.__leafLateErrors.push('provide:' + e.message) }
          try { ctx.emit('weather/late') } catch (e) { globalThis.__leafLateErrors.push('emit:' + e.name) }
          reject(new Error('late cleanup failure'))
        }
      }))
    }
  },
})
`)
  writeFileSync(file('healthy-leaf.mjs'), `
import { definePlugin } from '${SDK_URL}'
export default definePlugin({
  name: 'metrics',
  apply(ctx) {
    ctx.provide('forecast', { today: () => 'healthy-sibling' })
    ctx.effect(() => { globalThis.__leafHealthyLog.push('metrics-cleaned') })
  },
})
`)
  globalThis.__leafBudgetLog = []
  globalThis.__leafHealthyLog = []
  globalThis.__leafLateErrors = []
  const rt = inProcess(file('anchor.mjs'), {
    env: { RUTIS_ROW_UNLOAD_TIMEOUT: '0.05', RUTIS_TEST_INSPECT: '1' },
  })
  try {
    await rt.started()
    await rt.mount()
    await rt.load('r1', file('hang-leaf.mjs'), 'weather', { label: 'v1', hang: true })
    const stderr = await captureStderr(async () => {
      const started = performance.now()
      await rt.unload('r1')
      assert.ok(performance.now() - started < 1_000, 'rows.unload returned promptly')
    })
    assert.equal(
      stderr,
      'runtime node: cleanup of row weather did not finish within 0.05 s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it, or isolate this row in its own runtime)\n',
    )
    assert.deepEqual(globalThis.__leafBudgetLog, [], 'remaining user cleanups were skipped')
    const state = await rt.inspect()
    assert.deepEqual(state.rows, [])
    assert.deepEqual(state.slots, [])
    assert.equal(state.services, 0, 'provided service withdrawn from Cordis store')

    // The row loads again at once: plugins are trusted with their own remnant
    // (they are not sandboxed), and the runtime only says what happened.
    await rt.load('r2', file('hang-leaf.mjs'), 'weather', { label: 'v2', hang: false }, [], [['forecast', 'v2']])

    // Unrelated healthy row continues unaffected and can take over the withdrawn service.
    await rt.load('m1', file('healthy-leaf.mjs'), 'metrics', {})
    // Trigger late actions + late rejection from the lingering cleanup: isolated, never corrupts metrics.
    globalThis.__leafRelease?.()
    await new Promise(resolve => setImmediate(resolve))
    assert.equal(globalThis.__leafLateErrors.length, 2)
    assert.match(globalThis.__leafLateErrors[0], /^provide:.*not active/)
    assert.equal(globalThis.__leafLateErrors[1], 'emit:BindingError')
    await rt.unload('m1')
    assert.deepEqual(globalThis.__leafHealthyLog, ['metrics-cleaned'])
    await rt.unload('r2')
    assert.deepEqual(globalThis.__leafBudgetLog, ['earlier'], 'the reloaded generation cleaned up')
    assert.equal(await rt.stop(), 0)
  } finally {
    delete globalThis.__leafBudgetLog
    delete globalThis.__leafHealthyLog
    delete globalThis.__leafLateErrors
    delete globalThis.__leafBudgetCtx
    delete globalThis.__leafRelease
    rmSync(dir, { recursive: true, force: true })
  }
})

test('runner native Cordis fiber timeout finishes teardown, withdraws service, and prints verbatim line', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'rutis-node-budget-cordis-'))
  const file = name => join(dir, name)
  writeFileSync(file('anchor.mjs'), 'export function apply() {}\n')
  writeFileSync(file('hang-cordis.mjs'), `
export const name = 'weather'
export function apply(ctx, config) {
  ctx.provide('forecast', { today: () => config.label })
  if (config.hang) {
    ctx.effect(() => () => new Promise(() => {}))
  }
}
`)
  writeFileSync(file('healthy-cordis.mjs'), `
export const name = 'metrics'
export function apply(ctx) {
  ctx.provide('forecast', { today: () => 'healthy-cordis' })
}
`)
  const rt = inProcess(file('anchor.mjs'), {
    env: { RUTIS_TEST_INSPECT: '1' },
    rowUnloadTimeout: 50,
  })
  try {
    await rt.started()
    await rt.mount()
    await rt.load('c1', file('hang-cordis.mjs'), 'weather', { label: 'v1', hang: true })
    const stderr = await captureStderr(async () => {
      const started = performance.now()
      await rt.unload('c1')
      assert.ok(performance.now() - started < 1_000, 'native Cordis rows.unload returned promptly')
    })
    assert.equal(
      stderr,
      'runtime node: cleanup of row weather did not finish within 0.05 s; stopped waiting and finished unloading the row (the stuck cleanup may still be running; fix it, or isolate this row in its own runtime)\n',
    )
    const state = await rt.inspect()
    assert.deepEqual(state.rows, [])
    assert.deepEqual(state.slots, [])
    assert.equal(state.fibers, 0, 'Cordis removed fiber from registry')
    assert.equal(state.services, 0, 'Cordis withdrew provided service from store')

    // The row loads again at once (plugins are trusted with their own
    // remnant), and an unrelated healthy row takes over the withdrawn service.
    await rt.load('c2', file('hang-cordis.mjs'), 'weather', { label: 'v2', hang: false }, [], [['forecast', 'v2']])
    // Unrelated healthy row can still load and take over the withdrawn service.
    await rt.load('m1', file('healthy-cordis.mjs'), 'metrics', {})
    await rt.unload('m1')
    await rt.unload('c2')
    assert.equal(await rt.stop(), 0)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('a timeout in one instances scope does not affect a same-id row in another scope', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'rutis-node-budget-scoped-'))
  const file = name => join(dir, name)
  writeFileSync(file('anchor.mjs'), 'export function apply() {}\n')
  writeFileSync(file('instanced-weather.mjs'), `
import { definePlugin } from '${SDK_URL}'
export default definePlugin({
  name: 'weather',
  apply(ctx, config) {
    ctx.provide('forecast', { today: () => config.label })
    if (config.hang) {
      ctx.effect(() => new Promise(resolve => { globalThis.__nodeScopedRelease = resolve }))
    } else {
      ctx.effect(() => { globalThis.__nodeScopedLog.push(config.label) })
    }
  },
})
`)
  globalThis.__nodeScopedLog = []
  const scopeA = [{ id: 'tenant-a', group: 'tenants' }]
  const scopeB = [{ id: 'tenant-b', group: 'tenants' }]
  const rt = inProcess(file('anchor.mjs'), {
    env: { RUTIS_ROW_UNLOAD_TIMEOUT: '0.05', RUTIS_TEST_INSPECT: '1' },
  })
  try {
    await rt.started()
    await rt.mount()
    await rt.load('a1', file('instanced-weather.mjs'), 'weather', { label: 'a-v1', hang: true }, scopeA, [['forecast', 'tenant-a']])
    await rt.load('b1', file('instanced-weather.mjs'), 'weather', { label: 'b-v1', hang: false }, scopeB, [['forecast', 'tenant-b']])

    const stderr = await captureStderr(async () => {
      await rt.unload('a1')
    })
    assert.equal(
      stderr,
      'runtime node: cleanup of row weather did not finish within 0.05 s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it, or isolate this row in its own runtime)\n',
    )

    // Instance A loads again at once in its own scope.
    await rt.load('a2', file('instanced-weather.mjs'), 'weather', { label: 'a-v2', hang: false }, scopeA, [['forecast', 'tenant-a']])

    // Instance B has the same id ('weather') in scopeB: unloading and reloading succeeds cleanly.
    await rt.unload('b1')
    assert.deepEqual(globalThis.__nodeScopedLog, ['b-v1'])
    await rt.load('b1', file('instanced-weather.mjs'), 'weather', { label: 'b-v2', hang: false }, scopeB, [['forecast', 'tenant-b']])
    await rt.unload('b1')
    await rt.load('b2', file('instanced-weather.mjs'), 'weather', { label: 'b-v3', hang: false }, scopeB, [['forecast', 'tenant-b']])
    await rt.unload('b2')
    assert.deepEqual(globalThis.__nodeScopedLog, ['b-v1', 'b-v2', 'b-v3'])

    // Top-level 'weather' (empty scope []) is also distinct from scopeA and loads cleanly.
    await rt.load('root1', file('instanced-weather.mjs'), 'weather', { label: 'root-v1', hang: false }, [], [['forecast', 'root']])
    await rt.unload('root1')
    assert.deepEqual(globalThis.__nodeScopedLog, ['b-v1', 'b-v2', 'b-v3', 'root-v1'])

    globalThis.__nodeScopedRelease?.()
    assert.equal(await rt.stop(), 0)
  } finally {
    delete globalThis.__nodeScopedLog
    delete globalThis.__nodeScopedRelease
    rmSync(dir, { recursive: true, force: true })
  }
})
