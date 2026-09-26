// Real Cordis members over inherited fd 3. Fixture commands mount native graphs;
// all object authority, dispatch and completion use the production SDK actor.
import { Socket } from 'node:net'
import { readFileSync } from 'node:fs'
import { Context } from '@deepseek-ai/cordis'
import { Peer } from '../../src/frame.ts'
import { Exports } from '../../src/exports.ts'
import { ProtocolError } from '../../src/error.ts'
import { ActivationGate } from '../../src/managed.ts'
import { Bundles, NativePorts, type NativeServices } from '../../src/services.ts'
import { RuntimeObjects } from '../../src/session.ts'
import { handle } from '../../src/sdk.ts'
import * as rpc from '../../generated/rpc.ts'
import * as db from '../../generated/database.ts'

const bundles = new Bundles(['rpc', 'database'].map(name => readFileSync(new URL(`../../../fixtures/${name}.bundle.json`, import.meta.url))))
const runtime = new RuntimeObjects({ runtime: 'node', epoch: '1' }, bundles)
const nativeRoot = new Context()
const owner = (activation: string) => ({ runtime: 'node', epoch: '1', activation })
const contract = (hash: string) => ({ interface: 'Database', version: '1.0.0', bundle_sha256: hash })
const contracts = { rpc: contract(rpc.BUNDLE_SHA256), data: contract(db.BUNDLE_SHA256) }
const gates = new Map<string, ActivationGate>()
const members = new Map<string, NativeServices<{}>>()
const results: unknown[] = []
let cleanup = 0
let fenceSecondFinished = false
const providerTables = new Map<string, Exports>()
const providerObjects = new Map<string, import("../../src/imports.ts").ObjectIdentity>()
let borrowed: rpc.BorrowCallback0 | undefined
let connectEntered = false
let resumeConnect: (() => void) | undefined
let slowEntered = false
let resumeSlow: (() => void) | undefined
let childEntered = false
let resumeChild: (() => void) | undefined
let tamperRoute = false
let failRouteCommit = false
const routeStages = new Map<string, string>()

async function fixture(method: string, input: unknown): Promise<unknown> {
  const value = input as any
  switch (method) {
    case 'fixture/start': {
      const index = value?.activation ?? '1'; const identity = owner(index); const gate = new ActivationGate(); runtime.reserve(identity, gate); gates.set(index, gate)
      const ports = new NativePorts()
      ports.provide('rpc', 'nativeRpc', contracts.rpc, bundles, rpc.exportInterfaceDatabase)
      ports.provide('data', 'nativeData', contracts.data, bundles, db.exportInterfaceDatabase)
      const member = await ports.mount(nativeRoot, { apply(ctx) {
        const exports = Exports.managed(ctx, identity, runtime.ids, () => gate.isOpen); runtime.bind(identity, ctx, exports); providerTables.set(index, exports)
        ctx.effect(() => () => { cleanup++ })
        let session!: rpc.InterfaceSessionService
        const agent = { get session() { return session } }; session = { agent }
        let count = 0
        const connection: rpc.InterfaceConnectionService = { session, async query(context, params) {
          if (context.native() !== ctx) throw new Error('wrong creator context')
          if (params.sql === 'slow') { slowEntered = true; await new Promise<void>(resolve => { resumeSlow = resolve }) }
          if (params.sql === 'fail-child') {
            context.spawn(async () => { childEntered = true; await new Promise<void>(resolve => { resumeChild = resolve }) })
            throw new Error('handler failed before child completion')
          }
          return [{ owner: 'node', count: ++count, sql: params.sql }]
        } }
        const service: rpc.InterfaceDatabaseService = {
          async connect(context, params) { if (context.native() !== ctx) throw new Error('wrong original Ctx'); if (params.name === 'slow-connect') { connectEntered = true; await new Promise<void>(resolve => { resumeConnect = resolve }) }; return connection },
          async inspect(_, client) { return (await client.query({ sql: 'owner-passback' }))[0].owner === 'node' },
          async withCallback(context, callback) { borrowed = callback; await callback.call('callback'); context.spawn(async () => { await callback.call('descendant') }); return null },
        }
        ctx.provide('nativeRpc', service); ctx.provide('nativeData', service)
      } }, {}, identity, {}, runtime.caller(identity), gate)
      members.set(index, member)
      const staged = await member.stage(bundles, runtime.ids, providerTables.get(index))
      const source = staged.table.services.rpc.graph.references[0].source
      if (source.kind !== 'own') throw new Error('expected real native own root')
      providerObjects.set(index, source.object)
      return { table: runtime.stageServices(staged), contracts }
    }
    case 'fixture/publish': runtime.publish(owner(value.activation)); return null
    case 'fixture/reserve': { const index = value?.activation ?? '2'; const gate = new ActivationGate(); gates.set(index, gate); runtime.reserve(owner(index), gate); return null }
    case 'fixture/consume': {
      const index = value.activation.activation; const identity = owner(index); const gate = gates.get(index)!
      const ports = new NativePorts()
      ports.require('rpc', 'nativeRpc', contracts.rpc, bundles, rpc.bindInterfaceDatabase)
      ports.require('data', 'nativeData', contracts.data, bundles, db.bindInterfaceDatabase)
      const values = await runtime.receiveServices(identity, contracts, value.graphs)
      const member = await ports.mount(nativeRoot, { inject: ['nativeRpc', 'nativeData'], async apply(ctx) {
        const exports = Exports.managed(ctx, identity, runtime.ids, () => gate.isOpen); runtime.bind(identity, ctx, exports)
        ctx.effect(() => () => { cleanup++ })
        for (const name of ['nativeRpc', 'nativeData']) {
          const database = ctx.get(name) as rpc.InterfaceDatabase
          const first = await database.connect({ name: 'native-node-consumer' }); const second = await database.connect({ name: 'same' })
          let callbacks = 0
          await database.withCallback({ async call(context, text) {
            if (context.native() !== ctx) throw new Error('wrong callback creator context')
            callbacks++; await first.query({ sql: text }); return null
          } })
          results.push({ rows: await first.query({ sql: name }), identity: first === second, cycle: first.session.agent.session === first.session, passback: await database.inspect(first), callbacks })
        }
      } }, {}, identity, values, runtime.caller(identity), gate)
      members.set(index, member); await member.native.ready()
      return { results, distinct: members.get('1')!.native.nativeContext !== member.native.nativeContext }
    }
    case 'fixture/fence': {
      const database = members.get('2')!.native.nativeContext!.get('nativeRpc') as rpc.InterfaceDatabase
      const connection = await database.connect({ name: 'control-fence' }); handle(connection).proxy.release()
      const first = runtime.flush(); const second = runtime.flush().then(() => { fenceSecondFinished = true })
      await Promise.all([first, second]); return null
    }
    case 'fixture/pins': return providerTables.get(value.activation)!.pins(providerObjects.get(value.activation)!)
    case 'fixture/status': return { slowEntered, childEntered, connectEntered, fenceSecondFinished, consumerOpen: gates.get('2')?.isOpen ?? false, extraOpen: gates.get('4')?.isOpen ?? false, cleanup }
    case 'fixture/tamper-route': tamperRoute = true; return null
    case 'fixture/fail-route-commit': failRouteCommit = true; return null
    case 'fixture/resume': resumeSlow?.(); resumeChild?.(); resumeConnect?.(); return null
    case 'fixture/query': { const database = members.get('2')!.native.nativeContext!.get('nativeRpc') as rpc.InterfaceDatabase; return (await database.connect({ name: 'fixture' })).query({ sql: value.sql }) }
    case 'fixture/expired': { await borrowed!.call('expired'); throw new Error('expired callback unexpectedly ran') }
    case 'fixture/stop': { const identity = owner(value.activation); runtime.closeMember(identity); await members.get(value.activation)!.native.stop(); return { cleanup } }
    case 'fixture/retire': return runtime.retire()
    case 'fixture/close': runtime.close(); for (const member of members.values()) await member.native.stop(); await nativeRoot.fiber.dispose(); return { cleanup }
    default: throw new Error(`unknown fixture command ${method}`)
  }
}
const handler = runtime.handler(fixture)
const peer = new Peer(new Socket({ fd: 3, readable: true, writable: true }), async (method, value) => {
  if (method === 'object/commit' && routeStages.get((value as any).stage) === 'rpc' && failRouteCommit) {
    failRouteCommit = false
    throw new ProtocolError('CapabilityDenied', 'fixture', 'rejecting the second actual root commit')
  }
  const reply = await handler(method, value)
  if (method === 'object/route') routeStages.set((reply as any).stage, (value as any).service)
  if (method === 'object/abort') routeStages.delete((value as any).stage)
  if (method === 'object/route' && (value as any).service === 'data' && tamperRoute) {
    tamperRoute = false
    const changed = structuredClone(reply) as any
    changed.draft.references[0].source.view.source = 'route.' + '0'.repeat(64)
    return changed
  }
  return reply
}); runtime.attach(peer)
process.stdout.write('multi-member Cordis diagnostics remain on stdout\n')
