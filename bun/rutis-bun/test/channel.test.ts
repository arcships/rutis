import { expect, test } from 'bun:test'
import { createServer, type Socket } from 'node:net'
import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { frame, open, ConnectError, takeToken, TOKEN } from '../src/channel.ts'

// A connected pair of sockets over a Unix socket.
async function pair(): Promise<[Socket, Socket]> {
  const path = join(mkdtempSync(join(tmpdir(), 'rutis-bun-')), 'pair.sock')
  const server = createServer()
  const accepted = new Promise<Socket>(resolve => server.once('connection', resolve))
  await new Promise<void>(resolve => server.listen(path, resolve))
  const { createConnection } = await import('node:net')
  const client = createConnection(path)
  await new Promise(resolve => client.once('connect', resolve))
  const far = await accepted
  server.close()
  return [client, far]
}

function collect() {
  const messages: string[] = []
  let closed!: (reason?: string) => void
  const ended = new Promise<string | undefined>(resolve => { closed = resolve })
  return { messages, ended, handlers: { message: (text: string) => messages.push(text), closed } }
}

test('each line is one message, across reads and within one', async () => {
  const [a, b] = await pair()
  const got = collect()
  frame(b, got.handlers)
  a.write('one\ntw')
  await Bun.sleep(10)
  a.write('o\nthree\nfour\n')
  a.end()
  expect(await got.ended).toBeUndefined()
  expect(got.messages).toEqual(['one', 'two', 'three', 'four'])
})

test('a message longer than the limit ends the channel', async () => {
  const [a, b] = await pair()
  const got = collect()
  frame(b, got.handlers, 16)
  a.write('short\n' + 'x'.repeat(10))
  a.write('y'.repeat(10) + '\nnever\n')
  expect(await got.ended).toContain('longer than 16 bytes')
  expect(got.messages).toEqual(['short'])
  a.destroy()
})

test('a stream that ends inside a message fails', async () => {
  const [a, b] = await pair()
  const got = collect()
  frame(b, got.handlers)
  a.end('partial')
  expect(await got.ended).toBe('stream ended inside a message')
})

test('sending refuses a raw newline and an oversized message', async () => {
  const [a, b] = await pair()
  const channel = frame(a, collect().handlers, 8)
  expect(() => channel.send('a\nb')).toThrow('raw newline')
  expect(() => channel.send('123456789')).toThrow('longer than 8 bytes')
  a.destroy(); b.destroy()
})

test('a runtime does not dial WebSocket endpoints', async () => {
  const error = await open('ws://127.0.0.1:1/rutis', collect().handlers).catch(error => error)
  expect(error).toBeInstanceOf(ConnectError)
  expect(error.category).toBe('incompatible')
  expect(error.message).toContain('listens')
})

test('the loopback token is taken out of the environment', () => {
  process.env[TOKEN] = 'secret'
  expect(takeToken()).toBe('secret')
  expect(process.env[TOKEN]).toBeUndefined()
  expect(takeToken()).toBeUndefined()
})

test('a tcp channel without a token is refused before dialing', async () => {
  const error = await open('tcp:127.0.0.1:1', collect().handlers).catch(error => error)
  expect(error.category).toBe('auth-rejected')
})
