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

test('a frame the session refuses ends the channel, saying why', async () => {
  const { createServer } = await import('node:net')
  const { mkdtempSync } = await import('node:fs')
  const { tmpdir } = await import('node:os')
  const { join } = await import('node:path')
  const path = join(mkdtempSync(join(tmpdir(), 'rutis-bun-')), 'far.sock')
  let farEnded!: () => void
  const ended = new Promise<void>(resolve => { farEnded = resolve })
  const server = createServer(socket => {
    // The host's side: greet, then send one call twice (a repeated id).
    const call = JSON.stringify({ op: 'invoke', id: 'rust:1', path: [], target: 't', method: 'm', args: { type: 'list', value: [] } })
    socket.write(JSON.stringify({ op: 'hello', version: 2 }) + '\n')
    setTimeout(() => socket.write(call + '\n' + call + '\n'), 20)
    socket.on('data', () => {})
    socket.on('close', farEnded)
  })
  await new Promise<void>(resolve => server.listen(path, resolve))
  const client = await Client.connect(path, { dispatch: () => 'ok' })
  await client.closed()
  await ended
  server.close()
  expect(client.reason).toContain('session fault')
  expect(client.reason).toContain('repeated invocation identity')
})
