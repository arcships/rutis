// Conformance runtime over inherited private fd 3. This is an object protocol
// fixture, not the production manifest loader or the required legacy migration.
import { Socket } from 'node:net'
import { readFileSync } from 'node:fs'
import { Context } from '@deepseek-ai/cordis'
import { ManagedActivation } from '../../src/managed.ts'
import { admit, keys, type Method, type TypeExpr } from '../../src/contract.ts'
import { Imports, sequence, type Scope, type Delivery } from '../../src/imports.ts'
import { Exports, ObjectIds, type PinKey } from '../../src/exports.ts'
import { GraphExporter, type StagedGraph } from '../../src/draft.ts'
import { Peer } from '../../src/frame.ts'
import { CallContext, type Caller } from '../../src/sdk.ts'
import { ProtocolError } from '../../src/error.ts'
import { bindInterfaceDatabase, exportInterfaceDatabase, type InterfaceDatabase, type InterfaceDatabaseService, type InterfaceConnectionService, type InterfaceSessionService, type InterfaceAgentService } from '../../generated/rpc.ts'
import type { GraphScopes, WireGraph } from '../../src/graph.ts'

const admitted = admit(readFileSync(new URL('../../../fixtures/rpc.bundle.json', import.meta.url)))
const owner = { runtime: 'node', epoch: '1', activation: '1' }
const scope: Scope = { activation: owner, scope: '1' }
const imports = new Imports(); imports.openScope(scope)
const ids = new ObjectIds(); const stages = new Map<string, StagedGraph>(); const executions = new Map<string, { key: PinKey; scope: Scope }>()
let table!: Exports; let exporter!: GraphExporter; let native!: Context; let managed: ManagedActivation | undefined; let rustDatabase!: InterfaceDatabase
let database!: InterfaceDatabaseService
let borrowedCallback: Parameters<InterfaceDatabaseService['withCallback']>[1] | undefined
let peer!: Peer
const marker = { name: 'node-native' }
let slowEntered = false; let resumeSlow: (() => void) | undefined
function methodContract(iface: string, method: string): Method {
  if (iface.startsWith('$callback:')) {
    const callback = admitted.bundle.interfaces.Database.methods.withCallback.params
    if (callback.kind !== 'callback' || method !== 'call') throw new ProtocolError('CapabilityDenied', 'call', 'invalid callback selector')
    return { params: callback.params, result: callback.result }
  }
  const contract = admitted.bundle.interfaces[iface]?.methods[method]
  if (!contract) throw new ProtocolError('CapabilityDenied', 'call', 'unknown selector')
  return contract
}
const caller: Caller = { async call(target, method, value) {
  const delivery = target.delivery()
  const contract = methodContract(delivery.view.interface, method)
  const staged = exporter.encode(contract.params, value); const stage = ids.allocate(); stages.set(stage, staged)
  try {
    const response: any = await peer.request('call', { target: delivery, method, stage, draft: staged.draft })
    if (response.error) throw new ProtocolError(response.error.code, response.error.stage, response.error.message, response.error.execution)
    try { return imports.receiveGraph(admitted, contract.result, response.graph, { scope: target.scope, borrow: target.scope }) }
    finally { await peer.request('controls', { controls: imports.takeControls() }) }
  } finally { staged.abort(); stages.delete(stage) }
} }
function stage(type: TypeExpr, value: Parameters<GraphExporter['encode']>[1]): { stage: string; draft: StagedGraph['draft'] } {
  const staged = exporter.encode(type, value); const id = ids.allocate(); stages.set(id, staged); return { stage: id, draft: staged.draft }
}
function error(error: unknown) {
  const value = error instanceof ProtocolError ? error : new ProtocolError('Business', 'handler', String(error), 'unknown')
  return { code: value.code, stage: value.stage, message: value.message, execution: 'unknown' }
}
async function handler(method: string, input: unknown): Promise<unknown> {
  const value = input as any
  switch (method) {
    case 'hello': return { family: 'rutis-cordis-objects', version: '0.experimental', bundle: admitted.sha256, capabilities: ['object.scope', 'callback.borrow'] }
    case 'attach': {
      keys(value, ['graph'])
      rustDatabase = bindInterfaceDatabase(imports.receiveGraph(admitted, { kind: 'object', interface: 'Database', ownership: 'scope' }, value.graph, { scope, borrow: scope }) as any, caller)
      return { controls: imports.takeControls() }
    }
    case 'activate': {
      if (managed) throw new ProtocolError('StaleObject', 'start', 'activation cannot be reused')
      managed = new ManagedActivation(new Context(), { inject: ['marker', 'rustDatabase'], apply(ctx) {
        native = ctx
        table = Exports.managed(ctx, owner, ids, () => managed?.isOpen ?? true)
        imports.bindScope(scope, () => ctx.fiber.uid !== null && (managed?.isOpen ?? true))
        ctx.effect(() => () => imports.closeScope(scope))
        exporter = new GraphExporter(admitted, table, owner, admitted.bundle.id)
        let session: InterfaceSessionService
        const agent: InterfaceAgentService = { get session() { return session } }; session = { agent }
        let count = 0
        const connection: InterfaceConnectionService = { session, async query(context, params) {
          if (context.native() !== ctx || context.native().get('marker') !== marker) throw new Error('wrong native context')
          if (params.sql === 'slow') { slowEntered = true; await new Promise<void>(resolve => { resumeSlow = resolve }) }
          return [{ count: ++count, sql: params.sql, owner: 'node' }]
        } }
        database = {
          async connect(context, params) { if (context.native() !== ctx || !params.name) throw new Error('wrong connect context'); return connection },
          async inspect(_, client) { const rows = await client.query({ sql: 'passback-node' }); return rows[0].owner === 'node' },
          async withCallback(context, callback) { borrowedCallback = callback; await callback.call('node-callback'); context.spawn(async () => { await callback.call('node-child') }); return null },
        }
      } }, {}, { marker, rustDatabase })
      // Marker is a service object; compare the injected object by identity.
      await managed.ready()
      return stage({ kind: 'object', interface: 'Database', ownership: 'scope' }, exportInterfaceDatabase(database))
    }
    case 'open': imports.openScope(value.scope, scope); return null
    case 'slow-status': return slowEntered
    case 'resume': resumeSlow?.(); return null
    case 'end': {
      imports.closeScope(value.scope); table.release(value.key)
      if (value.stage) { executions.delete(value.stage); stages.get(value.stage)?.abort(); stages.delete(value.stage) }
      return { controls: imports.takeControls() }
    }
    case 'pin': table.pin(value.object, value.key); return null
    case 'release': table.release(value.key); return null
    case 'reject': imports.reject(value.deliveries); return { controls: imports.takeControls() }
    case 'abort': stages.get(value.stage)?.abort(); stages.delete(value.stage); return null
    case 'expired': {
      if (!borrowedCallback) throw new Error('callback was not captured')
      await borrowedCallback.call('expired')
      throw new Error('expired callback was incorrectly callable')
    }
    case 'retire-owner': table.retireDeliveries(value.runtime, value.epoch, value.through); return null
    case 'retire': {
      await peer.request('controls', { controls: imports.takeControls() })
      const proposal = imports.retirement()
      if (!proposal) throw new ProtocolError('InvalidParams', 'retire', 'no terminal prefix')
      const response: any = await peer.request('retirement', proposal)
      keys(response, ['terminal_through']); sequence(response.terminal_through)
      if (response.terminal_through !== proposal.terminal_through) throw new ProtocolError('InvalidParams', 'retire', 'ACK differs from proposed prefix')
      imports.acknowledgeRetirement(response.terminal_through)
      return proposal
    }
    case 'commit': {
      const staged = stages.get(value.stage)
      if (!staged) throw new ProtocolError('StaleObject', 'commit', 'unknown staging graph')
      staged.commit(value.deliveries as Delivery[], value.scopes as GraphScopes); stages.delete(value.stage)
      return null
    }
    case 'execute': {
      const contract = methodContract(value.view.interface, value.method)
      const paramsType = contract.params; const resultType = contract.result
      const key = value.key as PinKey
      const context = new CallContext(caller, native)
      try {
        const params = imports.receiveGraph(admitted, paramsType, value.graph as WireGraph, value.scopes as GraphScopes)
        // Accept before exposing parameters to user code: owner passback and
        // callbacks may immediately start a reentrant call using these grants.
        await peer.request('controls', { controls: imports.takeControls() })
        let result: any
        try { result = await exporter.dispatcher(value.object, value.view).dispatch(key, context, value.method, params) }
        finally { await context.finish() }
        const output = stage(resultType, result)
        executions.set(output.stage, { key, scope: value.scopes.borrow })
        return { ...output, controls: imports.takeControls(), finished: false }
      } catch (caught) {
        imports.closeScope(value.scopes.borrow); table.release(key)
        return { error: error(caught), controls: imports.takeControls(), finished: true }
      }
    }
    case 'finish': {
      const execution = executions.get(value.stage)
      if (!execution) throw new ProtocolError('StaleObject', 'finished', 'execution not pending')
      imports.closeScope(execution.scope); table.release(execution.key); executions.delete(value.stage)
      return { controls: imports.takeControls() }
    }
    case 'scenario': {
      const current = native.get('rustDatabase') as InterfaceDatabase
      if (current !== rustDatabase) throw new Error('native injection did not preserve the client')
      const a = await current.connect({ name: 'from-node' }); const b = await current.connect({ name: 'from-node' })
      const rows = await a.query({ sql: 'node-to-rust' })
      const inspected = await current.inspect(a)
      let callbacks = 0
      await current.withCallback({ async call(context, text) {
        if (context.native() !== native) throw new Error('wrong callback creator context')
        callbacks++
        const nested = await current.connect({ name: text }); await nested.query({ sql: 'reentrant-node' }); return null
      } })
      return { identity: a === b, cycle: a.session.agent.session === a.session, inspected, callbacks, rows }
    }
    case 'stop': await managed?.stop(); return { controls: imports.takeControls() }
    default: throw new ProtocolError('CapabilityDenied', 'control', 'unknown conformance command')
  }
}
const socket = new Socket({ fd: 3, readable: true, writable: true })
peer = new Peer(socket, handler)
process.stdout.write('native runtime diagnostics use stdout, never protocol framing\n')
socket.on('close', () => {
  for (const staged of stages.values()) staged.abort()
  stages.clear()
  for (const execution of executions.values()) { imports.closeScope(execution.scope); table.release(execution.key) }
  executions.clear(); table?.close(); void managed?.stop().catch(() => {})
})
