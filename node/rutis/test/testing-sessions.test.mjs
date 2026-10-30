// guarantee: bridge/session-service
// guarantee: loader/service
// `sessions` and `loader` are the host's to provide: rutis's own services
// are `rutis/sessions` and `rutis/loader`, so a test can give a plugin a
// fake `sessions` and `loader` (#986).
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { load } from '../src/testing.mjs'
import probe from './fixtures/sessions-probe.mjs'

for (const name of ['a test gives sessions and loader', 'another test gives them again']) {
  test(name, async () => {
    const t = await load(probe, { services: { sessions: { x: () => 7 }, loader: { y: () => 1 } } })
    assert.equal(await t.service('probe').go(), 8)
    await t.unload()
  })
}
