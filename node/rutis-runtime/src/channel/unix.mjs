import { createConnection } from 'node:net'
import { ConnectError } from './errors.mjs'
import { LIMITS } from './websocket.mjs'

// How long a channel that ended its side waits for the far end's end.
const END_GRACE = 1000
// The largest message either way, without its newline: the WebSocket
// binding's 16 MiB. Over it, the channel closes; a far end that never sends
// a newline costs at most this much memory.
export const MAX_MESSAGE = LIMITS.maxMessage
const NEWLINE = 0x0a

// A newline-framed channel on a connected stream: each message sent gets a
// trailing newline, each line received is one message. `closed(reason)`
// runs once, when the stream ends (reason undefined) or fails.
export function frame(stream, { message, closed }, { maxMessage = MAX_MESSAGE } = {}) {
  let failure
  const fail = reason => { failure ??= reason; stream.destroy() }
  stream.on('error', error => { failure ??= error.message })
  // The bytes of the line being received, kept until its newline arrives.
  let pending = [], size = 0
  stream.on('data', chunk => {
    let start = 0
    while (!stream.destroyed) {
      const end = chunk.indexOf(NEWLINE, start)
      const length = (end < 0 ? chunk.length : end) - start
      if (size + length > maxMessage) return fail(`received a message over the limit of ${maxMessage} bytes`)
      if (end < 0) {
        if (length) { pending.push(chunk.subarray(start)); size += length }
        return
      }
      const line = size ? Buffer.concat([...pending, chunk.subarray(start, end)]) : chunk.subarray(start, end)
      pending = []; size = 0; start = end + 1
      message(line.toString('utf8'))
    }
  })
  stream.once('end', () => { if (size) failure ??= 'stream ended inside a message' })
  stream.once('close', () => closed(failure))
  // A stream paused by whoever read before us (a loopback child's token)
  // stays paused when a 'data' listener is added: start it.
  stream.resume()
  return {
    send(text) {
      if (stream.destroyed) return
      const length = Buffer.byteLength(text)
      if (length > maxMessage) return fail(`message of ${length} bytes exceeds the limit of ${maxMessage}`)
      stream.write(text + '\n')
    },
    // Half-close, then wait for the far end's end, but not for ever: on
    // macOS a socket's end may not reach this side after it half-closed.
    end() { stream.end(); setTimeout(() => stream.destroy(), END_GRACE).unref() },
    close(reason) { failure ??= reason; stream.destroy() },
  }
}

// Dial a Unix socket: `unix:<path>` or a bare path.
export async function open(spec, handlers) {
  const path = spec.startsWith('unix:') ? spec.slice('unix:'.length) : spec
  const stream = createConnection(path)
  try {
    await new Promise((resolve, reject) => { stream.once('connect', resolve); stream.once('error', reject) })
  } catch (error) {
    stream.destroy()
    throw new ConnectError(error.code === 'EACCES' ? 'auth-rejected' : 'retryable', `${path}: ${error.message}`)
  }
  return frame(stream, handlers)
}
