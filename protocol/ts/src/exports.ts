import { ProtocolError } from './error.ts'
import { canonical } from './contract.ts'
import { sequence, type Activation, type ObjectIdentity } from './imports.ts'
import type { Context } from '@deepseek-ai/cordis'

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
  pins: number; disposed: boolean
  disposer?: (object: object) => Promise<void> | void
}
const fail = (code: 'ScopeClosed' | 'StaleObject' | 'CapabilityDenied' | 'InvalidParams', message: string): never => { throw new ProtocolError(code, 'export', message) }
export class Exports {
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
  constructor(owner: Activation, private ids: ObjectIds, private admitted: () => boolean = () => true) { this.owner = Object.freeze(structuredClone(owner)) }

  static managed(ctx: Context, owner: Activation, ids: ObjectIds, gate: () => boolean = () => true): Exports {
    const fiber = ctx.fiber
    const exports = new Exports(owner, ids, () => fiber.uid !== null && gate())
    ctx.effect(() => () => { exports.close(); return exports.join() })
    return exports
  }

  register<T extends object>(object: T): ObjectIdentity { return this.add(object) }
  registerExclusive<T extends object>(object: T, disposer: (object: T) => Promise<void> | void): ObjectIdentity {
    return this.add(object, disposer as (object: object) => Promise<void> | void)
  }
  private add(object: object, disposer?: (object: object) => Promise<void> | void): ObjectIdentity {
    if (!this.open || !this.admitted()) fail('ScopeClosed', 'owner activation closed')
    const old = this.identities.get(object)
    if (old) {
      if (old.disposed) fail('StaleObject', 'exclusive object disposed')
      if (disposer) fail('InvalidParams', 'exclusive disposer is already registered')
      return old.identity
    }
    const identity = Object.freeze({ owner: this.owner, object: this.ids.allocate() })
    const entry: Entry = { identity, weak: new WeakRef(object), held: disposer ? object : undefined, pins: 0, disposed: false, disposer }
    this.identities.set(object, entry); this.entries.set(identity.object, entry)
    return identity
  }
  pin(identity: ObjectIdentity, key: PinKey): void {
    if (!this.open || !this.admitted()) fail('ScopeClosed', 'owner activation closed')
    const lease = canonical(key)
    if (this.released.has(lease) || this.isRetired(key)) fail('StaleObject', 'released pin cannot be reacquired')
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
  release(key: PinKey): void {
    const lease = canonical(key)
    if (!this.isRetired(key)) this.released.set(lease, structuredClone(key))
    const entry = this.leases.get(lease)
    if (!entry) return
    this.leases.delete(lease); this.unpin(entry); this.notify()
  }
  close(): void {
    this.open = false
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
    const cleanup = Promise.resolve().then(() => disposer(object)).catch(error => { this.errors.push(error) })
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
    for (const [id, entry] of this.entries) if (!entry.pins && !entry.weak.deref()) this.entries.delete(id)
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
