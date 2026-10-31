// The test calls a function the plugin handed it (a callback) while the
// plugin calls a service of the test (#987): the test process and the
// test tool's driver each wait for the other synchronously, and each
// answers the other's call meanwhile. They waited for ever: a synchronous
// wait leaves node:test no turn to time the test out, so the file runs in
// a process of its own, stopped if it hangs.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'

const FILE = fileURLToPath(new URL('./fixtures/crossing.test.mjs', import.meta.url))

test('a callback of the plugin and a service of the test, called at once, return', { timeout: 60_000 }, async () => {
  const env = { ...process.env }
  delete env.NODE_TEST_CONTEXT
  const child = spawn(process.execPath, ['--test', '--test-reporter=tap', FILE], { env, stdio: ['ignore', 'pipe', 'pipe'], detached: process.platform !== 'win32' })
  let output = ''
  for (const stream of [child.stdout, child.stderr]) stream.on('data', chunk => { output += chunk })
  // Stop it after 30 s, with the file's own test process (its process
  // group).
  const deadline = setTimeout(() => {
    try { process.kill(process.platform === 'win32' ? child.pid : -child.pid, 'SIGKILL') } catch {}
  }, 30_000)
  const [code, signal] = await new Promise(resolve => child.once('exit', (...end) => resolve(end)))
  clearTimeout(deadline)
  assert.deepEqual({ code, signal }, { code: 0, signal: null }, output)
  assert.match(output, /^ok 1 - a callback of the plugin while the plugin calls a service of the test$/m)
  assert.match(output, /^ok 2 - a callback of the plugin, after an await, while the plugin calls a service of the test$/m)
})
