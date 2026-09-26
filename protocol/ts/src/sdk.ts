import { ProtocolError } from './error.ts'
import { Service } from '@deepseek-ai/cordis'
import { ObjectProxy, type ObjectIdentity } from './imports.ts'
import { Exports, type PinKey } from './exports.ts'
import type { Context } from '@deepseek-ai/cordis'

export type Outbound =
  | { kind: 'value'; value: unknown }
  | { kind: 'own'; value: NativeExport }
  | { kind: 'foreign'; value: ObjectProxy }
  | { kind: 'record'; fields: Record<string, Outbound> }
  | { kind: 'list'; items: Outbound[] }
  | { kind: 'optional'; value: Outbound | null }
export interface Caller {
  call(target: ObjectProxy, method: string, params: Outbound): Promise<unknown>
  bindNative?(ctx: Context, value: Outbound): void
}
/** Associate generated callback/object values before the client encodes them.
 * Registration creates no grants or execution path around the broker. */
export function bindNative(caller: Caller, ctx: Context, value: Outbound): void {
  if (!caller.bindNative) throw new ProtocolError('UnsupportedCapability', 'dispatch', 'caller has no managed native export table')
  caller.bindNative(ctx, value)
}
export function registerNative(exports: Exports, ctx: Context, value: Outbound): void {
  exports.requireOpen()
  exports.inContext(ctx, () => {
    const register = (value: Outbound): void => {
      switch (value.kind) {
        case 'own': value.value.register(exports); break
        case 'record': Object.values(value.fields).forEach(register); break
        case 'list': value.items.forEach(register); break
        case 'optional': if (value.value !== null) register(value.value); break
      }
    }
    register(value)
  })
}
/** Own graph nodes retain their first actual native creator. Foreign grants
 * retain their original proof and are never rebound to this context. */
export function withNative(ctx: Context, value: Outbound): Outbound {
  switch (value.kind) {
    case 'own': {
      const inner = value.value
      return { kind: 'own', value: {
        register: exports => exports.inContext(ctx, () => inner.register(exports)),
        snapshot: () => inner.snapshot(),
      } }
    }
    case 'record': return { kind: 'record', fields: Object.fromEntries(Object.entries(value.fields).map(([name, value]) => [name, withNative(ctx, value)])) }
    case 'list': return { kind: 'list', items: value.items.map(value => withNative(ctx, value)) }
    case 'optional': return { kind: 'optional', value: value.value === null ? null : withNative(ctx, value.value) }
    default: return value
  }
}
export class CallContext {
  private children = new Set<Promise<void>>()
  private errors: unknown[] = []
  private rootFinished = false
  private closed = false
  constructor(readonly caller: Caller, private context?: Context, private admitted: () => boolean = () => context?.fiber.uid !== null) {}
  native(): Context {
    if (!this.context) throw new ProtocolError('Unavailable', 'dispatch', 'native context unavailable')
    return this.context
  }
  outbound(value: Outbound): Outbound {
    if (!this.context) return value
    if (!this.admitted()) throw new ProtocolError('ScopeClosed', 'dispatch', 'native creator closed before result encoding')
    return withNative(this.context, value)
  }
  spawn(work: () => Promise<void>): void {
    if (this.closed) throw new ProtocolError('ScopeClosed', 'dispatch', 'execution finished')
    const child = Promise.resolve().then(work).catch(error => { this.errors.push(error) }).finally(() => {
      this.children.delete(child)
      if (this.rootFinished && !this.children.size) this.closed = true
    })
    this.children.add(child)
  }
  /** Runtime calls this after the handler settles, including failure. */
  async finish(): Promise<void> {
    this.rootFinished = true
    if (!this.children.size) this.closed = true
    while (this.children.size) await Promise.all([...this.children])
    if (this.errors.length) throw this.errors[0]
  }
}
export interface Dispatcher {
  dispatch(key: PinKey, context: CallContext, method: string, params: unknown): Promise<Outbound>
}
export interface RegisteredExport {
  identity: ObjectIdentity; interface: string; bundleSha256: string; dispatcher: Dispatcher
}
export interface NativeExport {
  register(exports: Exports): RegisteredExport
  snapshot(): Record<string, Outbound>
}
export class Client {
  constructor(readonly proxy: ObjectProxy, readonly caller: Caller, hash: string, iface: string) {
    const delivery = proxy.delivery()
    if (delivery.view.bundle_sha256 !== hash || delivery.view.interface !== iface) throw new ProtocolError('InterfaceMismatch', 'binding', 'generated interface does not match grant')
    Object.freeze(this)
  }
  call(method: string, params: Outbound): Promise<unknown> {
    this.proxy.delivery()
    return this.caller.call(this.proxy, method, params)
  }
  property(name: string): unknown { return this.proxy.property(name) }
}
const clients = new WeakMap<object, Client>()
const facades = new WeakMap<ObjectProxy, WeakMap<Caller, object>>()
/** Only generated, declared selectors are installed. No native object's keys,
 * getters or prototype are enumerated to discover an interface. */
export function facade<T extends object>(client: Client, methods: Record<string, (params: any) => Promise<any>>, properties: Record<string, () => unknown>): T {
  let cache = facades.get(client.proxy)
  if (!cache) { cache = new WeakMap(); facades.set(client.proxy, cache) }
  const previous = cache.get(client.caller)
  if (previous) return previous as T
  const target = Object.create(null)
  for (const [name, method] of Object.entries(methods)) Object.defineProperty(target, name, { value: method, enumerable: true })
  for (const [name, get] of Object.entries(properties)) Object.defineProperty(target, name, { get, enumerable: true })
  Object.freeze(target)
  const result = new Proxy(target, {
    get(target, name, receiver) {
      if (Object.hasOwn(target, name)) return Reflect.get(target, name, receiver)
      if (name === 'then') return undefined // never accidentally assimilate a remote object as a Promise
      if (name === Symbol.toStringTag) return 'ProtocolObject'
      // Native Cordis probes this public metadata symbol when reading an
      // injected service. This facade has no native tracking metadata.
      if (name === Service.tracker) return undefined
      throw new ProtocolError('CapabilityDenied', 'binding', 'member is not declared')
    },
  })
  clients.set(result, client); cache.set(client.caller, result)
  return result as T
}
function rawHandle(value: object): Client {
  const client = clients.get(value)
  if (!client) throw new ProtocolError('InterfaceMismatch', 'binding', 'expected a generated client')
  return client
}
export function handle(value: object): Client {
  const client = rawHandle(value)
  client.proxy.delivery()
  return client
}
export function release(value: object): void {
  const client = clients.get(value)
  if (!client) throw new ProtocolError('InterfaceMismatch', 'binding', 'expected a generated client')
  client.proxy.release()
}
export function sameObject(a: object, b: object): boolean { return rawHandle(a).proxy.sameObject(rawHandle(b).proxy) }
export function object(value: unknown): ObjectProxy {
  if (!(value instanceof ObjectProxy)) throw new ProtocolError('InterfaceMismatch', 'binding', 'expected an object reference')
  return value
}
export function json(value: unknown): Outbound { return { kind: 'value', value } }
export function foreign(value: object): Outbound { return { kind: 'foreign', value: handle(value).proxy } }
export function deniedMethod(): ProtocolError { return new ProtocolError('CapabilityDenied', 'dispatch', 'method is not declared') }
