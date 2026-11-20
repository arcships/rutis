// PoC tests for opt-in per-row unload cleanup budget in Bun
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

test('opt-in budget times out hanging cleanup, skips later cleanups, withdraws services, and prints verbatim line', async () => {
  const dir = project({
    'hang.ts': `
export function apply(ctx, config) {
  ctx.effect(() => { globalThis.__bunHangLog.push('earlier') })
  ctx.provide('forecast', { today: () => config.label })
  if (config.hang) {
    ctx.effect(() => new Promise(() => {}))
  }
}
`,
  })
  ;(globalThis as any).__bunHangLog = []
  const runtime = new Runtime(dir, { rowUnloadTimeout: 50 })
  runtime.dispatch('', 'mount', { announce: true })
  try {
    await runtime.load('w', './hang.ts', { label: 'v1', hang: true }, null, null, [], 'weather')
    expect((runtime.lookup('forecast') as any).today()).toBe('v1')
    const stderr = await captureStderr(async () => {
      const started = performance.now()
      await runtime.unload('w')
      expect(performance.now() - started).toBeLessThan(1_000)
    })
    const expected = `${rowUnloadTimeoutText('bun', 'weather', 50)}\n`
    expect(expected).toBe(
      'runtime bun: cleanup of row weather did not finish within 0.05 s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it or restart the runtime)\n',
    )
    expect(stderr).toBe(expected)
    expect((globalThis as any).__bunHangLog).toEqual([])
    expect(() => runtime.lookup('forecast')).toThrow('not available')
    // Reloading the same row & providing the same service succeeds cleanly.
    await runtime.load('w', './hang.ts', { label: 'v2', hang: false }, null, null, [], 'weather')
    expect((runtime.lookup('forecast') as any).today()).toBe('v2')
    await runtime.unload('w')
    expect((globalThis as any).__bunHangLog).toEqual(['earlier'])
  } finally {
    delete (globalThis as any).__bunHangLog
    await runtime.dispose()
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
    // Rejects well after the 50 ms budget to verify no unhandled rejection.
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
        'runtime bun: cleanup of row weather did not finish within 0.05 s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it or restart the runtime)\n',
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
