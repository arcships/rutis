// guarantee: bridge/session-service
// `sessions` is the host's to provide: rutis's own session service is
// `rutis/sessions`, so a test can give a plugin a fake `sessions` (#986).
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { load } from '../src/testing.mjs'
import probe from './fixtures/sessions-probe.mjs'

for (const name of ['a test gives sessions', 'another test gives sessions again']) {
  test(name, async () => {
    const t = await load(probe, { services: { sessions: { x: () => 7 } } })
    assert.equal(await t.service('probe').go(), 7)
    await t.unload()
  })
}
