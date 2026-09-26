import type { Duplex } from 'node:stream'
import { ProtocolError } from './error.ts'
import { decodeJson, MAX_JSON_BYTES } from './json.ts'
import { keys, validateJson } from './contract.ts'
import { sequence } from './imports.ts'

function failure(code: 'InvalidParams' | 'Unavailable' | 'Business', message: string): ProtocolError { return new ProtocolError(code, 'frame', message) }
export function encodeFrame(value: unknown): Buffer {
  validateJson({}, value) // reject undefined, NaN, classes and functions before stringify can coerce them
  const bytes = Buffer.from(JSON.stringify(value), 'utf8')
  decodeJson(bytes)
  const header = Buffer.allocUnsafe(4); header.writeUInt32BE(bytes.length)
  return Buffer.concat([header, bytes])
}
/** Incremental parser retains at most one bounded body plus a partial header;
 * it never concatenates an unbounded input stream or allocates before length checks. */
export class FrameDecoder {
  private header = Buffer.alloc(4)
  private headerBytes = 0
  private body?: Buffer
  private bodyBytes = 0
  push(input: Buffer, receive: (value: unknown) => void): void {
    let at = 0
    while (at < input.length) {
      if (!this.body) {
        const count = Math.min(4 - this.headerBytes, input.length - at)
        input.copy(this.header, this.headerBytes, at, at + count); at += count; this.headerBytes += count
        if (this.headerBytes < 4) continue
        const length = this.header.readUInt32BE()
        if (!length || length > MAX_JSON_BYTES) throw failure('InvalidParams', 'invalid frame length')
        this.body = Buffer.allocUnsafe(length); this.bodyBytes = 0
      }
      const count = Math.min(this.body.length - this.bodyBytes, input.length - at)
      input.copy(this.body, this.bodyBytes, at, at + count); at += count; this.bodyBytes += count
      if (this.bodyBytes === this.body.length) {
        const value = decodeJson(this.body)
        this.body = undefined; this.bodyBytes = 0; this.headerBytes = 0
        receive(value)
      }
    }
  }
  end(): void { if (this.headerBytes || this.body) throw failure('InvalidParams', 'truncated frame') }
}
type Handler = (method: string, params: unknown) => Promise<unknown>
interface Pending { resolve(value: unknown): void; reject(error: unknown): void }
export class Peer {
  private decoder = new FrameDecoder()
  private pending = new Map<string, Pending>()
  private next = 0n
  private latestRequest = 0n
  private failed?: ProtocolError
  constructor(private stream: Duplex, private handler: Handler) {
    stream.on('data', chunk => {
      try { this.decoder.push(chunk as Buffer, value => this.receive(value)) }
      catch (error) { this.close(error instanceof ProtocolError ? error : failure('InvalidParams', String(error))) }
    })
    stream.on('end', () => {
      try { this.decoder.end(); this.close(failure('Unavailable', 'private stream disconnected')) }
      catch (error) { this.close(error as ProtocolError) }
    })
    stream.on('error', error => this.close(failure('Unavailable', error.message)))
    stream.on('close', () => this.close(failure('Unavailable', 'private stream closed')))
  }
  get isClosed(): boolean { return this.failed !== undefined }
  close(error: ProtocolError): void {
    if (this.failed) return
    this.failed = error
    for (const pending of this.pending.values()) pending.reject(new ProtocolError(error.code, error.stage, error.message, 'unknown'))
    this.pending.clear(); this.stream.destroy()
  }
  private send(value: unknown): void {
    if (this.failed) throw this.failed
    const bytes = encodeFrame(value)
    // Node queues this entire buffer even if the awaiting request is abandoned.
    this.stream.write(bytes, error => { if (error) this.close(failure('Unavailable', error.message)) })
  }
  request(method: string, params: unknown): Promise<unknown> {
    if (this.failed) return Promise.reject(this.failed)
    const id = (++this.next).toString(); sequence(id)
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject })
      try { this.send({ type: 'request', id, method, params }) }
      catch (error) { this.pending.delete(id); reject(error) }
    })
  }
  private receive(value: unknown): void {
    keys(value, ['type', 'id'], ['method', 'params', 'result', 'error']); sequence(value.id)
    if (value.type === 'request') {
      keys(value, ['type', 'id', 'method', 'params'])
      if (typeof value.method !== 'string') throw failure('InvalidParams', 'method must be a string')
      if (BigInt(value.id) <= this.latestRequest) throw failure('InvalidParams', 'request id cannot be reused or reordered')
      this.latestRequest = BigInt(value.id)
      // A handler may await a nested request; reception continues independently.
      void Promise.resolve().then(() => this.handler(value.method, value.params)).then(result => {
        this.send({ type: 'response', id: value.id, result })
      }, error => {
        const protocol = error instanceof ProtocolError ? error : new ProtocolError('Business', 'handler', String(error), 'unknown')
        this.send({ type: 'error', id: value.id, error: { code: protocol.code, stage: protocol.stage, message: protocol.message, execution: protocol.execution } })
      }).catch(error => this.close(error instanceof ProtocolError ? error : failure('Unavailable', String(error))))
      return
    }
    const pending = this.pending.get(value.id)
    if (!pending) throw failure('InvalidParams', 'response does not identify a pending request')
    if (value.type === 'response') { keys(value, ['type', 'id', 'result']); this.pending.delete(value.id); pending.resolve(value.result); return }
    if (value.type === 'error') {
      keys(value, ['type', 'id', 'error']); keys(value.error, ['code', 'stage', 'message', 'execution'])
      if (!['InvalidParams', 'InterfaceMismatch', 'UnsupportedCapability', 'CapabilityDenied', 'StaleObject', 'ScopeClosed', 'Cancelled', 'DeadlineExceeded', 'Unavailable', 'Business'].includes(value.error.code) || !['not_started', 'unknown'].includes(value.error.execution) || typeof value.error.stage !== 'string' || typeof value.error.message !== 'string') throw failure('InvalidParams', 'invalid protocol error')
      this.pending.delete(value.id); pending.reject(new ProtocolError(value.error.code, value.error.stage, value.error.message, value.error.execution)); return
    }
    throw failure('InvalidParams', 'unknown frame message')
  }
}
