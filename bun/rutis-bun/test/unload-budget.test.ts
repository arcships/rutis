// PoC tests for the opt-in per-row unload cleanup budget in Bun
// (docs/design-row-unload-budget-2026-11-20.md, #1323).
import { expect, test } from 'bun:test'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { rowUnloadDeadline, rowUnloadTimeoutText, Runtime } from '../src/runner.ts'

function project(files: Record<string, string>) {
  const dir = mkdtempSync(join(tmpdir(), 'rutis-bun-budget-'))
  writeFileSync(join(dir, 'package.json'), '{}')
  for (const [name, text] of Object.entries(files)) writeFileSync(join(dir, name), text)
  return dir
}

async function captureStderr(body: () => Promise<void>): Promise<string> {
  const written: string[] = []
  const write = process.stderr.write.bind(process.stderr)
  ;(process.stderr as any).write = (chunk: any) => { written.push(String(chunk)); return true }
  try {
    await body()
  } finally {
    ;(process.stderr as any).write = write
  }
  return written.join('')
}

async function withEnv(value: string | undefined, body: () => Promise<void>) {
  const saved = process.env.RUTIS_ROW_UNLOAD_TIMEOUT
  if (value === undefined) delete process.env.RUTIS_ROW_UNLOAD_TIMEOUT
  else process.env.RUTIS_ROW_UNLOAD_TIMEOUT = value
  try {
    await body()
  } finally {
    if (saved === undefined) delete process.env.RUTIS_ROW_UNLOAD_TIMEOUT
    else process.env.RUTIS_ROW_UNLOAD_TIMEOUT = saved
  }
}

test('RUTIS_ROW_UNLOAD_TIMEOUT parses positive finite seconds and defaults to disabled', () => {
  expect(rowUnloadDeadline({})).toBeNull()
  expect(rowUnloadDeadline({ RUTIS_ROW_UNLOAD_TIMEOUT: '' })).toBeNull()
  expect(rowUnloadDeadline({ RUTIS_ROW_UNLOAD_TIMEOUT: '5' })).toBe(5_000)
  expect(rowUnloadDeadline({ RUTIS_ROW_UNLOAD_TIMEOUT: '0.25' })).toBe(250)
  for (const invalid of ['0', '-1', 'NaN', 'Infinity', 'abc']) {
    expect(rowUnloadDeadline({ RUTIS_ROW_UNLOAD_TIMEOUT: invalid })).toBeNull()
  }
})

test('default (unset) unload waits for delayed cleanups and emits no diagnostic', async () => {
  const dir = project({
    'slow.ts': `
export function apply(ctx) {
  ctx.effect(() => { globalThis.__bunBudgetLog.push('earlier') })
  ctx.provide('forecast', { today: () => 'sunny' })
  ctx.effect(async () => {
    await new Promise(resolve => setTimeout(resolve, 40))
    globalThis.__bunBudgetLog.push('delayed')
  })
}
`,
  })
  ;(globalThis as any).__bunBudgetLog = []
  await withEnv(undefined, async () => {
    const runtime = new Runtime(dir)
    runtime.dispatch('', 'mount', { announce: true })
    try {
      await runtime.load('w', './slow.ts', {}, null, null, [], 'weather')
      const stderr = await captureStderr(() => runtime.unload('w').then(() => {}))
      expect((globalThis as any).__bunBudgetLog).toEqual(['delayed', 'earlier'])
      expect(stderr).toBe('')
    } finally {
      await runtime.dispose()
      rmSync(dir, { recursive: true, force: true })
    }
  })
  delete (globalThis as any).__bunBudgetLog
})

test('timeout withdraws services, keeps healthy sibling unaffected, and isolates late actions', async () => {
  const dir = project({
    'hang.ts': `
export function apply(ctx, config) {
  ctx.effect(() => { globalThis.__bunHangLog.push('earlier') })
  const withdraw = ctx.provide('forecast', { today: () => config.label })
  if (config.hang) {
    ctx.effect(() => new Promise((_resolve, reject) => {
      globalThis.__bunRelease = () => {
        // Old withdraw handle must not touch another row's service.
        withdraw()
        try { ctx.provide('forecast', { today: () => 'stale' }) } catch (e) { globalThis.__bunLate.push('provide:' + e.message) }
        try { ctx.emit('weather/late') } catch (e) { globalThis.__bunLate.push('emit:' + e.name) }
        reject(new Error('late cleanup failure'))
      }
    }))
  }
}
`,
    'healthy.ts': `
export function apply(ctx) {
  ctx.provide('forecast', { today: () => 'healthy-sibling' })
  ctx.effect(() => { globalThis.__bunHealthyLog.push('metrics-cleaned') })
}
`,
  })
  ;(globalThis as any).__bunHangLog = []
  ;(globalThis as any).__bunHealthyLog = []
  ;(globalThis as any).__bunLate = []
  const runtime = new Runtime(dir, { rowUnloadTimeout: 50 })
  runtime.dispatch('', 'mount', { announce: true })
  try {
    await runtime.load('w1', './hang.ts', { label: 'v1', hang: true }, null, null, [], 'weather')
    expect((runtime.lookup('forecast') as any).today()).toBe('v1')
    const stderr = await captureStderr(async () => {
      const started = performance.now()
      await runtime.unload('w1')
      expect(performance.now() - started).toBeLessThan(1_000)
    })
    const expectedLine = `${rowUnloadTimeoutText('bun', 'weather', 50)}\n`
    expect(expectedLine).toBe(
      'runtime bun: cleanup of row weather did not finish within 0.05 s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it, or isolate this row in its own runtime)\n',
    )
    expect(stderr).toBe(expectedLine)
    expect((globalThis as any).__bunHangLog).toEqual([])
    expect(() => runtime.lookup('forecast')).toThrow('not available')

    // The row loads again at once (plugins are trusted with their own
    // remnant), and an unrelated healthy row takes over the withdrawn service.
    await runtime.load('w2', './hang.ts', { label: 'v2', hang: false }, [['forecast', 'v2']], null, [], 'weather')

    // Unrelated healthy row continues unaffected and takes over the withdrawn service.
    await runtime.load('m1', './healthy.ts', {}, null, null, [], 'metrics')
    expect((runtime.lookup('forecast') as any).today()).toBe('healthy-sibling')

    // Lingering old cleanup firing late actions/rejection never corrupts metrics.
    ;(globalThis as any).__bunRelease?.()
    await new Promise(resolve => setImmediate(resolve))
    expect((globalThis as any).__bunLate).toEqual(['provide:row w1 is not active', 'emit:BindingError'])
    expect((runtime.lookup('forecast') as any).today()).toBe('healthy-sibling')
    await runtime.unload('m1')
    expect((globalThis as any).__bunHealthyLog).toEqual(['metrics-cleaned'])
    await runtime.unload('w2')
    expect((globalThis as any).__bunHangLog).toEqual(['earlier'])
  } finally {
    await runtime.dispose()
    delete (globalThis as any).__bunHangLog
    delete (globalThis as any).__bunHealthyLog
    delete (globalThis as any).__bunLate
    delete (globalThis as any).__bunRelease
    rmSync(dir, { recursive: true, force: true })
  }
})

test('RUTIS_ROW_UNLOAD_TIMEOUT env var enables budget and preserves pre-timeout cleanup errors', async () => {
  const dir = project({
    'throw-then-hang.ts': `
export function apply(ctx) {
  ctx.effect(() => { globalThis.__bunErrLog.push('earliest') })
  ctx.provide('forecast', { today: () => 'sunny' })
  ctx.effect(() => new Promise((_, reject) => {
    setTimeout(() => reject(new Error('late rejection')), 120).unref?.()
  }))
  ctx.effect(() => { throw new Error('flush failed') })
}
`,
  })
  ;(globalThis as any).__bunErrLog = []
  await withEnv('0.05', async () => {
    const runtime = new Runtime(dir)
    runtime.dispatch('', 'mount', { announce: true })
    try {
      await runtime.load('w', './throw-then-hang.ts', {}, null, null, [], 'weather')
      let caught: unknown
      const stderr = await captureStderr(async () => {
        try {
          await runtime.unload('w')
        } catch (error) {
          caught = error
        }
      })
      expect((caught as Error)?.message).toBe('flush failed')
      expect(stderr).toBe(
        'runtime bun: cleanup of row weather did not finish within 0.05 s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it, or isolate this row in its own runtime)\n',
      )
      expect((globalThis as any).__bunErrLog).toEqual([])
      expect(() => runtime.lookup('forecast')).toThrow('not available')
    } finally {
      await runtime.dispose()
      rmSync(dir, { recursive: true, force: true })
    }
  })
  delete (globalThis as any).__bunErrLog
})

test('a timeout in one instances scope does not affect a same-id row in another scope', async () => {
  const dir = project({
    'instanced.ts': `
export function apply(ctx, config) {
  ctx.provide('forecast', { today: () => config.label })
  if (config.hang) {
    ctx.effect(() => new Promise(resolve => { globalThis.__bunScopedRelease = resolve }))
  } else {
    ctx.effect(() => { globalThis.__bunScopedLog.push(config.label) })
  }
}
`,
  })
  ;(globalThis as any).__bunScopedLog = []
  const scopeA = [{ id: 'tenant-a', group: 'tenants' }]
  const scopeB = [{ id: 'tenant-b', group: 'tenants' }]
  const runtime = new Runtime(dir, { rowUnloadTimeout: 50 })
  runtime.dispatch('', 'mount', { announce: true })
  try {
    await runtime.load('a1', './instanced.ts', { label: 'a-v1', hang: true }, [['forecast', 'tenant-a']], null, scopeA, 'weather')
    await runtime.load('b1', './instanced.ts', { label: 'b-v1', hang: false }, [['forecast', 'tenant-b']], null, scopeB, 'weather')

    const stderr = await captureStderr(() => runtime.unload('a1').then(() => {}))
    expect(stderr).toBe(`${rowUnloadTimeoutText('bun', 'weather', 50)}\n`)

    // Instance A loads again at once in its own scope.
    await runtime.load('a2', './instanced.ts', { label: 'a-v2', hang: false }, [['forecast', 'tenant-a']], null, scopeA, 'weather')
    await runtime.unload('a2')

    // Instance B has the same id ('weather') in scopeB: update and unload/reload succeed cleanly.
    await runtime.update('b1', { label: 'b-v2', hang: false })
    expect((globalThis as any).__bunScopedLog).toEqual(['a-v2', 'b-v1'])
    expect((runtime.lookup('forecast\0tenant-b') as any).today()).toBe('b-v2')
    await runtime.unload('b1')
    await runtime.load('b2', './instanced.ts', { label: 'b-v3', hang: false }, [['forecast', 'tenant-b']], null, scopeB, 'weather')
    expect((runtime.lookup('forecast\0tenant-b') as any).today()).toBe('b-v3')
    await runtime.unload('b2')
    expect((globalThis as any).__bunScopedLog).toEqual(['a-v2', 'b-v1', 'b-v2', 'b-v3'])

    // A rows.update whose cleanup times out reloads all the same.
    const scopeC = [{ id: 'tenant-c', group: 'tenants' }]
    await runtime.load('c1', './instanced.ts', { label: 'c-v1', hang: true }, [['forecast', 'tenant-c']], null, scopeC, 'weather')
    const updateStderr = await captureStderr(async () => {
      await runtime.update('c1', { label: 'c-v2', hang: false })
    })
    expect(updateStderr).toBe(`${rowUnloadTimeoutText('bun', 'weather', 50)}\n`)
    // scopeB still loads cleanly after scopeC timed out too.
    await runtime.load('b3', './instanced.ts', { label: 'b-v4', hang: false }, [['forecast', 'tenant-b']], null, scopeB, 'weather')
    await runtime.unload('b3')
    expect((globalThis as any).__bunScopedLog).toEqual(['a-v2', 'b-v1', 'b-v2', 'b-v3', 'b-v4'])
  } finally {
    ;(globalThis as any).__bunScopedRelease?.()
    delete (globalThis as any).__bunScopedLog
    delete (globalThis as any).__bunScopedRelease
    await runtime.dispose()
    rmSync(dir, { recursive: true, force: true })
  }
})
