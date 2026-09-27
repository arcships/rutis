import { ProtocolError } from './error.ts'
import { canonical } from './contract.ts'
import { sequence, type Activation, type ObjectIdentity } from './imports.ts'
import { Context } from '@deepseek-ai/cordis'

export type PinKey =
  | { type: 'staging'; id: string }
  | { type: 'delivery'; recipient: Activation; id: string }
  | { type: 'execution'; caller: Activation; call: string }
export class ObjectIds {
  private next = 0n
  allocate(): string {
    const next = this.next + 1n
    if (next > 18_446_744_073_709_551_615n) throw new ProtocolError('Unavailable', 'export', 'object sequence exhausted')
    sequence(next.toString()); this.next = next
    return next.toString()
  }
}
interface Entry {
  identity: ObjectIdentity; weak: WeakRef<object>; held?: object
  pins: number; disposed: boolean; closed: boolean
  cleanup?: Promise<void>; cleanupError?: unknown
  disposer?: (object: object) => Promise<void> | void
  native?: NativeCreator
}
export interface NativeCreator { readonly context: Context; isOpen(): boolean }
const fail = (code: 'ScopeClosed' | 'StaleObject' | 'CapabilityDenied' | 'InvalidParams', message: string): never => { throw new ProtocolError(code, 'export', message) }
export class Exports {
  private staging = new ObjectIds()
  private open = true
  private identities = new WeakMap<object, Entry>()
  private entries = new Map<string, Entry>()
  private leases = new Map<string, Entry>()
  private released = new Map<string, PinKey>()
  private retired = new Map<string, bigint>()
  private closedEpochs = new Map<string, bigint>()
  private cleanups = new Set<Promise<void>>()
  private errors: unknown[] = []
  private waiters = new Set<() => void>()
  private owner: Activation
  private native?: Context
  private revoke?: (objects: ObjectIdentity[]) => Promise<void>
  private pendingRevocations = new Map<string, ObjectIdentity>()
  private revocations = new Map<string, Promise<void>>()
  private ownerClosed!: () => void
  private ownerClosing = new Promise<void>(resolve => { this.ownerClosed = resolve })
  constructor(owner: Activation, private ids: ObjectIds, private admitted: () => boolean = () => true, private nativeRoot?: Context) {
    this.owner = Object.freeze(structuredClone(owner)); this.native = nativeRoot
  }
  get activation(): Activation { return this.owner }
  requireOpen(): void {
    if (!this.admitted()) this.open = false
    if (!this.open) fail('ScopeClosed', 'owner activation closed')
  }

  static managed(ctx: Context, owner: Activation, ids: ObjectIds, gate: () => boolean = () => true): Exports {
    const fiber = ctx.fiber
    const exports = new Exports(owner, ids, () => fiber.uid !== null && gate(), ctx)
    ctx.on('internal/status', changed => {
      if (changed === fiber || (changed.state !== 3 && changed.state !== 4 && changed.state !== 5)) return
      const closed: ObjectIdentity[] = []
      for (const entry of exports.entries.values()) {
        let child = entry.native?.context.fiber
        const seen = new Set<object>()
        while (child && child !== fiber && !seen.has(child)) {
          if (child === changed) { closed.push(entry.identity); break }
          seen.add(child); child = child.parent.fiber
        }
      }
      exports.closeObjects(closed)
    }, { global: true, prepend: true })
    ctx.effect(() => () => { exports.close(); return exports.join() })
    return exports
  }

  register<T extends object>(object: T): ObjectIdentity { return this.add(object) }
  /** Synchronous registration keeps the original context, without allocating
   * another managed activation or taking ownership of the native child. */
  inContext<T>(ctx: Context, register: () => T): T {
    if (!Context.is(ctx) || !this.nativeRoot) fail('CapabilityDenied', 'export creator is outside its managed native subtree')
    let fiber = ctx.fiber
    const visited = new Set<object>()
    while (fiber !== this.nativeRoot!.fiber) {
      if (visited.has(fiber) || fiber.parent.fiber === fiber) fail('CapabilityDenied', 'export creator is outside its managed native subtree')
      visited.add(fiber); fiber = fiber.parent.fiber
    }
    if (ctx.fiber.uid === null || (ctx.fiber.state !== 1 && ctx.fiber.state !== 2)) fail('ScopeClosed', 'export creator is closed')
    const previous = this.native; this.native = ctx
    try { return register() } finally { this.native = previous }
  }
  stage(identity: ObjectIdentity): PinKey {
    const key: PinKey = { type: 'staging', id: this.staging.allocate() }
    this.pin(identity, key); return key
  }
  registerExclusive<T extends object>(object: T, disposer: (object: T) => Promise<void> | void): ObjectIdentity {
    return this.add(object, disposer as (object: object) => Promise<void> | void)
  }
  private add(object: object, disposer?: (object: object) => Promise<void> | void): ObjectIdentity {
    this.requireOpen()
    const old = this.identities.get(object)
    if (old) {
      if (old.closed || (old.native && !old.native.isOpen())) fail('ScopeClosed', 'export creator is closed')
      if (old.disposed) fail('StaleObject', 'exclusive object disposed')
      if (disposer) fail('InvalidParams', 'exclusive disposer is already registered')
      return old.identity
    }
    const identity = Object.freeze({ owner: this.owner, object: this.ids.allocate() })
    const context = this.native
    const native = context ? { context, isOpen: () => !entry.closed && context.fiber.uid !== null && (context.fiber.state === 1 || context.fiber.state === 2) } : undefined
    const entry: Entry = { identity, weak: new WeakRef(object), held: disposer ? object : undefined, pins: 0, disposed: false, closed: false, disposer, native }
    if (native && native.context.fiber !== this.nativeRoot?.fiber) native.context.effect(() => () => {
      this.closeObjects([identity]); return this.joinObject(entry)
    })
    this.identities.set(object, entry); this.entries.set(identity.object, entry)
    return identity
  }
  pin(identity: ObjectIdentity, key: PinKey): void {
    this.requireOpen()
    const lease = canonical(key)
    if (this.released.has(lease) || this.isRetired(key)) fail('StaleObject', 'released pin cannot be reacquired')
    const registered = this.entries.get(identity.object)
    const creator = registered?.native
    if (registered?.closed || (creator && !creator.isOpen())) fail('ScopeClosed', 'export creator is closed')
    const old = this.leases.get(lease)
    if (old) {
      if (canonical(old.identity) !== canonical(identity)) fail('CapabilityDenied', 'pin key changed object')
      return
    }
    const entry = this.entries.get(identity.object)
    if (!entry || canonical(entry.identity) !== canonical(identity)) return fail('StaleObject', 'unknown object')
    if (entry.disposed) fail('StaleObject', 'exclusive object disposed')
    const object = entry.held ?? entry.weak.deref()
    if (!object) fail('StaleObject', 'local object already dropped')
    entry.held = object; entry.pins++; this.leases.set(lease, entry)
  }
  executionObject(key: PinKey): object {
    if (key.type !== 'execution') fail('CapabilityDenied', 'execution pin required')
    const entry = this.leases.get(canonical(key))
    if (!entry?.held) return fail('StaleObject', 'execution already finished')
    return entry.held
  }
  nativeContext(identity: ObjectIdentity): Context | undefined {
    return this.entries.get(identity.object)?.native?.context
  }
  executionContext(key: PinKey): NativeCreator | undefined {
    if (key.type !== 'execution') fail('CapabilityDenied', 'execution pin required')
    const entry = this.leases.get(canonical(key))
    if (!entry) return fail('StaleObject', 'execution already finished')
    const creator = entry.native
    if (creator && !creator.isOpen()) fail('ScopeClosed', 'export creator is closed')
    return creator
  }
  release(key: PinKey): void {
    const lease = canonical(key)
    if (!this.isRetired(key)) this.released.set(lease, structuredClone(key))
    const entry = this.leases.get(lease)
    if (!entry) return
    this.leases.delete(lease); this.unpin(entry); this.notify()
  }
  onRevoke(revoke: (objects: ObjectIdentity[]) => Promise<void>): void {
    if (this.revoke) fail('CapabilityDenied', 'object table already has a revocation transport')
    this.revoke = revoke; this.sendRevocations()
  }
  private closeObjects(objects: ObjectIdentity[]): void {
    const closed = new Set(objects.map(object => object.object))
    for (const object of objects) {
      const entry = this.entries.get(object.object)
      if (!entry || entry.closed) continue
      entry.closed = true
      if (this.open) this.pendingRevocations.set(object.object, object)
    }
    for (const [key, entry] of this.leases) {
      if (JSON.parse(key).type === 'execution' || !closed.has(entry.identity.object)) continue
      this.released.set(key, JSON.parse(key)); this.leases.delete(key); this.unpin(entry)
    }
    for (const id of closed) {
      const entry = this.entries.get(id)
      if (entry && !entry.pins && entry.held && entry.disposer) this.cleanup(entry)
    }
    this.sendRevocations(); this.notify()
  }
  private sendRevocations(): void {
    if (!this.revoke || !this.pendingRevocations.size) return
    const objects = [...this.pendingRevocations.values()]; this.pendingRevocations.clear()
    const revoke = this.revoke
    const receipt = Promise.resolve().then(() => revoke(objects))
    void receipt.catch(error => { this.errors.push(error) })
    for (const object of objects) this.revocations.set(object.object, receipt)
  }
  private async joinObject(entry: Entry): Promise<void> {
    while (entry.pins) await new Promise<void>(resolve => this.waiters.add(resolve))
    await entry.cleanup
    if ('cleanupError' in entry) throw entry.cleanupError
    if (this.open) {
      const receipt = this.revocations.get(entry.identity.object)
      if (receipt) await Promise.race([receipt, this.ownerClosing])
    }
  }
  close(): void {
    this.open = false
    this.ownerClosed()
    for (const [key, entry] of this.leases) {
      if (JSON.parse(key).type === 'execution') continue
      this.released.set(key, JSON.parse(key))
      this.leases.delete(key); this.unpin(entry)
    }
    for (const entry of this.entries.values()) if (!entry.pins && entry.held && entry.disposer) this.cleanup(entry)
    this.notify()
  }
  private unpin(entry: Entry): void {
    if (--entry.pins !== 0) return
    this.cleanup(entry)
  }
  private cleanup(entry: Entry): void {
    const object = entry.held!
    entry.held = undefined
    const disposer = entry.disposer
    if (!disposer) return
    entry.disposed = true; entry.disposer = undefined
    const cleanup = Promise.resolve().then(() => disposer(object)).catch(error => { this.errors.push(error); entry.cleanupError = error })
    entry.cleanup = cleanup
    this.cleanups.add(cleanup)
    void cleanup.then(() => { this.cleanups.delete(cleanup); this.notify() })
  }
  private notify(): void { for (const waiter of this.waiters) waiter(); this.waiters.clear() }
  async join(): Promise<void> {
    while (this.leases.size || this.cleanups.size) await new Promise<void>(resolve => this.waiters.add(resolve))
    if (this.errors.length) throw this.errors[0]
  }
  pins(identity: ObjectIdentity): number {
    const entry = this.entries.get(identity.object)
    return entry && canonical(entry.identity) === canonical(identity) ? entry.pins : 0
  }
  sweep(): void {
    for (const [id, entry] of this.entries) if (!entry.pins && !entry.weak.deref() && !this.cleanups.has(entry.cleanup!)) this.entries.delete(id)
  }
  private isRetired(key: PinKey): boolean {
    return key.type === 'delivery' && (BigInt(key.id) <= (this.retired.get(canonical([key.recipient.runtime, key.recipient.epoch])) ?? 0n)
      || BigInt(key.recipient.epoch) <= (this.closedEpochs.get(key.recipient.runtime) ?? 0n))
  }
  retireDeliveries(runtime: string, epoch: string, through: string): void {
    sequence(epoch); sequence(through)
    const target = canonical([runtime, epoch]); const prefix = BigInt(through)
    if (prefix < (this.retired.get(target) ?? 0n)) fail('InvalidParams', 'watermark cannot move backwards')
    const matches = (key: PinKey) => key.type === 'delivery' && key.recipient.runtime === runtime && key.recipient.epoch === epoch && BigInt(key.id) <= prefix
    if ([...this.leases.keys()].some(k => matches(JSON.parse(k)))) fail('InvalidParams', 'unconfirmed export delivery retirement')
    for (const [key, pin] of this.released) if (matches(pin)) this.released.delete(key)
    this.retired.set(target, prefix)
  }
  closeRecipientEpoch(runtime: string, epoch: string): void {
    sequence(epoch)
    const prefix = BigInt(epoch)
    this.closedEpochs.set(runtime, prefix > (this.closedEpochs.get(runtime) ?? 0n) ? prefix : this.closedEpochs.get(runtime)!)
    const matches = (key: PinKey) => key.type === 'delivery' && key.recipient.runtime === runtime && BigInt(key.recipient.epoch) <= prefix
    for (const [key, entry] of this.leases) if (matches(JSON.parse(key))) { this.leases.delete(key); this.unpin(entry) }
    for (const [key, pin] of this.released) if (matches(pin)) this.released.delete(key)
    for (const key of this.retired.keys()) {
      const [recipient, current] = JSON.parse(key)
      if (recipient === runtime && BigInt(current) <= prefix) this.retired.delete(key)
    }
    this.notify()
  }
}
