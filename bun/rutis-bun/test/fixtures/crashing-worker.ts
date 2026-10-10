// An I/O worker that greets, then crashes without saying so.
import { workerData } from 'node:worker_threads'

const { port, signal } = workerData
const send = (message: unknown) => {
  port.postMessage(message)
  Atomics.add(signal, 0, 1)
  Atomics.notify(signal, 0)
}
send({ ready: true })
send({ op: 'hello', version: 2 })
setTimeout(() => { throw new Error('the worker broke') }, 20)
