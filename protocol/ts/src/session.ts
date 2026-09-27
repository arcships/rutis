// Runtime objects share the authenticated lifecycle stream. Only the Rust Host
// broker issues grants; this actor owns actual native tables and staged values.
import { createHash } from 'node:crypto'
import type { Context } from '@deepseek-ai/cordis'
import { canonical, identifier, keys, type TypeExpr } from './contract.ts'
import { GraphExporter, type DraftGraph, type StagedGraph } from './draft.ts'
import { ProtocolError, type ErrorCode } from './error.ts'
import { Exports, ObjectIds, type PinKey } from './exports.ts'
import { Peer } from './frame.ts'
import { validateGraph, type GraphScopes, type WireGraph } from './graph.ts'
import { Imports, ObjectProxy, parseDelivery, sequence, type Activation, type Delivery, type InterfaceView, type Scope, type ObjectIdentity } from './imports.ts'
import { signatureCanonical } from './json.ts'
import { ActivationGate } from './managed.ts'
import { Runner, type CatalogService } from './lifecycle.ts'
import type { Duplex } from 'node:stream'
import { CallContext, registerNative, type Caller, type Outbound } from './sdk.ts'
import { Bundles, StagedServices, type ServiceTable } from './services.ts'

export interface RuntimeIdentity { runtime: string; epoch: string }
export interface StageOffer { activation: Activation; stage: string; source: string; draft: DraftGraph }
function fail(code: ErrorCode, message: string): never { throw new ProtocolError(code, 'object_session', message) }
function activation(value: unknown): asserts value is Activation {
  keys(value, ['runtime', 'epoch', 'activation'])
  if (typeof value.runtime !== 'string' || !identifier(value.runtime)) fail('InvalidParams', 'invalid native member identity')
  sequence(value.epoch); sequence(value.activation)
}
function scope(value: unknown): asserts value is Scope { keys(value, ['activation', 'scope']); activation(value.activation); sequence(value.scope) }
function scopes(value: unknown): asserts value is GraphScopes { keys(value, ['scope', 'borrow']); scope(value.scope); scope(value.borrow) }
function pin(value: unknown): asserts value is PinKey {
  keys(value, ['type'], ['id', 'recipient', 'caller', 'call'])
  if (value.type === 'staging') { keys(value, ['type', 'id']); sequence(value.id) }
  else if (value.type === 'delivery') { keys(value, ['type', 'recipient', 'id']); activation(value.recipient); sequence(value.id) }
  else if (value.type === 'execution') { keys(value, ['type', 'caller', 'call']); activation(value.caller); sequence(value.call) }
  else fail('InvalidParams', 'invalid native pin key')
}
export function root(owner: Activation): Scope { return { activation: owner, scope: '1' } }
export function callSource(owner: Activation, hash: string): string {
  return createHash('sha256').update(signatureCanonical(['protocol-call', owner, hash])).digest('hex')
}
interface Member { owner: Activation; gate: ActivationGate; native?: Context; exports?: Exports; exporters: Map<string, GraphExporter>; services?: StagedServices; published: boolean; upstream: Set<string>; requiredRoots: Set<string>; executing: Set<string>; admitted: Map<string, PinKey>; running: Set<string> }

/** Reserve before plugin construction; bind the actual apply Ctx before it
 * makes required-service calls. Publication follows lifecycle/HostActive. */
export class RuntimeObjects {
  readonly imports = new Imports()
  readonly ids = new ObjectIds()
  private stageIds = new ObjectIds()
  private members = new Map<string, Member>()
  private stages = new Map<string, { owner: Activation; graph: StagedGraph }>()
  private peer?: Peer
  private removeClose?: () => void
  private closed = false
  private controlTail: Promise<void> = Promise.resolve()
  constructor(readonly identity: RuntimeIdentity, readonly bundles: Bundles) {
    keys(identity, ['runtime', 'epoch']); sequence(identity.epoch)
    if (!identifier(identity.runtime)) fail('InvalidParams', 'invalid runtime identity')
    this.identity = Object.freeze(structuredClone(identity))
  }
  private require(owner: Activation): void {
    activation(owner)
    if (owner.runtime !== this.identity.runtime || owner.epoch !== this.identity.epoch) fail('CapabilityDenied', 'activation belongs to another private session')
  }
  reserve(owner: Activation, gate: ActivationGate): void {
    this.require(owner)
    const key = canonical(owner)
    if (this.closed || this.members.has(key) || !gate.isOpen) fail('ScopeClosed', 'closed or reused native member')
    owner = Object.freeze(structuredClone(owner))
    this.imports.openScope(root(owner)); this.imports.bindScope(root(owner), () => gate.isOpen)
    this.members.set(key, { owner, gate, exporters: new Map(), published: false, upstream: new Set(), requiredRoots: new Set(), executing: new Set(), admitted: new Map(), running: new Set() })
    gate.onClose(() => {
      this.closeMember(owner)
      if (this.peer && !this.peer.isClosed) void this.peer.request('object/closing', owner).catch(() => {})
    })
  }
  bind(owner: Activation, native: Context, exports: Exports): void {
    const member = this.member(owner)
    if (!member.gate.isOpen || member.native || canonical(exports.activation) !== canonical(owner)) fail('CapabilityDenied', 'closed, rebound or mismatched native table')
    member.native = native; member.exports = exports
    try {
      exports.onRevoke(async objects => {
        if (this.closed || !member.gate.isOpen) return
        try { await this.connection().request('object/closed', { owner, objects }) }
        catch (error) { if (!this.closed && member.gate.isOpen) throw error }
      })
    } catch (error) { this.closeMember(owner); throw error }
  }
  stageServices(services: StagedServices): ServiceTable {
    const member = this.member(services.table.activation)
    if (!member.gate.isOpen || member.services || member.exports !== services.exports) fail('CapabilityDenied', 'service table belongs to another native member')
    // Every admitted source remains part of the full dispatcher lookup key.
    for (const name of Object.keys(services.table.services)) this.exporter(member.owner, services.contracts[name].bundle_sha256).mergeRegistered(services.exporter(name))
    member.services = services
    return services.table
  }
  publish(owner: Activation): void {
    const member = this.member(owner)
    if (!member.gate.isOpen || !member.native) fail('ScopeClosed', 'native member is not ready')
    member.published = true
  }
  trackImports(owner: Activation, values: Record<string, unknown>): void {
    const member = this.member(owner)
    if (member.native || !member.gate.isOpen) fail('ScopeClosed', 'native imports already mounted')
    const upstream = new Set<string>()
    const requiredRoots = new Set<string>()
    for (const value of Object.values(values)) {
      if (!(value instanceof ObjectProxy)) fail('InterfaceMismatch', 'native import is not an object')
      const delivery = value.delivery()
      if (canonical(delivery.recipient) !== canonical(root(owner))) fail('CapabilityDenied', 'native import belongs to another root')
      upstream.add(canonical(delivery.object.owner))
      requiredRoots.add(canonical(delivery.object))
    }
    member.upstream = upstream
    member.requiredRoots = requiredRoots
  }
  closeMember(owner: Activation): void {
    const member = this.members.get(canonical(owner))
    if (member) { member.published = false; member.gate.close(); for (const [id, key] of member.admitted) if (!member.running.has(id)) member.exports?.release(key); member.exports?.close(); member.services?.abort() }
    for (const [id, stage] of this.stages) if (canonical(stage.owner) === canonical(owner)) { stage.graph.abort(); this.stages.delete(id) }
    this.imports.closeScope(root(owner))
  }
  close(): void {
    this.closed = true
    for (const member of this.members.values()) this.closeMember(member.owner)
    this.removeClose?.(); this.removeClose = undefined
  }
  attach(peer: Peer): void {
    if (this.peer || this.closed) fail('Unavailable', 'runtime cannot rebind its private session')
    this.peer = peer; this.removeClose = peer.onClose(() => this.close())
  }
  handler(fallback: (method: string, value: unknown) => Promise<unknown>): (method: string, value: unknown) => Promise<unknown> {
    return (method, value) => method.startsWith('object/') ? this.handle(method, value) : fallback(method, value)
  }
  lifecycleHandler(runner: Runner): (method: string, value: unknown) => Promise<unknown> {
    return this.handler(async (method, value) => {
      let selected: Activation | undefined
      if (method === 'plugin/activate' || method === 'plugin/stop') {
        keys(value, ['activation']); activation(value.activation); selected = value.activation
      }
      if (method === 'runtime/stop') { keys(value, []); this.close() }
      const result = runner.handle(method, value)
      if (method === 'plugin/stop') this.closeMember(selected!)
      const response = await result
      if (method === 'plugin/activate') this.publish(selected!)
      return response
    })
  }
  private connection(): Peer { if (!this.peer) fail('Unavailable', 'private session unavailable'); return this.peer }
  private member(owner: Activation): Member {
    this.require(owner)
    const member = this.members.get(canonical(owner))
    if (!member) fail('Unavailable', 'unknown native member')
    return member
  }
  private table(owner: Activation): Exports {
    const exports = this.member(owner).exports
    if (!exports) fail('Unavailable', 'native table unavailable')
    return exports
  }
  private exporter(owner: Activation, hash: string): GraphExporter {
    const member = this.member(owner)
    if (!member.gate.isOpen) fail('ScopeClosed', 'native member closed')
    const bundle = this.bundles.exact(hash)
    let exporter = member.exporters.get(hash)
    if (!exporter) { exporter = new GraphExporter(bundle, this.table(owner), member.owner, callSource(owner, hash)); member.exporters.set(hash, exporter) }
    return exporter
  }
  encode(owner: Activation, hash: string, type: TypeExpr, value: Outbound): StageOffer {
    const graph = this.exporter(owner, hash).encode(type, value)
    const stage = this.stageIds.allocate(); this.stages.set(stage, { owner: this.member(owner).owner, graph })
    return { activation: owner, stage, source: callSource(owner, hash), draft: graph.draft }
  }
  private abort(owner: Activation, id: string): void {
    const stage = this.stages.get(id)
    if (stage && canonical(stage.owner) !== canonical(owner)) fail('CapabilityDenied', 'staging graph belongs to another member')
    stage?.graph.abort(); this.stages.delete(id)
  }
  private stageRoute(owner: Activation, name: string, source: string): StageOffer {
    const member = this.member(owner)
    if (!member.gate.isOpen) fail('ScopeClosed', 'route owner closed')
    if (!/^route\.[0-9a-f]{64}$/.test(source)) fail('InvalidParams', 'invalid prepared route source')
    const services = member.services
    if (!services) fail('Unavailable', 'native service table is not staged')
    const graph = services.stageRoute(name, source)
    try {
      this.exporter(owner, services.contracts[name].bundle_sha256).mergeRegistered(services.exporter(name))
      const stage = this.stageIds.allocate(); this.stages.set(stage, { owner: member.owner, graph })
      return { activation: member.owner, stage, source, draft: graph.draft }
    } catch (error) { graph.abort(); throw error }
  }
  flush(): Promise<void> {
    const controls = this.imports.takeControls()
    if (!controls.length) return this.controlTail
    const previous = this.controlTail
    const next = (async () => {
      let prior: unknown; let failed = false
      try { await previous } catch (error) { prior = error; failed = true }
      // Even a failed earlier request must not abandon later Release controls.
      await this.connection().request('object/controls', controls)
      if (failed) throw prior
    })()
    void next.catch(() => {})
    this.controlTail = next; return next
  }
  /** The complete root table is validated and accepted before any native
   * provider or author constructor sees its generated facades. */
  async receiveServices(owner: Activation, contracts: Record<string, CatalogService>, graphs: Record<string, WireGraph>): Promise<Record<string, unknown>> {
    this.member(owner)
    const scopes = { scope: root(owner), borrow: root(owner) }
    const deliveries = Object.values(graphs ?? {}).flatMap(graph => Array.isArray(graph?.references) ? graph.references.flatMap(reference => { try { return [parseDelivery(reference.delivery)] } catch { return [] } }) : [])
    try {
      keys(graphs, Object.keys(contracts))
      for (const [name, graph] of Object.entries(graphs)) validateGraph(this.bundles.service(contracts[name]), { kind: 'object', interface: contracts[name].interface, ownership: 'scope' }, graph, scopes)
      const values = Object.fromEntries(Object.entries(graphs).map(([name, graph]) => [name, this.imports.receiveGraph(this.bundles.service(contracts[name]), { kind: 'object', interface: contracts[name].interface, ownership: 'scope' }, graph, scopes)]))
      this.trackImports(owner, values); await this.flush(); return values
    } catch (error) {
      this.closeMember(owner); this.imports.reject(deliveries)
      try { await this.flush() } catch (cleanup) { throw new AggregateError([error, cleanup], 'native root rollback failed') }
      throw error
    }
  }
  async retire(): Promise<{ received_through: string; terminal_through: string } | null> {
    await this.flush()
    const proposal = this.imports.retirement()
    if (!proposal) return null
    const through = await this.connection().request('object/retire', proposal); sequence(through)
    if (through !== proposal.terminal_through) fail('InvalidParams', 'retirement ACK differs from proposal')
    this.imports.acknowledgeRetirement(through); return proposal
  }
  caller(owner: Activation): Caller {
    this.member(owner)
    return { bindNative: (ctx, value) => registerNative(this.table(owner), ctx, value), call: async (target: ObjectProxy, method: string, params: Outbound) => {
      const delivery = target.delivery()
      if (canonical(delivery.recipient.activation) !== canonical(owner)) fail('CapabilityDenied', 'client belongs to another native member')
      const contract = this.bundles.method(delivery.view, method)
      const input = this.encode(owner, delivery.view.bundle_sha256, contract.params, params)
      try {
        const graph = await this.connection().request('object/call', { target: delivery, method, input }) as WireGraph
        try { return this.imports.receiveGraph(this.bundles.exact(delivery.view.bundle_sha256), contract.result, graph, { scope: target.scope, borrow: target.scope }) }
        finally { await this.flush() }
      } finally { this.abort(owner, input.stage) }
    } }
  }
  async handle(method: string, input: unknown): Promise<unknown> {
    const value = input as any
    try { switch (method) {
      case 'object/open': scope(value); this.require(value.activation); this.imports.openScope(value, root(value.activation)); return null
      case 'object/pin': {
        keys(value, ['object', 'key']); keys(value.object, ['owner', 'object']); this.require(value.object.owner); sequence(value.object.object); pin(value.key)
        this.table(value.object.owner).pin(value.object as ObjectIdentity, value.key); if (value.key.type === 'execution') this.member(value.object.owner).admitted.set(canonical(value.key), structuredClone(value.key)); return null
      }
      case 'object/release': keys(value, ['activation', 'key']); pin(value.key); this.table(value.activation).release(value.key); return null
      case 'object/abort': keys(value, ['activation', 'stage']); activation(value.activation); sequence(value.stage); this.abort(value.activation, value.stage); return null
      case 'object/commit': {
        keys(value, ['activation', 'stage', 'deliveries', 'scopes']); activation(value.activation); sequence(value.stage); scopes(value.scopes)
        if (!Array.isArray(value.deliveries)) fail('InvalidParams', 'invalid commit deliveries')
        const stage = this.stages.get(value.stage)
        if (!stage) fail('StaleObject', 'unknown staging graph')
        if (canonical(stage.owner) !== canonical(value.activation)) fail('CapabilityDenied', 'staging graph belongs to another member')
        stage.graph.commit(value.deliveries.map(parseDelivery), value.scopes); return null
      }
      case 'object/services-commit': {
        keys(value, ['activation', 'graphs', 'scopes']); scopes(value.scopes)
        const services = this.member(value.activation).services
        if (!services) fail('StaleObject', 'no staged service table')
        services.commit(value.graphs, value.scopes); return null
      }
      case 'object/route': {
        keys(value, ['activation', 'service', 'source'])
        if (typeof value.service !== 'string' || !identifier(value.service) || typeof value.source !== 'string') fail('InvalidParams', 'invalid route stage request')
        return this.stageRoute(value.activation, value.service, value.source)
      }
      case 'object/reject': {
        if (!Array.isArray(value)) fail('InvalidParams', 'invalid rejected deliveries')
        this.imports.reject(value.map(parseDelivery)); await this.flush(); return []
      }
      case 'object/revoke-objects': {
        if (!Array.isArray(value)) fail('InvalidParams', 'invalid object revocation')
        for (const object of value) { keys(object, ['owner', 'object']); activation(object.owner); sequence(object.object) }
        this.imports.revokeObjects(value)
        const closed = new Set(value.map(object => canonical(object)))
        for (const member of this.members.values()) if ([...member.requiredRoots].some(object => closed.has(object))) this.closeMember(member.owner)
        await this.flush(); return []
      }
      case 'object/revoke': { activation(value); this.imports.revokeOwner(value); this.closeMember(value); for (const member of this.members.values()) if (member.upstream.has(canonical(value))) this.closeMember(member.owner); await this.flush(); return [] }
      case 'object/retire-owner': {
        keys(value, ['runtime', 'through']); keys(value.runtime, ['runtime', 'epoch']); sequence(value.runtime.epoch); sequence(value.through)
        if (typeof value.runtime.runtime !== 'string') fail('InvalidParams', 'invalid retired runtime')
        for (const member of this.members.values()) member.exports?.retireDeliveries(value.runtime.runtime, value.runtime.epoch, value.through)
        return null
      }
      case 'object/end': {
        keys(value, ['activation', 'scope', 'key', 'stage']); this.require(value.activation); scope(value.scope); pin(value.key)
        if (canonical(value.scope.activation) !== canonical(value.activation) || value.scope.scope === '1') fail('CapabilityDenied', 'invalid execution scope')
        this.imports.closeScope(value.scope); this.table(value.activation).release(value.key); this.member(value.activation).executing.delete(canonical(value.key)); this.member(value.activation).admitted.delete(canonical(value.key)); this.member(value.activation).running.delete(canonical(value.key))
        if (value.stage !== null) { sequence(value.stage); this.abort(value.activation, value.stage) }
        await this.flush(); return []
      }
      case 'object/execute': {
        keys(value, ['object', 'view', 'method', 'key', 'graph', 'scopes']); keys(value.object, ['owner', 'object']); this.require(value.object.owner); sequence(value.object.object)
        keys(value.view, ['interface', 'bundle_sha256', 'source']); pin(value.key); scopes(value.scopes)
        if (typeof value.method !== 'string' || Object.values(value.view).some(v => typeof v !== 'string')) fail('InvalidParams', 'invalid dispatch selector')
        if (canonical(value.scopes.scope) !== canonical(root(value.object.owner)) || canonical(value.scopes.borrow.activation) !== canonical(value.object.owner) || value.scopes.borrow.scope === '1') fail('CapabilityDenied', 'invalid dispatch scopes')
        const member = this.member(value.object.owner)
        if ((!member.published && !value.view.interface.startsWith('$callback:')) || !member.gate.isOpen || !member.native) fail('Unavailable', 'native member has not been published')
        const contract = this.bundles.method(value.view as InterfaceView, value.method)
        const execution = canonical(value.key)
        if (value.key.type !== 'execution' || member.executing.has(execution)) fail('InvalidParams', 'duplicate or invalid native execution')
        const creator = member.exports!.executionContext(value.key)
        member.executing.add(execution); member.running.add(execution)
        const context = new CallContext(this.caller(member.owner), creator?.context ?? member.native, () => creator ? creator.isOpen() : member.gate.isOpen)
        let result: Outbound | undefined; let error: unknown; let failed = false
        try {
          const params = this.imports.receiveGraph(this.bundles.exact(value.view.bundle_sha256), contract.params, value.graph, value.scopes)
          await this.flush() // ACK before exposing reentrant parameters.
          result = await this.exporter(member.owner, value.view.bundle_sha256).dispatcher(value.object as ObjectIdentity, value.view as InterfaceView).dispatch(value.key, context, value.method, params)
        } catch (caught) { error = caught; failed = true }
        try { await context.finish() } catch (caught) { if (!failed) error = caught; failed = true }
        member.running.delete(execution)
        if (this.closed || !member.gate.isOpen) { member.exports!.release(value.key); member.admitted.delete(execution); this.imports.closeScope(value.scopes.borrow) }
        if (failed) throw error instanceof ProtocolError ? new ProtocolError(error.code, error.stage, error.message, 'unknown') : new ProtocolError('Business', 'handler', String(error), 'unknown')
        try { return this.encode(member.owner, value.view.bundle_sha256, contract.result, context.outbound(result!)) }
        catch (error) { throw error instanceof ProtocolError ? new ProtocolError(error.code, error.stage, error.message, 'unknown') : new ProtocolError('Business', 'result', String(error), 'unknown') }
      }
      default: fail('UnsupportedCapability', 'unknown runtime object operation')
    } } catch (error) {
      if (method === 'object/execute') {
        const deliveries = Array.isArray(value?.graph?.references) ? value.graph.references.flatMap((reference: any) => {
          try { const delivery = parseDelivery(reference.delivery); return delivery.recipient.activation.runtime === this.identity.runtime && delivery.recipient.activation.epoch === this.identity.epoch ? [delivery] : [] } catch { return [] }
        }) : []
        this.imports.reject(deliveries); await this.flush()
      }
      throw error
    }
  }
}

/** A service-capable Driver owns reservation/binding before returning its
 * staged table. Root cleanup and independent process reaping stay with caller. */
export async function serve(stream: Duplex, runner: Runner, objects: RuntimeObjects): Promise<void> {
  const peer = new Peer(stream, objects.lifecycleHandler(runner))
  objects.attach(peer); runner.attach(peer)
  await peer.closed; objects.close()
  const results = await Promise.allSettled(runner.close())
  const errors = results.flatMap(result => result.status === 'rejected' ? [result.reason] : [])
  if (errors.length) throw new AggregateError(errors, 'native runtime cleanup failed')
}
