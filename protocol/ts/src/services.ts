import { createHash } from 'node:crypto'
import { Inject, type Context, type Plugin } from '@deepseek-ai/cordis'
import { admit, callbackKey, canonical, identifier, keys, record, type AdmittedBundle, type Method, type TypeExpr } from './contract.ts'
import { ProtocolError, type ErrorCode } from './error.ts'
import { draftGraph, GraphExporter, sourceObject, sourceView, type DraftGraph, type StagedGraph } from './draft.ts'
import { Exports, ObjectIds, type PinKey } from './exports.ts'
import { validateGraph, type GraphScopes, type WireGraph } from './graph.ts'
import { ObjectProxy, sequence, type Activation } from './imports.ts'
import { ActivationGate, ManagedActivation } from './managed.ts'
import { handle, type Caller, type Outbound } from './sdk.ts'
import type { CatalogService, Contracts } from './lifecycle.ts'
import { signatureCanonical } from './json.ts'

function fail(code: ErrorCode, message: string): never { throw new ProtocolError(code, 'services', message) }
function immutable<T>(input: T): T {
  const freeze = (value: any): void => {
    if (!value || typeof value !== 'object') return
    for (const child of Object.values(value)) freeze(child)
    Object.freeze(value)
  }
  const result = structuredClone(input); freeze(result); return result
}
function activation(value: unknown): asserts value is Activation {
  keys(value, ['runtime', 'epoch', 'activation'])
  if (typeof value.runtime !== 'string' || !identifier(value.runtime)) fail('InvalidParams', 'invalid service table activation')
  sequence(value.epoch); sequence(value.activation)
}
function sameNames(a: object, b: object): boolean { return canonical(Object.keys(a).sort()) === canonical(Object.keys(b).sort()) }

/** Exact raw bundle admission precedes native factory or module construction. */
export class Bundles {
  private readonly admitted = new Map<string, AdmittedBundle>()
  constructor(bytes: Iterable<Uint8Array>) {
    const identities = new Map<string, string>()
    for (const raw of bytes) {
      const bundle = admit(raw)
      const identity = canonical([bundle.bundle.id, bundle.bundle.version])
      if (identities.has(identity) && identities.get(identity) !== bundle.sha256) fail('InterfaceMismatch', 'same bundle identity has different raw bytes')
      identities.set(identity, bundle.sha256); this.admitted.set(bundle.sha256, bundle)
    }
  }
  service(contract: CatalogService): AdmittedBundle {
    keys(contract, ['interface', 'version', 'bundle_sha256'])
    const bundle = this.admitted.get(contract.bundle_sha256)
    if (!bundle || bundle.bundle.version !== contract.version || !Object.hasOwn(bundle.bundle.interfaces, contract.interface)) fail('InterfaceMismatch', 'named service contract differs from exact bundle')
    return bundle
  }
  exact(hash: string): AdmittedBundle {
    const bundle = this.admitted.get(hash)
    if (!bundle) fail('InterfaceMismatch', 'object bundle was not admitted')
    return bundle
  }
  method(view: { interface: string; bundle_sha256: string }, name: string): Method {
    const bundle = this.exact(view.bundle_sha256).bundle
    const iface = Object.hasOwn(bundle.interfaces, view.interface) ? bundle.interfaces[view.interface] : undefined
    if (iface && Object.hasOwn(iface.methods, name)) return iface.methods[name]
    const scan = (type: TypeExpr): Method | undefined => {
      switch (type.kind) {
        case 'callback': return callbackKey(type) === view.interface ? { params: type.params, result: type.result } : scan(type.params) ?? scan(type.result)
        case 'record': return Object.values(type.fields).map(scan).find(Boolean)
        case 'list': case 'optional': return scan(type.item)
      }
    }
    if (name === 'call') {
      for (const iface of Object.values(bundle.interfaces)) {
        for (const method of Object.values(iface.methods)) { const found = scan(method.params) ?? scan(method.result); if (found) return found }
        for (const type of Object.values(iface.properties ?? {})) { const found = scan(type); if (found) return found }
      }
      for (const event of Object.values(bundle.events ?? {})) { const found = scan(event.params) ?? scan(event.result); if (found) return found }
    }
    fail('CapabilityDenied', 'unknown exact interface selector')
  }
}
export interface ServiceDraft { stage: string; graph: DraftGraph }
export interface ServiceTable { activation: Activation; services: Record<string, ServiceDraft> }
export function serviceSource(owner: Activation, name: string): string {
  return createHash('sha256').update(signatureCanonical(['protocol-service', owner, name])).digest('hex')
}
export function serviceType(contract: CatalogService): TypeExpr { return { kind: 'object', interface: contract.interface, ownership: 'scope' } }
export function validateTable(input: ServiceTable, contracts: Record<string, CatalogService>, bundles: Bundles): void {
  keys(input, ['activation', 'services']); activation(input.activation); record(input.services)
  if (!sameNames(input.services, contracts)) fail('InterfaceMismatch', 'complete named service table differs')
  const stages = new Set<string>()
  const scope = { activation: input.activation, scope: '1' }
  for (const [name, service] of Object.entries(input.services)) {
    keys(service, ['stage', 'graph']); sequence(service.stage)
    if (!identifier(name) || stages.has(service.stage)) fail('InvalidParams', 'invalid/duplicate service staging identity')
    stages.add(service.stage)
    draftGraph(bundles.service(contracts[name]), serviceType(contracts[name]), service.graph, { scope, borrow: scope })
    const root = service.graph.root
    if (root.kind !== 'ref') fail('InterfaceMismatch', 'named service must be an object root')
    const source = service.graph.references[root.index].source
    if (source.kind !== 'own' || canonical(source.object.owner) !== canonical(input.activation)) fail('CapabilityDenied', 'named root must be native-owned by this activation')
    for (const reference of service.graph.references) {
      const source = reference.source
      if (source.kind === 'own') {
        if (canonical(source.object.owner) !== canonical(input.activation) || source.view.source !== serviceSource(input.activation, name)) fail('CapabilityDenied', 'named graph changed its owner or service source')
      } else if (canonical(source.delivery.recipient.activation) !== canonical(input.activation)) fail('CapabilityDenied', 'foreign proof belongs to another activation')
    }
  }
}

interface ExportPort { native: string; contract: CatalogService; export: (value: object) => Outbound }
interface ImportPort { native: string; contract: CatalogService; bind: (value: ObjectProxy, caller: Caller) => object }
/** Native names stay local; wire names bind exact bundle contracts. No grants,
 * object IDs, or reflection of undeclared members are exposed to authors. */
export class NativePorts {
  private exports = new Map<string, ExportPort>()
  private imports = new Map<string, ImportPort>()
  private names = new Set<string>()
  private sealed = false
  private reserve(name: string, native: string, exists: boolean): void {
    if (this.sealed || !identifier(name) || !identifier(native) || exists || this.names.has(native)) fail('InvalidParams', 'duplicate/invalid native service port or key')
    this.names.add(native)
  }
  provide<T extends object>(name: string, native: string, contract: CatalogService, bundles: Bundles, exportValue: (value: T) => Outbound): void {
    bundles.service(contract); this.reserve(name, native, this.exports.has(name))
    this.exports.set(name, { native, contract: immutable(contract), export: value => exportValue(value as T) })
  }
  require<T extends object>(name: string, native: string, contract: CatalogService, bundles: Bundles, bind: (value: ObjectProxy, caller: Caller) => T): void {
    bundles.service(contract); this.reserve(name, native, this.imports.has(name))
    this.imports.set(name, { native, contract: immutable(contract), bind })
  }
  check(catalog: Contracts): void {
    const provides = Object.fromEntries([...this.exports].map(([name, port]) => [name, port.contract]))
    const requires = Object.fromEntries([...this.imports].map(([name, port]) => [name, port.contract]))
    if (canonical(provides) !== canonical(catalog.provides) || canonical(requires) !== canonical(catalog.requires)) fail('InterfaceMismatch', 'native ports differ from frozen factory service declarations')
  }
  async mount<C>(parent: Context, plugin: Plugin.Object<C>, config: C, owner: Activation, values: Record<string, unknown>, caller: Caller, gate = new ActivationGate()): Promise<NativeServices<C>> {
    activation(owner); record(values)
    if (!gate.isOpen) fail('Unavailable', 'native bindings cannot be installed or reused')
    const required = [...this.imports.values()].map(port => port.native).sort()
    const inject = Inject.resolve(plugin.inject)
    const actual = Object.keys(inject).sort()
    if (canonical(required) !== canonical(actual)) fail('InterfaceMismatch', 'native plugin injects differ from linked service ports')
    if (!sameNames(values, Object.fromEntries(this.imports))) fail('InterfaceMismatch', 'native required service table differs')
    const dependencies: Record<string, object> = Object.create(null)
    const checks: Record<string, () => boolean> = Object.create(null)
    // All typed binding checks precede the first native provider side effect.
    for (const [name, port] of this.imports) {
      const value = values[name]
      if (!(value instanceof ObjectProxy)) fail('InterfaceMismatch', 'expected an object root')
      const proof = value.delivery()
      if (canonical(proof.recipient.activation) !== canonical(owner) || proof.recipient.scope !== '1') fail('CapabilityDenied', 'native import belongs to another activation root')
      const bound = port.bind(value, caller)
      const generated = handle(bound).proxy.delivery()
      if (generated.view.interface !== port.contract.interface || generated.view.bundle_sha256 !== port.contract.bundle_sha256) fail('InterfaceMismatch', 'generated port contract differs')
      dependencies[port.native] = bound
      checks[port.native] = () => { try { value.delivery(); return gate.isOpen } catch { return false } }
    }
    this.sealed = true
    const names = [...this.exports.values()].map(port => port.native)
    const native = await ManagedActivation.mount(parent, { ...plugin, inject }, config, dependencies, undefined, names, gate, checks)
    for (const name of names) native.requireExport(name)
    return new NativeServices(native, immutable(owner), this.exports, required)
  }
}

export class NativeServices<C = unknown> {
  private staged = false
  constructor(readonly native: ManagedActivation<C>, readonly owner: Activation, private ports: ReadonlyMap<string, ExportPort>, private imports: string[]) {}
  refreshImports(): void { this.native.refreshDependencies(this.imports) }
  async stage(bundles: Bundles, ids: ObjectIds, table?: Exports): Promise<StagedServices> {
    await this.native.ready()
    if (this.staged || !this.native.gate.isOpen) fail('Unavailable', 'native service table cannot be restaged')
    this.staged = true
    const ctx = this.native.nativeContext!
    const exports = table ?? Exports.managed(ctx, this.owner, ids, () => this.native.gate.isOpen)
    if (canonical(exports.activation) !== canonical(this.owner)) fail('CapabilityDenied', 'native table belongs to another member')
    const graphs = new Map<string, StagedGraph>()
    const exporters = new Map<string, GraphExporter>()
    const services: Record<string, ServiceDraft> = Object.create(null)
    const contracts: Record<string, CatalogService> = Object.create(null)
    try {
      for (const [name, port] of [...this.ports].sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0)) {
        const value = ctx.get(port.native)
        if (!value) fail('Unavailable', 'native export is unavailable')
        const exporter = new GraphExporter(bundles.service(port.contract), exports, this.owner, serviceSource(this.owner, name))
        const graph = exporter.encode(serviceType(port.contract), port.export(value))
        graphs.set(name, graph); exporters.set(name, exporter); contracts[name] = port.contract
        services[name] = { stage: String(graphs.size), graph: graph.draft }
      }
      const table = { activation: this.owner, services }
      validateTable(table, contracts, bundles)
      return new StagedServices(table, graphs, exporters, immutable(contracts), bundles, exports)
    } catch (error) { for (const graph of graphs.values()) graph.abort(); throw error }
  }
}

/** Complete private native manifests are checked before any staging pin is
 * converted. A failed pin handoff aborts every graph and cannot be retried. */
export class StagedServices {
  readonly table: ServiceTable
  private committed?: Record<string, WireGraph>
  private aborted = false
  constructor(table: ServiceTable, private graphs: Map<string, StagedGraph>, private exporters: Map<string, GraphExporter>, readonly contracts: Record<string, CatalogService>, private bundles: Bundles, readonly exports: Exports) { this.table = immutable(table) }
  exporter(name: string): GraphExporter {
    const exporter = this.exporters.get(name)
    if (!exporter) fail('InvalidParams', 'unknown named exporter')
    return exporter
  }
  mergeDispatchers(target: GraphExporter): void { for (const exporter of this.exporters.values()) target.mergeRegistered(exporter) }
  commit(graphs: Record<string, WireGraph>, scopes: GraphScopes): void {
    if (this.aborted) fail('ScopeClosed', 'named service handoff aborted')
    this.exports.requireOpen()
    if (this.committed) {
      if (canonical(graphs) !== canonical(this.committed)) fail('CapabilityDenied', 'committed named service table changed')
      return
    }
    record(graphs)
    if (!sameNames(graphs, this.table.services)) fail('InterfaceMismatch', 'service commit table differs')
    for (const [name, graph] of Object.entries(graphs)) {
      validateGraph(this.bundles.service(this.contracts[name]), serviceType(this.contracts[name]), graph, scopes)
      const draft = this.table.services[name].graph
      if (canonical(graph.root) !== canonical(draft.root) || graph.references.length !== draft.references.length || graph.references.some((wire, index) => {
        const reference = draft.references[index]
        return canonical(wire.delivery.object) !== canonical(sourceObject(reference.source)) || canonical(wire.delivery.view) !== canonical(sourceView(reference.source)) || canonical(wire.properties) !== canonical(reference.properties)
      })) fail('CapabilityDenied', 'service commit changed native object manifest')
    }
    try {
      for (const [name, graph] of Object.entries(graphs)) this.graphs.get(name)!.commit(graph.references.map(reference => reference.delivery), scopes)
      this.committed = immutable(graphs)
    } catch (error) {
      for (const [name, graph] of Object.entries(graphs)) graph.references.forEach((wire, index) => {
        if (this.table.services[name].graph.references[index].source.kind === 'own') this.exports.release({ type: 'delivery', recipient: wire.delivery.recipient.activation, id: wire.delivery.id } satisfies PinKey)
      })
      this.abort(); throw error
    }
  }
  abort(): void { if (!this.aborted) { this.aborted = true; for (const graph of this.graphs.values()) graph.abort() } }
}
