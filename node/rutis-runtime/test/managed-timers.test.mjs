// Managed timers (`ctx.timeout`, `ctx.interval`) on Node leaf plugins (#1325,
// docs/design-managed-timers-2026-11-20.en.md).
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { Context } from '@deepseek-ai/cordis'
import { definePlugin } from '../../rutis/src/index.mjs'
import { supported, toCordis } from '../src/leaf.mjs'

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms))

async function captureStderr(run) {
  const written = []
  const original = process.stderr.write
  process.stderr.write = chunk => { written.push(String(chunk)); return true }
  try {
    await run()
  } finally {
    process.stderr.write = original
  }
  return written.join('')
}

test('timeout fires once normally and does not fire if cancelled early or unloaded before delay', async () => {
  const root = new Context()
  let fired = 0
  let cancelledFired = 0
  let unloadedFired = 0
  const spec = definePlugin({
    name: 'timers',
    apply(ctx) {
      ctx.timeout(15, () => { fired++ })
      const cancel = ctx.timeout(15, () => { cancelledFired++ })
      cancel()
      cancel() // idempotent
    },
  })
  const plugin = root.plugin(toCordis(supported(spec, 'timers')))
  await plugin.await()
  await sleep(50)
  assert.equal(fired, 1)
  assert.equal(cancelledFired, 0)
  await plugin.dispose()

  const specUnload = definePlugin({
    name: 'unloaded',
    apply(ctx) {
      ctx.timeout(40, () => { unloadedFired++ })
    },
  })
  const pluginUnload = root.plugin(toCordis(supported(specUnload, 'unloaded')))
  await pluginUnload.await()
  await pluginUnload.dispose()
  await sleep(70)
  assert.equal(unloadedFired, 0)
})

test('interval fires multiple times, stops on unload, does not overlap slow async callbacks, and allows self-cancel', async () => {
  const root = new Context()
  let ticks = 0
  let active = 0
  let maxActive = 0
  const spec = definePlugin({
    name: 'heartbeat',
    apply(ctx) {
      ctx.interval(10, async () => {
        active++
        if (active > maxActive) maxActive = active
        await sleep(30)
        active--
        ticks++
      })
    },
  })
  const plugin = root.plugin(toCordis(supported(spec, 'heartbeat')))
  await plugin.await()
  const deadline = Date.now() + 2000
  while (ticks < 3 && Date.now() < deadline) await sleep(10)
  await plugin.dispose()

  const atUnload = ticks
  assert.ok(atUnload >= 3, `expected at least 3 ticks, got ${atUnload}`)
  assert.equal(maxActive, 1, 'slow async interval callbacks must not overlap')
  await sleep(60)
  assert.equal(ticks, atUnload, 'zero fires after unload')

  // Self-cancel inside callback stops after the current tick.
  let selfTicks = 0
  const selfStop = definePlugin({
    name: 'self-stop',
    apply(ctx) {
      const stop = ctx.interval(10, () => {
        selfTicks++
        if (selfTicks === 2) stop()
      })
    },
  })
  const p2 = root.plugin(toCordis(supported(selfStop, 'self-stop')))
  await p2.await()
  await sleep(80)
  assert.equal(selfTicks, 2)
  await p2.dispose()
})

test('registering timeout or interval during unload and after dispose is refused verbatim and starts nothing', async () => {
  const root = new Context()
  let ran = false
  let kept
  const stderr = await captureStderr(async () => {
    const spec = definePlugin({
      name: 'heartbeat',
      apply(ctx) {
        kept = ctx
        ctx.effect(() => {
          const c1 = ctx.timeout(1, () => { ran = true })
          const c2 = ctx.interval(1, () => { ran = true })
          c1()
          c2()
        })
      },
    })
    const plugin = root.plugin(toCordis(supported(spec, 'heartbeat')))
    await plugin.await()
    await plugin.dispose()
    // Also refused after dispose.
    kept.timeout(1, () => { ran = true })
    await sleep(30)
  })
  assert.equal(ran, false)
  assert.equal(
    stderr,
    'rutis: plugin heartbeat is not active: timer is not registered\n'.repeat(3),
  )
})

test('invalid delay, period, or callback throws exact RangeError / TypeError', async () => {
  const root = new Context()
  const errors = {}
  const spec = definePlugin({
    name: 'validate',
    apply(ctx) {
      const check = (key, fn) => {
        try { fn() } catch (error) { errors[key] = [error.name, error.message] }
      }
      check('negTimeout', () => ctx.timeout(-5, () => {}))
      check('nanTimeout', () => ctx.timeout(Number.NaN, () => {}))
      check('overflowTimeout', () => ctx.timeout(3_000_000_000, () => {}))
      check('zeroInterval', () => ctx.interval(0, () => {}))
      check('negInterval', () => ctx.interval(-10, () => {}))
      check('badTimeoutCb', () => ctx.timeout(10, null))
      check('badIntervalCb', () => ctx.interval(10, 'nope'))
    },
  })
  const plugin = root.plugin(toCordis(supported(spec, 'validate')))
  await plugin.await()
  await plugin.dispose()

  assert.deepEqual(errors.negTimeout, ['RangeError', 'timeout delay must be a non-negative finite number of milliseconds, got -5'])
  assert.deepEqual(errors.nanTimeout, ['RangeError', 'timeout delay must be a non-negative finite number of milliseconds, got NaN'])
  assert.deepEqual(errors.overflowTimeout, ['RangeError', 'timeout delay must be at most 2147483647 ms, got 3000000000'])
  assert.deepEqual(errors.zeroInterval, ['RangeError', 'interval period must be a positive finite number of milliseconds, got 0'])
  assert.deepEqual(errors.negInterval, ['RangeError', 'interval period must be a positive finite number of milliseconds, got -10'])
  assert.deepEqual(errors.badTimeoutCb, ['TypeError', 'timeout needs a callback function'])
  assert.deepEqual(errors.badIntervalCb, ['TypeError', 'interval needs a callback function'])
})

test('timer callback sync throw and async rejection are contained, reported verbatim, and interval keeps running', async () => {
  const root = new Context()
  let unhandled = 0
  const onUnhandled = () => { unhandled++ }
  process.on('unhandledRejection', onUnhandled)
  let intervalTicks = 0
  try {
    const stderr = await captureStderr(async () => {
      const spec = definePlugin({
        name: 'heartbeat',
        apply(ctx) {
          ctx.timeout(5, () => { throw new Error('sync boom') })
          ctx.timeout(5, async () => { throw new Error('connection refused') })
          ctx.interval(10, async () => {
            intervalTicks++
            if (intervalTicks === 1) throw new Error('tick one failed')
          })
        },
      })
      const plugin = root.plugin(toCordis(supported(spec, 'heartbeat')))
      await plugin.await()
      const deadline = Date.now() + 2000
      while (intervalTicks < 3 && Date.now() < deadline) await sleep(10)
      await plugin.dispose()
    })
    assert.equal(unhandled, 0, 'no unhandledRejection should escape')
    assert.ok(intervalTicks >= 3, 'interval must continue after a failed tick')
    assert.ok(stderr.includes('rutis: timer of plugin heartbeat failed: Error: sync boom\n'), stderr)
    assert.ok(stderr.includes('rutis: timer of plugin heartbeat failed: Error: connection refused\n'), stderr)
    assert.ok(stderr.includes('rutis: timer of plugin heartbeat failed: Error: tick one failed\n'), stderr)
  } finally {
    process.off('unhandledRejection', onUnhandled)
  }
})

test('native Cordis plugins do not get leaf ctx.timeout or ctx.interval (Cordis compatibility)', async () => {
  const root = new Context()
  let hasTimeout = true
  let hasInterval = true
  const plugin = root.plugin(function nativeCordisPlugin(ctx) {
    hasTimeout = 'timeout' in ctx
    hasInterval = 'interval' in ctx
  })
  await plugin.await()
  await plugin.dispose()
  assert.equal(hasTimeout, false)
  assert.equal(hasInterval, false)
})

test('async callback that synchronously self-cancels before await is still awaited before earlier LIFO cleanups', async () => {
  for (const mode of ['timeout', 'interval']) {
    const root = new Context()
    const events = []
    let poolOpen = true
    let startedResolve
    const started = new Promise(resolve => { startedResolve = resolve })
    let releaseResolve
    const release = new Promise(resolve => { releaseResolve = resolve })

    const spec = definePlugin({
      name: `self-cancel-${mode}`,
      apply(ctx) {
        ctx.effect(() => {
          poolOpen = false
          events.push('pool.close')
        })
        const cancel = ctx[mode](5, async () => {
          // Synchronously self-cancel BEFORE the first await!
          cancel()
          events.push('callback.started')
          startedResolve()
          await release
          events.push(poolOpen ? 'callback.used-open-pool' : 'callback.USED-CLOSED-POOL')
        })
      },
    })
    const plugin = root.plugin(toCordis(supported(spec, `self-cancel-${mode}`)))
    await plugin.await()
    await started

    // Unload while the self-cancelled async callback is still in flight.
    const disposing = plugin.dispose()
    await sleep(20)
    assert.equal(poolOpen, true, `${mode}: pool must stay open while self-cancelled callback is still in flight`)

    releaseResolve()
    await disposing
    assert.deepEqual(events, ['callback.started', 'callback.used-open-pool', 'pool.close'])
  }
})
