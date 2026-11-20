// PoC tests for opt-in per-row unload cleanup budget in Node (leaf plugins and
// native Cordis fibers; docs/design-row-unload-budget-2026-11-20.md, #1323).
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
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
    if (frame.op === 'throw') throw new Error(`call failed: ${JSON.stringify(frame.error)}`)
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
    load: (key, file, id = key, config = {}) =>
      ask('rows.load', data([key, file, config, [], [], {}, { scope: [], id }])),
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

test('standalone leaf timeout skips later cleanups, withdraws services, and prints verbatim line', async () => {
  const root = new Context()
  const log = []
  let leakedCtx
  const spec = definePlugin({
    name: 'weather',
    apply(ctx) {
      leakedCtx = ctx
      ctx.effect(() => { log.push('earlier-should-be-skipped') })
      ctx.provide('forecast', { today: () => 'sunny' })
      ctx.effect(() => new Promise(() => {})) // hangs forever
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
    'runtime node: cleanup of plugin weather did not finish within 0.05 s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it or restart the runtime)\n',
  )
  assert.equal(stderr, expected)
  assert.deepEqual(log, [], 'user cleanups after the timed-out cleanup must not run')
  assert.throws(() => leakedCtx.use('forecast'), { code: 'not-declared' }, 'provided service was withdrawn')
  // Another plugin can immediately provide the same service name without clash.
  const replacement = definePlugin({
    name: 'weather-v2',
    apply(ctx) { ctx.provide('forecast', { today: () => 'rainy' }) },
  })
  const second = root.plugin(toCordis(supported(replacement, 'weather-v2')))
  await second.await()
  assert.equal(root.get('forecast').today(), 'rainy')
  await second.dispose()
})

test('runner leaf row timeout via RUTIS_ROW_UNLOAD_TIMEOUT withdraws service and allows reload', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'rutis-node-budget-leaf-'))
  const file = name => join(dir, name)
  writeFileSync(file('anchor.mjs'), 'export function apply() {}\n')
  writeFileSync(file('hang-leaf.mjs'), `
import { definePlugin } from '${SDK_URL}'
export default definePlugin({
  name: 'weather',
  apply(ctx, config) {
    ctx.effect(() => { globalThis.__leafBudgetLog.push('earlier') })
    ctx.provide('forecast', { today: () => config.label })
    if (config.hang) {
      ctx.effect(() => new Promise(() => {}))
    }
  },
})
`)
  globalThis.__leafBudgetLog = []
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
      'runtime node: cleanup of row weather did not finish within 0.05 s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it or restart the runtime)\n',
    )
    assert.deepEqual(globalThis.__leafBudgetLog, [], 'remaining user cleanups were skipped')
    const state = await rt.inspect()
    assert.deepEqual(state.rows, [])
    assert.deepEqual(state.slots, [])
    assert.equal(state.services, 0, 'provided service withdrawn from Cordis store')
    // Reloading the same row & service name succeeds cleanly.
    await rt.load('r1', file('hang-leaf.mjs'), 'weather', { label: 'v2', hang: false })
    await rt.unload('r1')
    assert.deepEqual(globalThis.__leafBudgetLog, ['earlier'])
    assert.equal(await rt.stop(), 0)
  } finally {
    delete globalThis.__leafBudgetLog
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
      'runtime node: cleanup of row weather did not finish within 0.05 s; stopped waiting and finished unloading the row (the stuck cleanup may still be running; fix it or restart the runtime)\n',
    )
    const state = await rt.inspect()
    assert.deepEqual(state.rows, [])
    assert.deepEqual(state.slots, [])
    assert.equal(state.fibers, 0, 'Cordis removed fiber from registry')
    assert.equal(state.services, 0, 'Cordis withdrew provided service from store')
    // Reloading the same row & providing the same service succeeds without clash.
    await rt.load('c1', file('hang-cordis.mjs'), 'weather', { label: 'v2', hang: false })
    await rt.unload('c1')
    assert.equal(await rt.stop(), 0)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})
