import { canonical, keys, ProtocolError } from './contract.ts'

export interface Activation { runtime: string; epoch: string; activation: string }
export interface Scope { activation: Activation; scope: string }
export interface ObjectIdentity { owner: Activation; object: string }
export interface InterfaceView { interface: string; bundle_sha256: string; source: string }
export interface Delivery { id: string; token: string; object: ObjectIdentity; recipient: Scope; view: InterfaceView }
export type ImportControl = { type: 'accept' | 'release'; id: string; token: string }
const fail = (code: 'InvalidParams' | 'ScopeClosed' | 'StaleObject' | 'CapabilityDenied', message: string): never => { throw new ProtocolError(code, 'import', message) }
export function sequence(value: unknown): asserts value is string {
  if (typeof value !== 'string' || !/^[1-9]\d*$/.test(value) || BigInt(value) > 18_446_744_073_709_551_615n) fail('InvalidParams', 'sequence must be a canonical positive u64 decimal string')
}
function activation(value: any): void {
  keys(value, ['runtime', 'epoch', 'activation'])
  if (typeof value.runtime !== 'string') fail('InvalidParams', 'runtime must be a string')
  sequence(value.epoch); sequence(value.activation)
}
export function parseDelivery(value: unknown): Delivery {
  keys(value, ['id', 'token', 'object', 'recipient', 'view'])
  sequence(value.id)
  if (typeof value.token !== 'string') fail('InvalidParams', 'token must be a string')
  keys(value.object, ['owner', 'object']); activation(value.object.owner); sequence(value.object.object)
  keys(value.recipient, ['activation', 'scope']); activation(value.recipient.activation); sequence(value.recipient.scope)
  keys(value.view, ['interface', 'bundle_sha256', 'source'])
  if (Object.values(value.view).some(v => typeof v !== 'string')) fail('InvalidParams', 'view fields must be strings')
  // Retain our own immutable copy; a decoded object supplied to author code
  // must not allow mutation of the permission cache key or an old token.
  const result = structuredClone(value) as unknown as Delivery
  Object.freeze(result.object.owner); Object.freeze(result.object); Object.freeze(result.recipient.activation)
  Object.freeze(result.recipient); Object.freeze(result.view)
  return Object.freeze(result)
}

interface Seen { delivery: Delivery; terminal: boolean }
const wrappers = new WeakMap<ObjectProxy, { active: boolean; tokens: Set<string> }>()
/** Runtime-level serial critical section is the Node event loop, with no await
 * between checking a wrapper and attaching/removing its delivery tokens. */
export class Imports {
  private scopes = new Map<string, { scope: Scope; parent?: string }>()
  private latest = new Map<string, bigint>()
  private cache = new Map<string, WeakRef<ObjectProxy>>()
  private seen = new Map<string, Seen>()
  private retired = 0n
  private controls: ImportControl[] = []

  openScope(scope: Scope, parent?: Scope): void {
    const key = canonical(scope)
    const owner = canonical(scope.activation)
    sequence(scope.scope); activation(scope.activation)
    if (this.scopes.has(key)) fail('InvalidParams', 'scope already open')
    if (BigInt(scope.scope) <= (this.latest.get(owner) ?? 0n)) fail('StaleObject', 'scope id cannot be reused')
    if (parent && (!this.scopes.has(canonical(parent)) || canonical(parent.activation) !== owner)) fail('ScopeClosed', 'parent scope closed')
    this.latest.set(owner, BigInt(scope.scope))
    this.scopes.set(key, { scope: structuredClone(scope), parent: parent && canonical(parent) })
  }

  receive(input: Delivery): ObjectProxy { return this.receiveBatch([input])[0] }

  receiveBatch(inputs: Delivery[]): ObjectProxy[] {
    let deliveries: Delivery[]
    try {
      deliveries = inputs.map(parseDelivery)
      const batch = new Map<string, Delivery>()
      for (const delivery of deliveries) {
        if (BigInt(delivery.id) <= this.retired) fail('StaleObject', 'delivery is below retirement watermark')
        if (!this.scopes.has(canonical(delivery.recipient))) fail('ScopeClosed', 'delivery scope closed')
        const seen = this.seen.get(delivery.id)
        if (seen && canonical(seen.delivery) !== canonical(delivery)) fail('CapabilityDenied', 'delivery id changed identity')
        if (seen?.terminal) fail('StaleObject', 'released delivery cannot revive a wrapper')
        const old = batch.get(delivery.id)
        if (old && canonical(old) !== canonical(delivery)) fail('CapabilityDenied', 'conflicting delivery records in one graph')
        batch.set(delivery.id, delivery)
      }
    } catch (error) { this.reject(inputs); throw error }
    return deliveries.map(delivery => {
      const key = canonical([delivery.recipient, delivery.object, delivery.view])
      let proxy = this.cache.get(key)?.deref()
      if (!proxy?.active) proxy = new ObjectProxy(this, delivery.object, delivery.view, delivery.recipient)
      wrappers.get(proxy)!.tokens.add(delivery.id)
      this.cache.set(key, new WeakRef(proxy))
      this.seen.set(delivery.id, { delivery, terminal: false })
      this.controls.push({ type: 'accept', id: delivery.id, token: delivery.token })
      return proxy
    })
  }

  /** Failed typed graphs only release new handoffs, preserving earlier aliases. */
  reject(inputs: Delivery[]): void {
    for (const input of inputs) {
      let delivery: Delivery
      try { delivery = parseDelivery(input) } catch { continue }
      if (BigInt(delivery.id) <= this.retired || this.seen.has(delivery.id)) continue
      this.seen.set(delivery.id, { delivery, terminal: false }); this.releaseToken(delivery.id)
    }
  }

  closeScope(scope: Scope): void {
    const closed = new Set([canonical(scope)])
    let count = -1
    while (count !== closed.size) {
      count = closed.size
      for (const [key, value] of this.scopes) if (value.parent && closed.has(value.parent)) closed.add(key)
    }
    for (const key of closed) this.scopes.delete(key)
    for (const [key, ref] of this.cache) {
      const proxy = ref.deref()
      if (proxy && closed.has(canonical(proxy.scope))) { proxy.release(); this.cache.delete(key) }
      else if (!proxy) this.cache.delete(key)
    }
    for (const [id, seen] of this.seen) if (closed.has(canonical(seen.delivery.recipient))) this.releaseToken(id)
  }

  acknowledgeRetirement(through: string): void {
    sequence(through)
    const prefix = BigInt(through)
    if (prefix < this.retired) fail('InvalidParams', 'watermark cannot move backwards')
    if (prefix === this.retired) return
    const entries = [...this.seen].filter(([id]) => BigInt(id) > this.retired && BigInt(id) <= prefix)
    if (BigInt(entries.length) !== prefix - this.retired || entries.some(([, seen]) => !seen.terminal)) fail('InvalidParams', 'prefix has gaps or live tokens')
    for (const [id] of entries) this.seen.delete(id)
    for (const [key, ref] of this.cache) if (!ref.deref()?.active) this.cache.delete(key)
    this.retired = prefix
  }

  takeControls(): ImportControl[] { const controls = this.controls; this.controls = []; return controls }
  releaseToken(id: string): void {
    const seen = this.seen.get(id)
    if (seen && !seen.terminal) { seen.terminal = true; this.controls.push({ type: 'release', id, token: seen.delivery.token }) }
  }
  delivery(proxy: ObjectProxy): Delivery {
    if (!proxy.active || !this.scopes.has(canonical(proxy.scope))) fail('ScopeClosed', 'proxy wrapper released')
    const token = wrappers.get(proxy)!.tokens.values().next().value
    if (token === undefined) fail('StaleObject', 'proxy has no delivery token')
    return this.seen.get(token!)!.delivery
  }
}

/** TS assignment and Rust clone are aliases. release() closes all aliases in
 * the same scope; a later delivery creates a new wrapper, never revives this. */
export class ObjectProxy {
  constructor(private imports: Imports, readonly identity: ObjectIdentity, readonly view: InterfaceView, readonly scope: Scope) {
    wrappers.set(this, { active: true, tokens: new Set() })
    Object.freeze(this)
  }
  get active(): boolean { return wrappers.get(this)!.active }
  sameObject(other: ObjectProxy): boolean { return canonical(this.identity) === canonical(other.identity) }
  delivery(): Delivery { return this.imports.delivery(this) }
  release(): void {
    const state = wrappers.get(this)!
    if (!state.active) return
    state.active = false
    for (const id of state.tokens) this.imports.releaseToken(id)
    state.tokens.clear()
  }
}
