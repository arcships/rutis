import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { isAbsolute } from 'node:path'
import { pathToFileURL } from 'node:url'
import type { Duplex } from 'node:stream'
import { Context, type Plugin } from '@deepseek-ai/cordis'
import { canonical, identifier, keys, validateJson, type ErrorCode } from './contract.ts'
import { ProtocolError } from './error.ts'
import { decodeJson } from './json.ts'
import { Peer } from './frame.ts'
import { sequence, type Activation } from './imports.ts'
import { ActivationGate, ManagedActivation } from './managed.ts'
import { Bundles, NativePorts } from './services.ts'
import { ObjectSession } from './runtime.ts'
import { Exports } from './exports.ts'
import type { WireGraph } from './graph.ts'

export const FAMILY = 'rutis-cordis-objects'
export const VERSION = '0.experimental'
export const CORDIS_VERSION = '4.0.1'
export interface RuntimeIdentity {
  runtime: string; epoch: string; kind: 'rust-rutis' | 'node-cordis'
  framework_version: string; environment_sha256: string; code_sha256: string; capabilities: string[]
}
export interface CatalogService { interface: string; version: string; bundle_sha256: string }
export interface Contracts {
  config_sha256: string; provides: Record<string, CatalogService>; requires: Record<string, CatalogService>
}
export interface Member {
  entry: { kind: 'node'; entry: string } | { kind: 'rust'; factory: string }
  config: unknown; config_schema: string; contracts: Contracts
}
export interface Hello {
  protocol_family: string; protocol_version: string; identity: RuntimeIdentity; members: Record<string, Member>
}
export type Phase = 'starting' | 'staged' | 'published' | 'closing' | 'stopped' | 'failed'
export interface Status { instance: string; activation: Activation; phase: Phase }
export interface Mounted { native: ManagedActivation<any>; services(): Promise<Record<string, unknown>> }
export interface MountRequest { instance: string; activation: Activation; member: Member; required: Record<string, WireGraph> }
export interface Driver {
  readonly objectSession?: ObjectSession
  /** Declaration checks only: no module import, factory construction or apply. */
  admit(plan: Readonly<Hello>): void
  mount(request: MountRequest, admission: ActivationGate): Mounted | Promise<Mounted>
}
const failure = (code: ErrorCode, message: string) => new ProtocolError(code, 'lifecycle', message)
function invalid(message: string): never { throw failure('InvalidParams', message) }
function unavailable(message: string): never { throw failure('Unavailable', message) }
const digest = (bytes: string) => createHash('sha256').update(bytes).digest('hex')
const version = (value: unknown) => typeof value === 'string' && /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(value)
const sha = (value: unknown) => typeof value === 'string' && /^[0-9a-f]{64}$/.test(value)
function freeze<T>(value: T): T {
  if (value && typeof value === 'object') {
    for (const child of Object.values(value)) freeze(child)
    Object.freeze(value)
  }
  return value
}
function identity(value: unknown): asserts value is RuntimeIdentity {
  keys(value, ['runtime', 'epoch', 'kind', 'framework_version', 'environment_sha256', 'code_sha256', 'capabilities'])
  sequence(value.epoch)
  if (typeof value.runtime !== 'string' || !identifier(value.runtime) || !['rust-rutis', 'node-cordis'].includes(value.kind)
    || !version(value.framework_version) || !sha(value.environment_sha256) || !sha(value.code_sha256)) invalid('invalid runtime identity')
  if (!Array.isArray(value.capabilities) || new Set(value.capabilities).size !== value.capabilities.length) invalid('capabilities must be a unique list')
  for (const capability of value.capabilities) {
    if (!['object.scope', 'callback.borrow', 'event.parallel', 'event.serial'].includes(capability)) throw failure('UnsupportedCapability', 'unknown runtime capability')
  }
}
function contracts(value: unknown): asserts value is Contracts {
  keys(value, ['config_sha256', 'provides', 'requires'])
  if (!sha(value.config_sha256)) invalid('invalid config schema digest')
  for (const map of [value.provides, value.requires]) {
    keys(map, Object.keys(map ?? {}))
    for (const [name, service] of Object.entries(map)) {
      if (!identifier(name)) invalid('invalid service name')
      keys(service, ['interface', 'version', 'bundle_sha256'])
      if (typeof service.interface !== 'string' || !identifier(service.interface) || !version(service.version) || !sha(service.bundle_sha256)) invalid('invalid service contract')
    }
  }
}
/** Take a private immutable copy; callers cannot change a retained plan after
 * hello, and native code receives a separate copy of its own configuration. */
export function parseHello(value: unknown): Hello {
  validateJson({}, value)
  const copy = decodeJson(Buffer.from(canonical(value)))
  keys(copy, ['protocol_family', 'protocol_version', 'identity', 'members'])
  if (copy.protocol_family !== FAMILY || copy.protocol_version !== VERSION) throw failure('InterfaceMismatch', 'protocol identity differs')
  identity(copy.identity)
  keys(copy.members, Object.keys(copy.members ?? {}))
  if (!Object.keys(copy.members).length) invalid('members are required')
  for (const [name, member] of Object.entries(copy.members)) {
    if (!identifier(name)) invalid('invalid member name')
    keys(member, ['entry', 'config', 'config_schema', 'contracts'])
    keys(member.entry, ['kind'], ['entry', 'factory'])
    if (member.entry.kind === 'node') {
      keys(member.entry, ['kind', 'entry'])
      if (typeof member.entry.entry !== 'string' || !isAbsolute(member.entry.entry)) invalid('Node entry must be an absolute snapshot path')
    } else if (member.entry.kind === 'rust') {
      keys(member.entry, ['kind', 'factory'])
      if (typeof member.entry.factory !== 'string' || !identifier(member.entry.factory)) invalid('invalid native factory')
    } else invalid('unknown member entry')
    contracts(member.contracts)
    if (typeof member.config_schema !== 'string') invalid('config schema must retain raw UTF-8 JSON')
    if (digest(member.config_schema) !== member.contracts.config_sha256) throw failure('InterfaceMismatch', 'config schema raw digest differs')
    const schema = decodeJson(Buffer.from(member.config_schema))
    keys(schema, Object.keys(schema ?? {}))
    validateJson(schema, member.config)
  }
  return freeze(copy as unknown as Hello)
}
function activation(value: unknown): Activation {
  keys(value, ['runtime', 'epoch', 'activation'])
  if (typeof value.runtime !== 'string' || !identifier(value.runtime)) invalid('invalid activation runtime')
  sequence(value.epoch); sequence(value.activation)
  return freeze(structuredClone(value) as Activation)
}
function deferred<T>() {
  let resolve!: (value: T) => void; let reject!: (reason: unknown) => void
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no })
  // An abandoned request never owns the lifecycle task. The owner joins this
  // promise at stop/disconnect even if the original waiter has gone away.
  void promise.catch(() => {})
  return { promise, resolve, reject }
}
interface Slot {
  status: Status; admission: ActivationGate; native?: ManagedActivation<any>
  completion: ReturnType<typeof deferred<unknown>>; stop?: Promise<Status>; removeObserver?: () => void
}

/** Protocol intent around real native fibers. No business await occurs in a
 * management critical section (the synchronous Node event-loop stack). */
export class Runner {
  private plan?: Hello
  private closed = false
  private slots = new Map<string, Slot>()
  private current = new Map<string, Slot>()
  private removeClose?: () => void
  constructor(private driver: Driver) {}
  readonly handle = (method: string, params: unknown): Promise<unknown> => {
    try {
      switch (method) {
        case 'runtime/hello': return Promise.resolve(this.hello(params))
        case 'plugin/start': return this.start(params)
        case 'plugin/activate': return Promise.resolve(this.activate(params))
        case 'plugin/stop': return this.stop(this.select(params))
        case 'plugin/state': return Promise.resolve(structuredClone(this.select(params).status))
        case 'runtime/stop': {
          keys(params, [])
          return this.join(this.close()).then(() => ({ stopped: true }))
        }
        default: throw failure('UnsupportedCapability', 'unknown lifecycle method')
      }
    } catch (error) { return Promise.reject(error) }
  }
  private hello(params: unknown): unknown {
    if (this.closed || this.plan) unavailable('runtime hello cannot be reused')
    const plan = parseHello(params)
    this.driver.admit(plan)
    this.plan = plan
    return { protocol_family: FAMILY, protocol_version: VERSION, identity: plan.identity }
  }
  private start(params: unknown): Promise<unknown> {
    validateJson({}, params)
    keys(params, ['instance', 'activation'], ['required'])
    if (typeof params.instance !== 'string' || !identifier(params.instance)) invalid('invalid member name')
    const id = activation(params.activation)
    const plan = this.plan
    if (this.closed || !plan) unavailable('runtime is not ready')
    if (id.runtime !== plan.identity.runtime || id.epoch !== plan.identity.epoch) unavailable('activation belongs to another runtime epoch')
    if (!Object.hasOwn(plan.members, params.instance)) invalid('unknown member')
    const key = canonical(id)
    if (this.slots.has(key)) unavailable('activation identity cannot be reused')
    const current = this.current.get(params.instance)
    if (current && (BigInt(id.activation) <= BigInt(current.status.activation.activation) || current.status.phase !== 'stopped')) unavailable('previous activation has not stopped or new id is stale')
    const required = structuredClone(params.required === undefined ? {} : params.required) as Record<string, WireGraph>
    const slot: Slot = {
      status: { instance: params.instance, activation: id, phase: 'starting' },
      admission: new ActivationGate(), completion: deferred(),
    }
    this.slots.set(key, slot); this.current.set(params.instance, slot)
    // Reserve before even invoking an async module loader. Stop can revoke the
    // admission object during that loader and prevent the eventual native apply.
    void Promise.resolve().then(() => this.mount(slot, structuredClone(plan.members[params.instance]), required)).then(slot.completion.resolve, slot.completion.reject)
    return slot.completion.promise
  }
  private async mount(slot: Slot, member: Member, required: Record<string, WireGraph>): Promise<unknown> {
    const expectedServices = Object.keys(member.contracts.provides).sort()
    try {
      if (!slot.admission.isOpen) throw failure('Cancelled', 'activation stopped before construction')
      const mounted = await this.driver.mount({ instance: slot.status.instance, activation: slot.status.activation, member, required }, slot.admission)
      slot.native = mounted.native
      if (mounted.native.gate !== slot.admission) {
        await mounted.native.stop()
        throw failure('Unavailable', 'driver did not preserve activation admission')
      }
      slot.removeObserver = slot.admission.onClose(() => { void this.stop(slot).catch(() => {}) })
      if (!slot.admission.isOpen) throw failure('Cancelled', 'activation stopped during construction')
      try { await mounted.native.ready() }
      catch (error) {
        if (slot.status.phase === 'closing') throw failure('Cancelled', 'activation stopped before native readiness')
        throw error
      }
      if (!slot.admission.isOpen) throw failure('Cancelled', 'activation stopped before staging')
      const services = await Promise.race([
        mounted.services(),
        slot.admission.revoked.then(() => { throw failure('Cancelled', 'activation revoked during staging') }),
      ])
      validateJson({}, services)
      keys(services, Object.keys(services ?? {}))
      if (canonical(Object.keys(services).sort()) !== canonical(expectedServices)) throw failure('InterfaceMismatch', 'staged service names differ from declaration')
      if (!slot.admission.isOpen || slot.status.phase === 'closing') throw failure('Cancelled', 'activation stopped during staging')
      slot.status.phase = 'staged'
      return { activation: slot.status.activation, services }
    } catch (error) {
      slot.admission.close()
      try { await slot.native?.stop() }
      catch (cleanup) { error = cleanup }
      if (slot.status.phase !== 'closing') slot.status.phase = 'failed'
      throw error instanceof ProtocolError ? error : new ProtocolError('Business', 'start', String(error), 'unknown')
    }
  }
  private select(params: unknown): Slot {
    keys(params, ['activation'])
    const slot = this.slots.get(canonical(activation(params.activation)))
    if (!slot) unavailable('unknown activation')
    return slot
  }
  private activate(params: unknown): Status {
    const slot = this.select(params)
    if (this.closed || !['staged', 'published'].includes(slot.status.phase) || !slot.admission.isOpen || !slot.native?.isOpen) unavailable('activation is not staged')
    slot.status.phase = 'published'
    return structuredClone(slot.status)
  }
  requirePublished(id: Activation): void {
    const slot = this.slots.get(canonical(id))
    if (this.closed || slot?.status.phase !== 'published' || !slot.admission.isOpen || !slot.native?.isOpen) unavailable('activation has not been published or was revoked')
  }
  private stop(slot: Slot): Promise<Status> {
    if (slot.stop) return slot.stop
    // Publish the cached cleanup before invoking any native revocation hook.
    const stopped = deferred<Status>(); slot.stop = stopped.promise
    slot.status.phase = 'closing'
    slot.admission.close()
    if (slot.native) void slot.native.stop().catch(() => {})
    void (async () => {
      await slot.completion.promise.catch(() => {})
      await slot.native?.stop()
      slot.removeObserver?.(); slot.removeObserver = undefined
      slot.native = undefined
      slot.status.phase = 'stopped'
      return structuredClone(slot.status)
    })().then(stopped.resolve, error => stopped.reject(error instanceof ProtocolError ? error : new ProtocolError('Business', 'stop', String(error))))
    return stopped.promise
  }
  /** Closing publication and all native gates is synchronous; cleanup tasks
   * keep running independently of transport waiters or the process supervisor. */
  close(): Promise<Status>[] {
    this.closed = true
    return [...this.slots.values()].map(slot => this.stop(slot))
  }
  attach(peer: Peer): void {
    if (this.removeClose) unavailable('runtime cannot rebind its private peer')
    this.removeClose = peer.onClose(() => { void this.join(this.close()).catch(() => {}) })
  }
  private async join(tasks: Promise<unknown>[]): Promise<void> {
    const results = await Promise.allSettled(tasks)
    const errors = results.filter((result): result is PromiseRejectedResult => result.status === 'rejected')
    if (errors.length) throw new ProtocolError('Business', 'runtime_stop', errors.map(result => String(result.reason)).join('; '))
  }
  async stopped(): Promise<void> { await this.join(this.close()) }
}

export interface NodeModule {
  entry: string; contracts: Contracts
  /** Retained without invocation until start. Default loader uses the frozen
   * absolute snapshot entry, never a path supplied by business code. */
  load(): Promise<{ default: Plugin.Object<any>; protocolPorts?: NativePorts }>
}
export function nodeModule(entry: string, declaration: Contracts): NodeModule {
  if (!isAbsolute(entry)) invalid('Node module must use an absolute snapshot entry')
  contracts(declaration)
  return { entry, contracts: freeze(structuredClone(declaration)), load: () => import(pathToFileURL(entry).href) }
}
/** Optional service transport accepts roots before module import. Successful
 * hello checks pure declarations and never imports business code. */
export class NativeModuleDriver implements Driver {
  private modules = new Map<string, NodeModule>()
  readonly objectSession?: ObjectSession
  constructor(private parent: Context, private environmentSha256: string, private codeSha256: string, modules: Iterable<NodeModule>, private bundles?: Bundles) {
    if (bundles) this.objectSession = new ObjectSession(bundles)
    if (!sha(environmentSha256) || !sha(codeSha256)) invalid('invalid code/environment digest')
    const framework = decodeJson(readFileSync(new URL(import.meta.resolve('@deepseek-ai/cordis/package.json'))))
    keys(framework, Object.keys(framework ?? {}))
    if (framework.name !== '@deepseek-ai/cordis' || framework.version !== CORDIS_VERSION) throw failure('InterfaceMismatch', 'loaded Cordis package differs from pinned native adapter')
    for (const module of modules) {
      if (!isAbsolute(module.entry) || this.modules.has(module.entry) || typeof module.load !== 'function') invalid('invalid or duplicate module entry/loader')
      contracts(module.contracts)
      this.modules.set(module.entry, { entry: module.entry, load: module.load, contracts: freeze(structuredClone(module.contracts)) })
    }
  }
  admit(plan: Hello): void {
    if (plan.identity.kind !== 'node-cordis' || plan.identity.framework_version !== CORDIS_VERSION || plan.identity.environment_sha256 !== this.environmentSha256 || plan.identity.code_sha256 !== this.codeSha256) throw failure('InterfaceMismatch', 'native runner identity differs')
    if (plan.identity.capabilities.some(capability => !this.objectSession || !['object.scope', 'callback.borrow'].includes(capability))) throw failure('UnsupportedCapability', 'native driver does not implement requested capability')
    for (const member of Object.values(plan.members)) {
      const module = member.entry.kind === 'node' ? this.modules.get(member.entry.entry) : undefined
      if (!module || canonical(module.contracts) !== canonical(member.contracts)) throw failure('InterfaceMismatch', 'member differs from declared Node module')
      if (!this.bundles && (Object.keys(member.contracts.provides).length || Object.keys(member.contracts.requires).length)) throw failure('UnsupportedCapability', 'native lifecycle driver has no protocol service codecs')
      for (const contract of [...Object.values(member.contracts.provides), ...Object.values(member.contracts.requires)]) this.bundles!.service(contract)
    }
    this.objectSession?.admit(plan)
  }
  async mount({ member, activation, required }: MountRequest, admission: ActivationGate): Promise<Mounted> {
    const module = member.entry.kind === 'node' ? this.modules.get(member.entry.entry) : undefined
    if (!module) unavailable('undeclared Node module')
    if (!this.objectSession && Object.keys(required).length) invalid('service-less driver received required roots')
    const objects = this.objectSession?.objects()
    objects?.reserve(activation, admission)
    try {
      const values = objects ? await objects.receiveServices(activation, member.contracts.requires, required) : {}
      if (!admission.isOpen) throw failure('Cancelled', 'activation stopped before module import')
      const loaded = await module.load()
      if (!admission.isOpen) throw failure('Cancelled', 'activation stopped during module import')
      const plugin = loaded.default
      if (!plugin || typeof plugin !== 'object' || typeof plugin.apply !== 'function') invalid('module default must be a native Cordis plugin object')
      if (objects) {
        const ports = loaded.protocolPorts ?? new NativePorts()
        if (!(ports instanceof NativePorts)) throw failure('InterfaceMismatch', 'module ports must use the canonical protocol SDK')
        ports.check(member.contracts)
        let table: Exports | undefined
        const wrapped: Plugin.Object<any> = { ...plugin, apply(ctx, config) {
          table = Exports.managed(ctx, activation, objects.ids, () => admission.isOpen)
          objects.bind(activation, ctx, table)
          return plugin.apply.call(plugin, ctx, config)
        } }
        const services = await ports.mount(this.parent, wrapped, member.config, activation, values, objects.caller(activation), admission)
        return { native: services.native, services: async () => {
          if (!table) unavailable('original native context has not entered apply')
          return objects.stageServices(await services.stage(this.bundles!, objects.ids, table)).services
        } }
      }
      if (Object.keys(plugin.inject ?? {}).length) throw failure('InterfaceMismatch', 'module requests undeclared native dependencies')
      const native = await ManagedActivation.mount(this.parent, plugin, member.config, {}, undefined, [], admission)
      return { native, services: () => Promise.resolve({}) }
    } catch (error) {
      if (objects) {
        objects.closeMember(activation)
        try { await objects.flush() } catch (cleanup) { throw new AggregateError([error, cleanup], 'native mount rollback failed') }
      }
      throw error
    }
  }
}

export interface NodeCatalog {
  protocol_family: string; protocol_version: string; framework_version: string
  environment_sha256: string; code_sha256: string; modules: Record<string, Contracts>
  bundles?: Record<string, string>
}
/** This launch file is materialized by Host from its frozen prepared group. It
 * has no instance configurations and is read before any business code import. */
export function parseNodeCatalog(value: unknown): NodeCatalog {
  validateJson({}, value)
  const copy = decodeJson(Buffer.from(canonical(value)))
  keys(copy, ['protocol_family', 'protocol_version', 'framework_version', 'environment_sha256', 'code_sha256', 'modules'], ['bundles'])
  if (copy.protocol_family !== FAMILY || copy.protocol_version !== VERSION || copy.framework_version !== CORDIS_VERSION) throw failure('InterfaceMismatch', 'Node launch protocol/framework differs')
  if (!sha(copy.environment_sha256) || !sha(copy.code_sha256)) invalid('invalid Node launch digest')
  keys(copy.modules, Object.keys(copy.modules ?? {}))
  if (!Object.keys(copy.modules).length) invalid('Node launch modules are required')
  for (const [entry, declaration] of Object.entries(copy.modules)) {
    if (!isAbsolute(entry)) invalid('Node launch entry must be an absolute snapshot path')
    contracts(declaration)
  }
  if (copy.bundles !== undefined) {
    keys(copy.bundles, Object.keys(copy.bundles ?? {}))
    for (const [hash, raw] of Object.entries(copy.bundles)) if (!sha(hash) || typeof raw !== 'string' || digest(raw) !== hash) throw failure('InterfaceMismatch', 'frozen bundle raw digest differs')
  }
  return freeze(copy as unknown as NodeCatalog)
}

/** Embedding owns the native root. A runtime/stop ACK is followed by Host peer
 * close and process reaping; it is not itself an OS shutdown confirmation. */
export async function serve(stream: Duplex, driver: Driver): Promise<void> {
  const runner = new Runner(driver)
  const peer = new Peer(stream, driver.objectSession?.handler(runner) ?? runner.handle)
  driver.objectSession?.attach(peer)
  runner.attach(peer)
  await peer.closed
  await runner.stopped()
}
