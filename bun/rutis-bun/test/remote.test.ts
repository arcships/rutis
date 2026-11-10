// How long a child runtime still unloading is waited for before it is
// killed (#615): its cleanups have RUTIS_SHUTDOWN_TIMEOUT, then a margin,
// as rutis-host gives a runtime at a restart. A child the test drives: no
// process is started.
import { expect, test } from 'bun:test'
import type { ChildProcess } from 'node:child_process'
import { EventEmitter } from 'node:events'
import { ended, shutdownDeadline } from '../src/remote.ts'
import { within } from './support.ts'

type FakeChild = ChildProcess & { signals: string[] }

// A child process the test drives: when it ends, and how it was killed.
function fakeChild(): FakeChild {
  const child = new EventEmitter() as unknown as FakeChild
  child.exitCode = null
  child.signalCode = null
  child.signals = []
  child.kill = (signal?: NodeJS.Signals | number) => {
    const name = String(signal)
    child.signals.push(name)
    child.signalCode = name as NodeJS.Signals
    child.emit('exit', null, name)
    return true
  }
  return child
}

// Run `body` with RUTIS_SHUTDOWN_TIMEOUT set to `value`, restored after.
async function withShutdownTimeout(value: string | undefined, body: () => unknown): Promise<void> {
  const saved = process.env.RUTIS_SHUTDOWN_TIMEOUT
  if (value === undefined) delete process.env.RUTIS_SHUTDOWN_TIMEOUT
  else process.env.RUTIS_SHUTDOWN_TIMEOUT = value
  try {
    await body()
  } finally {
    if (saved === undefined) delete process.env.RUTIS_SHUTDOWN_TIMEOUT
    else process.env.RUTIS_SHUTDOWN_TIMEOUT = saved
  }
}

test('the shutdown deadline follows RUTIS_SHUTDOWN_TIMEOUT, 10 s otherwise', async () => {
  await withShutdownTimeout('1', () => expect(shutdownDeadline()).toBe(1_000))
  await withShutdownTimeout('0.25', () => expect(shutdownDeadline()).toBe(250))
  // Not a positive number: the default, as before (#615).
  for (const invalid of ['', 'soon', '0', '-1']) {
    await withShutdownTimeout(invalid, () => expect(shutdownDeadline(), `RUTIS_SHUTDOWN_TIMEOUT=${JSON.stringify(invalid)}`).toBe(10_000))
  }
  await withShutdownTimeout(undefined, () => expect(shutdownDeadline()).toBe(10_000))
})

test('a child still unloading is killed 2 s past its deadline, not at 5 s (#615)', async () => {
  await withShutdownTimeout('1', async () => {
    const child = fakeChild()
    const started = Date.now()
    await within('the child to be killed', ended(child))
    const took = Date.now() - started
    expect(child.signals).toEqual(['SIGKILL'])
    // Its cleanups have their deadline (1 s), then the margin (2 s).
    expect(took, `killed after ${took} ms, before the deadline and its margin`).toBeGreaterThanOrEqual(2_900)
    expect(took, `killed after ${took} ms: 5 s is the fixed kill time this replaces`).toBeLessThan(4_500)
  })
})

test('a child that ends within its deadline is not killed, and not waited for', async () => {
  await withShutdownTimeout('1', async () => {
    const child = fakeChild()
    setTimeout(() => { child.exitCode = 0; child.emit('exit', 0, null) }, 20)
    const started = Date.now()
    await within('the child to end', ended(child))
    const took = Date.now() - started
    expect(child.signals).toEqual([])
    expect(took, `waited ${took} ms for a child that ended at once`).toBeLessThan(1_000)
  })
})
