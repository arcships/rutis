import { expect, test } from 'bun:test'
import { Client } from '../src/client.ts'

test('a worker that crashes ends the session, saying so', async () => {
  const client = await Client.connect('unused', {
    dispatch: () => null,
    worker: new URL('./fixtures/crashing-worker.ts', import.meta.url),
  })
  await client.closed()
  expect(client.reason).toContain('communication worker exited')
  expect(() => client.call('t', 'm', [])).toThrow()
})
