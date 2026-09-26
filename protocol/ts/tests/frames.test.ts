import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { Duplex } from 'node:stream'
import { test } from 'node:test'
import { encodeFrame, FrameDecoder, Peer } from '../src/frame.ts'
import { ProtocolError } from '../src/error.ts'

test('shared frame corpus with every byte arriving separately', () => {
  const cases = JSON.parse(readFileSync(new URL('../../fixtures/frames.json', import.meta.url), 'utf8'))
  for (const item of cases) {
    const decoder = new FrameDecoder(); const values: unknown[] = []
    const parse = () => { for (const byte of Buffer.from(item.hex, 'hex')) decoder.push(Buffer.from([byte]), value => values.push(value)); decoder.end() }
    if (item.valid) { parse(); assert.equal(values.length, 1, item.name) }
    else assert.throws(parse, (error: any) => error.code === 'InvalidParams', item.name)
  }
})
test('outbound JSON cannot change meaning through stringify', () => {
  for (const invalid of [undefined, NaN, Infinity, { value: undefined }, { value() {} }, new Date(), new Array(2), Object.assign([1], { hidden: 2 }), Object.create({ inherited: true })]) {
    assert.throws(() => encodeFrame(invalid), (error: any) => error.code === 'InvalidParams')
  }
  const decoder = new FrameDecoder(); const values: unknown[] = []
  decoder.push(Buffer.concat([encodeFrame({ text: '雪' }), encodeFrame(null), encodeFrame([false, 0])]), value => values.push(value)); decoder.end()
  assert.deepEqual(JSON.parse(JSON.stringify(values)), [{ text: '雪' }, null, [false, 0]])
})
class Wire extends Duplex {
  other!: Wire
  _read(): void {}
  _write(bytes: Buffer, _: string, done: (error?: Error | null) => void): void { this.other.push(Buffer.from(bytes)); done() }
  _destroy(error: Error | null, done: (error?: Error | null) => void): void { this.other.push(null); done(error) }
}
function pair(): [Wire, Wire] { const a = new Wire(); const b = new Wire(); a.other = b; b.other = a; return [a, b] }
const closed = () => new ProtocolError('Unavailable', 'test', 'closed')
test('independent receive pump supports callback reentry and concurrent calls', async () => {
  const [a, b] = pair(); const first = new Peer(a, async (_, value) => value)
  const second = new Peer(b, async (_, value) => second.request('callback', value))
  try { const result = await Promise.all(Array.from({ length: 64 }, (_, n) => first.request('nested', n))); assert.deepEqual(result, Array.from({ length: 64 }, (_, n) => n)) }
  finally { first.close(closed()); second.close(closed()) }
})
test('duplicate request ids close without a second execution', async () => {
  const [a, b] = pair(); let executions = 0
  const peer = new Peer(a, async () => { executions++; return null })
  b.write(encodeFrame({ type: 'request', id: '1', method: 'once', params: null }))
  await new Promise(resolve => setImmediate(resolve)); assert.equal(executions, 1)
  b.write(encodeFrame({ type: 'request', id: '1', method: 'once', params: null }))
  await new Promise(resolve => setImmediate(resolve)); assert.equal(executions, 1); assert.equal(peer.isClosed, true); b.destroy()
})
test('disconnect rejects pending waiters with unknown execution', async () => {
  const [a, b] = pair(); const peer = new Peer(a, async () => null)
  const pending = peer.request('never-completed', null); peer.close(closed())
  await assert.rejects(pending, (error: any) => error.code === 'Unavailable' && error.execution === 'unknown'); b.destroy()
})
