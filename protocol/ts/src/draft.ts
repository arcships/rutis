import { callbackKey, canonical, keys, validateWire, type AdmittedBundle, type TypeExpr, type WireValue } from './contract.ts'
import { ProtocolError } from './error.ts'
import { Exports, type PinKey } from './exports.ts'
import { ObjectProxy, parseDelivery, type Activation, type Delivery, type InterfaceView, type ObjectIdentity } from './imports.ts'
import { validateGraph, type GraphScopes, type WireGraph } from './graph.ts'
import type { Dispatcher, Outbound } from './sdk.ts'

export type DraftSource = { kind: 'own'; object: ObjectIdentity; view: InterfaceView } | { kind: 'foreign'; delivery: Delivery }
export interface DraftReference { source: DraftSource; ownership: 'scope' | 'borrow'; properties: Record<string, WireValue> }
export interface DraftGraph { root: WireValue; references: DraftReference[] }
function fail(code: 'InvalidParams' | 'InterfaceMismatch' | 'CapabilityDenied' | 'ScopeClosed', message: string): never { throw new ProtocolError(code, 'encode', message) }
export function sourceObject(source: DraftSource): ObjectIdentity { return source.kind === 'own' ? source.object : source.delivery.object }
export function sourceView(source: DraftSource): InterfaceView { return source.kind === 'own' ? source.view : source.delivery.view }

/** Placeholder deliveries are only for shape/type validation. They never
 * enter Imports or Exports and carry no authority. The broker issues grants. */
export function draftGraph(admitted: AdmittedBundle, type: TypeExpr, input: DraftGraph, scopes: GraphScopes): WireGraph {
  keys(input, ['root', 'references'])
  if (!Array.isArray(input.references)) fail('InvalidParams', 'reference table must be an array')
  const references = input.references.map((reference, index) => {
    keys(reference, ['source', 'ownership', 'properties'])
    if (!['scope', 'borrow'].includes(reference.ownership)) fail('InvalidParams', 'invalid draft ownership')
    if (reference.source?.kind === 'own') keys(reference.source, ['kind', 'object', 'view'])
    else if (reference.source?.kind === 'foreign') keys(reference.source, ['kind', 'delivery'])
    else fail('InvalidParams', 'invalid draft source')
    const delivery = parseDelivery({ id: String(index + 1), token: 'validation-only', object: sourceObject(reference.source), view: sourceView(reference.source), recipient: scopes[reference.ownership] })
    if (reference.source.kind === 'foreign') parseDelivery(reference.source.delivery)
    return { delivery, properties: reference.properties }
  })
  const graph = { root: input.root, references }
  validateGraph(admitted, type, graph, scopes)
  return graph
}
function outbound(value: unknown, type: TypeExpr): Outbound {
  switch (type.kind) {
    case 'value': return { kind: 'value', value }
    case 'object': case 'callback':
      if (!(value instanceof ObjectProxy)) fail('InterfaceMismatch', 'expected a decoded object')
      return { kind: 'foreign', value }
    case 'record': return { kind: 'record', fields: Object.fromEntries(Object.entries(type.fields).map(([name, expr]) => [name, outbound((value as any)[name], expr)])) }
    case 'list': return { kind: 'list', items: (value as unknown[]).map(item => outbound(item, type.item)) }
    case 'optional': return { kind: 'optional', value: value === null ? null : outbound(value, type.item) }
    default: fail('InvalidParams', 'unsupported outbound type')
  }
}
export class GraphExporter {
  private dispatchers = new Map<string, Dispatcher>()
  constructor(readonly admitted: AdmittedBundle, readonly exports: Exports, readonly owner: Activation, private source: string) {}
  dispatcher(object: ObjectIdentity, view: InterfaceView): Dispatcher {
    const dispatch = this.dispatchers.get(canonical([object, view]))
    if (!dispatch) fail('InterfaceMismatch', 'native dispatch adapter unavailable')
    return dispatch
  }
  encode(type: TypeExpr, value: Outbound): StagedGraph {
    this.exports.requireOpen()
    const references: DraftReference[] = []
    const indices = new Map<string, number>()
    const pins: PinKey[] = []
    const visit = (type: TypeExpr, value: Outbound, inherited?: 'scope' | 'borrow'): WireValue => {
      if (type.kind === 'value' && value.kind === 'value') {
        validateWire(type, { kind: 'value', value: value.value }, [])
        return { kind: 'value', value: structuredClone(value.value) }
      }
      if (type.kind === 'record' && value.kind === 'record') {
        keys(value.fields, Object.keys(type.fields))
        return { kind: 'record', fields: Object.fromEntries(Object.entries(type.fields).map(([name, type]) => [name, visit(type, value.fields[name], inherited)])) }
      }
      if (type.kind === 'list' && value.kind === 'list') return { kind: 'list', items: value.items.map(item => visit(type.item, item, inherited)) }
      if (type.kind === 'optional' && value.kind === 'optional') return { kind: 'optional', value: value.value === null ? null : visit(type.item, value.value, inherited) }
      if ((type.kind !== 'object' && type.kind !== 'callback') || (value.kind !== 'own' && value.kind !== 'foreign')) fail('InvalidParams', 'outbound value does not match contract')
      const iface = type.kind === 'object' ? type.interface : callbackKey(type)
      const ownership = inherited ?? type.ownership
      let source: DraftSource
      if (value.kind === 'own') {
        const registered = value.value.register(this.exports)
        if (canonical(registered.identity.owner) !== canonical(this.owner) || registered.bundleSha256 !== this.admitted.sha256 || registered.interface !== iface) fail('InterfaceMismatch', 'native adapter contract mismatch')
        const key = this.exports.stage(registered.identity); pins.push(key)
        const view = { interface: iface, bundle_sha256: this.admitted.sha256, source: this.source }
        this.dispatchers.set(canonical([registered.identity, view]), registered.dispatcher)
        source = { kind: 'own', object: registered.identity, view }
      } else {
        const delivery = value.value.delivery()
        if (canonical(delivery.recipient.activation) !== canonical(this.owner)) fail('CapabilityDenied', 'foreign reference belongs to another activation')
        source = { kind: 'foreign', delivery }
      }
      if (sourceView(source).interface !== iface || sourceView(source).bundle_sha256 !== this.admitted.sha256) fail('InterfaceMismatch', 'reference contract mismatch')
      const key = canonical([sourceObject(source), sourceView(source), ownership])
      const old = indices.get(key)
      if (old !== undefined) return { kind: 'ref', index: old }
      const index = references.length; indices.set(key, index)
      const properties: Record<string, WireValue> = Object.create(null)
      references.push({ source, ownership, properties })
      const propertyTypes = this.admitted.bundle.interfaces[iface]?.properties ?? {}
      const fields = value.kind === 'own' ? value.value.snapshot() : Object.fromEntries(Object.entries(propertyTypes).map(([name, type]) => [name, outbound(value.value.property(name), type)]))
      keys(fields, Object.keys(propertyTypes))
      for (const [name, type] of Object.entries(propertyTypes)) properties[name] = visit(type, fields[name], ownership)
      return { kind: 'ref', index }
    }
    try {
      const graph = { root: visit(type, value), references }
      const scopes = { scope: { activation: this.owner, scope: '1' }, borrow: { activation: this.owner, scope: '2' } }
      draftGraph(this.admitted, type, graph, scopes)
      return new StagedGraph(this.admitted, this.exports, type, graph, pins)
    } catch (error) { for (const pin of pins) this.exports.release(pin); throw error }
  }
}
export class StagedGraph {
  private closed = false
  private committed?: WireGraph
  constructor(private admitted: AdmittedBundle, private exports: Exports, private type: TypeExpr, readonly draft: DraftGraph, private pins: PinKey[]) {}
  commit(inputs: Delivery[], scopes: GraphScopes): WireGraph {
    const deliveries = inputs.map(parseDelivery)
    if (this.committed) {
      if (canonical(deliveries) !== canonical(this.committed.references.map(r => r.delivery))) fail('CapabilityDenied', 'committed graph deliveries changed')
      return this.committed
    }
    if (this.closed) fail('ScopeClosed', 'staged graph aborted')
    const pinned: PinKey[] = []
    try {
      if (deliveries.length !== this.draft.references.length) fail('InvalidParams', 'incomplete graph handoff')
      const references = this.draft.references.map((reference, index) => {
        const delivery = deliveries[index]
        if (canonical(delivery.object) !== canonical(sourceObject(reference.source)) || canonical(delivery.view) !== canonical(sourceView(reference.source))) fail('CapabilityDenied', 'broker changed source object or view')
        return { delivery, properties: reference.properties }
      })
      const graph = { root: this.draft.root, references }
      validateGraph(this.admitted, this.type, graph, scopes)
      for (let index = 0; index < this.draft.references.length; index++) {
        if (this.draft.references[index].source.kind !== 'own') continue
        const delivery = deliveries[index]
        const pin: PinKey = { type: 'delivery', recipient: delivery.recipient.activation, id: delivery.id }
        this.exports.pin(delivery.object, pin); pinned.push(pin)
      }
      this.committed = graph
      return graph
    } catch (error) { for (const pin of pinned) this.exports.release(pin); throw error }
    finally { this.abort() }
  }
  abort(): void { if (!this.closed) { this.closed = true; for (const pin of this.pins) this.exports.release(pin); this.pins = [] } }
}
