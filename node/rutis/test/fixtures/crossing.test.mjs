// The test calls a function the plugin handed it while the plugin calls
// one of the test's services (#987): each side, waiting for the other,
// answers the other's call meanwhile. Run by ../testing-crossing.test.mjs,
// which stops it if it hangs.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { load } from '../../src/testing.mjs'
import crossing from './crossing.mjs'
import crossingLater from './crossing-later.mjs'

test('a callback of the plugin while the plugin calls a service of the test', async () => {
  const scope = { withLock: callback => callback() }
  const t = await load(crossing, { services: { files: { open: () => scope }, other: { ping: () => 'pong' } } })
  assert.deepEqual(await t.service('probe').go(), ['got', 'pong'])
  await t.unload()
})

test('a callback of the plugin, after an await, while the plugin calls a service of the test', async () => {
  const files = {
    async withLock(work) {
      await null
      return await work()
    },
    async other() { return 'other' },
  }
  const t = await load(crossingLater, { services: { files } })
  assert.deepEqual(await t.service('probe').go(), ['worked', 'other'])
  await t.unload()
})
