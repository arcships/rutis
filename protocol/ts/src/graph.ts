import { ProtocolError, keys, record, validateWire, canonical, type Bundle, type TypeExpr, type WireValue } from './contract.ts'
import { parseDelivery, type Delivery, type Scope, type ObjectIdentity, type InterfaceView } from './imports.ts'

export interface WireReference { delivery: Delivery; properties: Record<string, WireValue> }
export interface WireGraph { root: WireValue; references: WireReference[] }
export interface GraphScopes { scope: Scope; borrow: Scope }
export interface ObjectLink { scope: Scope; object: ObjectIdentity; view: InterfaceView }
export type SnapshotValue =
  | { kind: 'value'; value: unknown }
  | { kind: 'link'; link: ObjectLink }
  | { kind: 'record'; fields: Record<string, SnapshotValue> }
  | { kind: 'list'; items: SnapshotValue[] }
  | { kind: 'optional'; value: SnapshotValue | null }
export interface ValidatedGraph { root: SnapshotValue; deliveries: Delivery[]; properties: Record<string, SnapshotValue>[] }
export function link(delivery: Delivery): ObjectLink {
  return { scope: delivery.recipient, object: delivery.object, view: delivery.view }
}
export function linkKey(link: ObjectLink): string { return canonical([link.scope, link.object, link.view]) }
const fail = (code: 'InvalidParams' | 'InterfaceMismatch' | 'CapabilityDenied', message: string): never => { throw new ProtocolError(code, 'graph', message) }

/** Validate every contract and ownership edge before attaching any tokens.
 * Object relationships inherit the parent's grant scope, including borrow. */
export function validateGraph(admitted: { bundle: Bundle; sha256: string }, type: TypeExpr, input: WireGraph, scopes: GraphScopes): ValidatedGraph {
  if (canonical(scopes.scope.activation) !== canonical(scopes.borrow.activation)) fail('CapabilityDenied', 'persistent and borrow scopes belong to different activations')
  keys(input, ['root', 'references'])
  if (!Array.isArray(input.references)) fail('InvalidParams', 'reference table must be an array')
  const references = input.references.map(reference => {
    keys(reference, ['delivery', 'properties']); record(reference.properties)
    return { delivery: parseDelivery(reference.delivery), properties: reference.properties }
  })
  const interfaces = references.map(r => r.delivery.view.interface)
  for (const reference of references) {
    if (reference.delivery.view.bundle_sha256 !== admitted.sha256) fail('InterfaceMismatch', 'snapshot references another interface bundle')
    if (reference.delivery.view.interface.startsWith('$callback:')) {
      if (Object.keys(reference.properties).length) fail('InvalidParams', 'callback references cannot have snapshot properties')
      continue
    }
    const iface = admitted.bundle.interfaces[reference.delivery.view.interface]
    if (!iface) fail('InterfaceMismatch', 'snapshot interface is not declared')
  }
  for (const reference of references) {
    const iface = admitted.bundle.interfaces[reference.delivery.view.interface]
    if (!iface) continue
    keys(reference.properties, Object.keys(iface.properties ?? {}))
    for (const [name, value] of Object.entries(reference.properties)) validateWire(iface.properties![name], value, interfaces)
  }
  validateWire(type, input.root, interfaces)
  const expected = new Map<number, Scope>()
  const queue: number[] = []
  function assign(type: TypeExpr, value: WireValue, inherited?: Scope): void {
    if (value.kind === 'ref' && (type.kind === 'object' || type.kind === 'callback')) {
      const scope = inherited ?? (type.ownership === 'scope' ? scopes.scope : scopes.borrow)
      const old = expected.get(value.index)
      if (old && canonical(old) !== canonical(scope)) fail('CapabilityDenied', 'one reference cannot cross ownership scopes')
      if (!old) { expected.set(value.index, scope); queue.push(value.index) }
    } else if (value.kind === 'record' && type.kind === 'record') {
      for (const [name, expr] of Object.entries(type.fields)) assign(expr, value.fields[name], inherited)
    } else if (value.kind === 'list' && type.kind === 'list') {
      for (const item of value.items) assign(type.item, item, inherited)
    } else if (value.kind === 'optional' && type.kind === 'optional' && value.value) assign(type.item, value.value, inherited)
  }
  assign(type, input.root)
  const reached = new Set<number>()
  for (let at = 0; at < queue.length; at++) {
    const index = queue[at]
    if (reached.has(index)) continue
    reached.add(index)
    const reference = references[index]
    const scope = expected.get(index)!
    if (canonical(reference.delivery.recipient) !== canonical(scope)) fail('CapabilityDenied', 'reference would escape its declared ownership scope')
    const iface = admitted.bundle.interfaces[reference.delivery.view.interface]
    if (iface) for (const [name, value] of Object.entries(reference.properties)) assign(iface.properties![name], value, scope)
  }
  if (reached.size !== references.length) fail('InvalidParams', 'reference table contains unconsumed deliveries')
  function normalize(value: WireValue): SnapshotValue {
    switch (value.kind) {
      case 'value': return { kind: 'value', value: structuredClone(value.value) }
      case 'ref': return { kind: 'link', link: link(references[value.index].delivery) }
      case 'record': return { kind: 'record', fields: Object.fromEntries(Object.entries(value.fields).map(([name, value]) => [name, normalize(value)])) }
      case 'list': return { kind: 'list', items: value.items.map(normalize) }
      case 'optional': return { kind: 'optional', value: value.value ? normalize(value.value) : null }
    }
  }
  const properties = references.map(reference => Object.fromEntries(Object.entries(reference.properties).map(([name, value]) => [name, normalize(value)])))
  const snapshots = new Map<string, Record<string, SnapshotValue>>()
  for (let i = 0; i < references.length; i++) {
    const key = linkKey(link(references[i].delivery))
    const old = snapshots.get(key)
    if (old && canonical(old) !== canonical(properties[i])) fail('InvalidParams', 'one object view has conflicting immutable snapshots')
    snapshots.set(key, properties[i])
  }
  return { root: normalize(input.root), deliveries: references.map(r => r.delivery), properties }
}
