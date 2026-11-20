// Managed timers (`ctx.timeout`, `ctx.interval`) in the Bun runtime (#1325,
// docs/design-managed-timers-2026-11-20.en.md).
import { expect, test } from 'bun:test'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { Runtime } from '../src/runner.ts'

const sleep = (ms: number) => new Promise(resolve => setTimeout(resolve, ms))

function project(files: Record<string, string>) {
  const dir = mkdtempSync(join(tmpdir(), 'rutis-bun-timers-'))
  writeFileSync(join(dir, 'package.json'), '{}')
  for (const [name, text] of Object.entries(files)) writeFileSync(join(dir, name), text)
  return dir
}

async function withRuntime(files: Record<string, string>, run: (runtime: Runtime) => Promise<void>) {
  const dir = project(files)
  const runtime = new Runtime(dir)
  runtime.dispatch('', 'mount', { announce: true })
  try {
    await run(runtime)
  } finally {
    await runtime.dispose()
    rmSync(dir, { recursive: true, force: true })
  }
}

async function captureStderr(run: () => Promise<void>): Promise<string> {
  const written: string[] = []
  const write = process.stderr.write.bind(process.stderr)
  ;(process.stderr as any).write = (chunk: any) => { written.push(String(chunk)); return true }
  try {
    await run()
  } finally {
    ;(process.stderr as any).write = write
  }
  return written.join('')
}

test('timeout fires once normally and does not fire if cancelled early or unloaded before delay', () => withRuntime({
  'ok.ts': `
export function apply(ctx) {
  ctx.timeout(15, () => { globalThis.bunTimerHits.fired++ })
  const cancel = ctx.timeout(15, () => { globalThis.bunTimerHits.cancelled++ })
  cancel()
  cancel()
}
`,
  'unloaded.ts': `
export function apply(ctx) {
  ctx.timeout(40, () => { globalThis.bunTimerHits.unloaded++ })
}
`,
}, async runtime => {
  ;(globalThis as any).bunTimerHits = { fired: 0, cancelled: 0, unloaded: 0 }
  await runtime.load('ok', './ok.ts', {}, null, null)
  await sleep(50)
  expect((globalThis as any).bunTimerHits.fired).toBe(1)
  expect((globalThis as any).bunTimerHits.cancelled).toBe(0)
  await runtime.unload('ok')

  await runtime.load('u', './unloaded.ts', {}, null, null)
  await runtime.unload('u')
  await sleep(70)
  expect((globalThis as any).bunTimerHits.unloaded).toBe(0)
}))

test('interval fires multiple times, stops on unload, does not overlap slow async callbacks, and supports self-cancel', () => withRuntime({
  'loop.ts': `
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms))
export function apply(ctx) {
  ctx.interval(10, async () => {
    const s = globalThis.bunIntervalState
    s.active++
    if (s.active > s.maxActive) s.maxActive = s.active
    await sleep(30)
    s.active--
    s.ticks++
  })
}
`,
  'self.ts': `
export function apply(ctx) {
  const stop = ctx.interval(10, () => {
    globalThis.bunSelfTicks++
    if (globalThis.bunSelfTicks === 2) stop()
  })
}
`,
}, async runtime => {
  const state = (globalThis as any).bunIntervalState = { active: 0, maxActive: 0, ticks: 0 }
  await runtime.load('hb', './loop.ts', {}, null, null)
  const deadline = Date.now() + 2000
  while (state.ticks < 3 && Date.now() < deadline) await sleep(10)
  await runtime.unload('hb')

  const atUnload = state.ticks
  expect(atUnload).toBeGreaterThanOrEqual(3)
  expect(state.maxActive).toBe(1)
  await sleep(60)
  expect(state.ticks).toBe(atUnload)

  ;(globalThis as any).bunSelfTicks = 0
  await runtime.load('self', './self.ts', {}, null, null)
  await sleep(80)
  expect((globalThis as any).bunSelfTicks).toBe(2)
  await runtime.unload('self')
}))

test('registering timeout or interval during unload and after dispose is refused verbatim and starts nothing', () => withRuntime({
  'late.ts': `
export function apply(ctx) {
  globalThis.bunLateCtx = ctx
  ctx.effect(() => {
    const c1 = ctx.timeout(1, () => { globalThis.bunLateRan = true })
    const c2 = ctx.interval(1, () => { globalThis.bunLateRan = true })
    c1()
    c2()
  })
}
`,
}, async runtime => {
  ;(globalThis as any).bunLateRan = false
  const stderr = await captureStderr(async () => {
    await runtime.load('heartbeat', './late.ts', {}, null, null, [], 'heartbeat')
    await runtime.unload('heartbeat')
    ;(globalThis as any).bunLateCtx.timeout(1, () => { (globalThis as any).bunLateRan = true })
    await sleep(30)
  })
  expect((globalThis as any).bunLateRan).toBe(false)
  expect(stderr).toBe('rutis: row heartbeat is not active: timer is not registered\n'.repeat(3))
}))

test('invalid delay, period, or callback throws exact RangeError / TypeError', () => withRuntime({
  'bad.ts': `
export function apply(ctx) {
  const out = globalThis.bunTimerValidation = {}
  const check = (key, fn) => {
    try { fn() } catch (error) { out[key] = [error.name, error.message] }
  }
  check('negTimeout', () => ctx.timeout(-5, () => {}))
  check('nanTimeout', () => ctx.timeout(Number.NaN, () => {}))
  check('overflowTimeout', () => ctx.timeout(3_000_000_000, () => {}))
  check('zeroInterval', () => ctx.interval(0, () => {}))
  check('negInterval', () => ctx.interval(-10, () => {}))
  check('badTimeoutCb', () => ctx.timeout(10, null))
  check('badIntervalCb', () => ctx.interval(10, 'nope'))
}
`,
}, async runtime => {
  await runtime.load('bad', './bad.ts', {}, null, null)
  await runtime.unload('bad')
  const out = (globalThis as any).bunTimerValidation
  expect(out.negTimeout).toEqual(['RangeError', 'timeout delay must be a non-negative finite number of milliseconds, got -5'])
  expect(out.nanTimeout).toEqual(['RangeError', 'timeout delay must be a non-negative finite number of milliseconds, got NaN'])
  expect(out.overflowTimeout).toEqual(['RangeError', 'timeout delay must be at most 2147483647 ms, got 3000000000'])
  expect(out.zeroInterval).toEqual(['RangeError', 'interval period must be a positive finite number of milliseconds, got 0'])
  expect(out.negInterval).toEqual(['RangeError', 'interval period must be a positive finite number of milliseconds, got -10'])
  expect(out.badTimeoutCb).toEqual(['TypeError', 'timeout needs a callback function'])
  expect(out.badIntervalCb).toEqual(['TypeError', 'interval needs a callback function'])
}))

test('timer callback errors are contained, reported verbatim, and interval keeps running', () => withRuntime({
  'faulty.ts': `
export function apply(ctx) {
  ctx.timeout(5, () => { throw new Error('sync boom') })
  ctx.timeout(5, async () => { throw new Error('connection refused') })
  ctx.interval(10, async () => {
    globalThis.bunFaultyTicks++
    if (globalThis.bunFaultyTicks === 1) throw new Error('tick one failed')
  })
}
`,
}, async runtime => {
  ;(globalThis as any).bunFaultyTicks = 0
  const stderr = await captureStderr(async () => {
    await runtime.load('heartbeat', './faulty.ts', {}, null, null, [], 'heartbeat')
    const deadline = Date.now() + 2000
    while ((globalThis as any).bunFaultyTicks < 3 && Date.now() < deadline) await sleep(10)
    await runtime.unload('heartbeat')
  })
  expect((globalThis as any).bunFaultyTicks).toBeGreaterThanOrEqual(3)
  expect(stderr).toContain('rutis: timer of row heartbeat failed: Error: sync boom\n')
  expect(stderr).toContain('rutis: timer of row heartbeat failed: Error: connection refused\n')
  expect(stderr).toContain('rutis: timer of row heartbeat failed: Error: tick one failed\n')
}))
