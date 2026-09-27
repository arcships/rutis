import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { Context } from '@deepseek-ai/cordis'
import { admit, callbackKey, canonical } from '../src/contract.ts'
import { GraphExporter, sourceObject, sourceView } from '../src/draft.ts'
import { ObjectIds } from '../src/exports.ts'
import { Imports, type Activation } from '../src/imports.ts'
import { ActivationGate, NativeState } from '../src/managed.ts'
import { CallContext, handle, type Caller } from '../src/sdk.ts'
import { Bundles, NativePorts, serviceType, validateTable, type NativeServices, type StagedServices } from '../src/services.ts'
import type { CatalogService } from '../src/lifecycle.ts'
import type { WireGraph } from '../src/graph.ts'
import { BUNDLE_SHA256, bindInterfaceSession, exportInterfaceSession, exportInterfaceDatabase, type InterfaceSession, type InterfaceSessionService } from '../generated/database.ts'
import * as rpc from '../generated/rpc.ts'

const raw = readFileSync(new URL('../../fixtures/database.bundle.json', import.meta.url))
const rpcRaw = readFileSync(new URL('../../fixtures/rpc.bundle.json', import.meta.url))
const bundles = new Bundles([raw, rpcRaw])
const bundle = admit(raw)
const session: CatalogService = { interface: 'Session', version: '1.0.0', bundle_sha256: BUNDLE_SHA256 }
const rpcSession: CatalogService = { ...session, bundle_sha256: rpc.BUNDLE_SHA256 }
const owner = (runtime: string, activation = '1'): Activation => ({ runtime, epoch: '1', activation })
const scopes = (activation: Activation) => { const scope = { activation, scope: '1' }; return { scope, borrow: scope } }
function cyclic(): InterfaceSessionService { let session!: InterfaceSessionService; const agent = { get session() { return session } }; session = { agent }; return session }
const caller: Caller = { async call() { throw new Error('property-only binding must not call transport') } }

// Owner commit conformance fixture only. It retains identities allocated from
// real native objects. Authoritative multi-bundle broker admission is exercised
// by Rust tests/services.rs; this fixture does not pretend to implement it.
function handoff(staged: StagedServices, recipient: Activation): Record<string, WireGraph> {
  let id = 0
  return Object.fromEntries(Object.entries(staged.table.services).map(([name, service]) => [name, {
    root: structuredClone(service.graph.root),
    references: service.graph.references.map(reference => ({
      delivery: { id: String(++id), token: `commit-manifest-${id}`, object: sourceObject(reference.source), view: sourceView(reference.source), recipient: scopes(recipient).scope },
      properties: structuredClone(reference.properties),
    })),
  }]))
}
async function provider(root: Context, identity: Activation) {
  const ports = new NativePorts()
  ports.provide('session', 'nativeSession', session, bundles, exportInterfaceSession)
  ports.check({ config_sha256: '0'.repeat(64), provides: { session }, requires: {} })
  const member = await ports.mount(root, { apply(ctx) { ctx.provide('nativeSession', cyclic()) } }, {}, identity, {}, caller)
  const staged = await member.stage(bundles, new ObjectIds())
  return { member, staged }
}

test('shared named service table corpus matches both SDKs', () => {
  const cases = JSON.parse(readFileSync(new URL('../../fixtures/services-corpus.json', import.meta.url), 'utf8'))
  for (const c of cases) {
    if (c.error === null) assert.doesNotThrow(() => validateTable(c.table, c.contracts, bundles), c.name)
    else assert.throws(() => validateTable(c.table, c.contracts, bundles), (error: any) => error.code === c.error, c.name)
  }
})

test('native Cordis ports inject generated service facades into isolated members', async () => {
  const root = new Context()
  const members: { source: NativeServices<{}>; staged: StagedServices; consumer: NativeServices<{}>; imports: Imports; seen: InterfaceSession; original: Context }[] = []
  for (const index of ['1', '2']) {
    const { member: source, staged } = await provider(root, owner('provider', index))
    const recipient = owner('consumer', index)
    const graphs = handoff(staged, recipient)
    staged.commit(graphs, scopes(recipient))
    const imports = new Imports(); imports.openScope(scopes(recipient).scope)
    const gate = new ActivationGate(); imports.bindScope(scopes(recipient).scope, () => gate.isOpen)
    const value = imports.receiveGraph(bundle, serviceType(session), graphs.session, scopes(recipient))
    const ports = new NativePorts()
    ports.require('session', 'nativeSession', session, bundles, bindInterfaceSession)
    let seen!: InterfaceSession
    let original!: Context
    const consumer = await ports.mount(root, { inject: { nativeSession: {} }, apply(ctx) {
      original = ctx
      seen = ctx.get('nativeSession') as InterfaceSession
      assert.equal(seen.agent.session, seen)
    } }, {}, recipient, { session: value }, caller, gate)
    await consumer.native.ready()
    assert.equal(root.get('nativeSession'), undefined)
    await assert.rejects(source.stage(bundles, new ObjectIds()), (error: any) => error.code === 'Unavailable')
    members.push({ source, staged, consumer, imports, seen, original })
  }
  assert.notEqual(members[0].original, members[1].original)
  assert.notEqual(members[0].seen, members[1].seen)
  handle(members[0].seen).proxy.release()
  members[0].consumer.refreshImports()
  assert.equal(members[0].consumer.native.gate.isOpen, false)
  assert.throws(() => members[0].original.effect(() => () => {}), /inactive context/)
  await members[0].consumer.native.stop()
  assert.equal(members[1].consumer.native.native.state, NativeState.ACTIVE)
  assert.equal(members[1].seen.agent.session, members[1].seen)
  for (const member of members) { await member.consumer.native.stop(); member.imports.closeScope(scopes(owner('consumer', member.staged.table.activation.activation)).scope); await member.source.native.stop() }
  await root.fiber.dispose()
})

test('complete native manifest rejects changed siblings before converting staging pins', async () => {
  const root = new Context()
  const { member, staged } = await provider(root, owner('provider'))
  const recipient = owner('host')
  const graph = handoff(staged, recipient)
  assert.throws(() => { staged.table.services.session.stage = '2' }, TypeError)
  const first = graph.session.references[0].delivery.object
  const before = staged.exports.pins(first)
  const changed = structuredClone(graph)
  changed.session.references[1].delivery.object.object = '999'
  assert.throws(() => staged.commit(changed, scopes(recipient)), (error: any) => error.code === 'CapabilityDenied')
  assert.equal(staged.exports.pins(first), before)
  staged.commit(graph, scopes(recipient))
  const committed = staged.exports.pins(first)
  staged.commit(graph, scopes(recipient))
  assert.equal(staged.exports.pins(first), committed)
  const replay = structuredClone(graph); replay.session.references[0].delivery.token = 'changed'
  assert.throws(() => staged.commit(replay, scopes(recipient)), (error: any) => error.code === 'CapabilityDenied')
  assert.equal(staged.exports.pins(first), committed)
  const target = new GraphExporter(bundle, staged.exports, staged.table.activation, 'dispatch-target')
  staged.mergeDispatchers(target)
  assert.equal(target.dispatcher(first, graph.session.references[0].delivery.view), staged.exporter('session').dispatcher(first, graph.session.references[0].delivery.view))
  const { member: unrelated, staged: other } = await provider(root, owner('other'))
  assert.throws(() => target.mergeRegistered(other.exporter('session')), (error: any) => error.code === 'CapabilityDenied')
  await unrelated.native.stop(); await member.native.stop(); await root.fiber.dispose()
})

test('multi-bundle native pin failure rolls back the complete service table and is terminal', async () => {
  const root = new Context()
  const ports = new NativePorts()
  ports.provide('a', 'first', session, bundles, exportInterfaceSession)
  ports.provide('z', 'last', rpcSession, bundles, rpc.exportInterfaceSession)
  const member = await ports.mount(root, { apply(ctx) { ctx.provide('first', cyclic()); ctx.provide('last', cyclic()) } }, {}, owner('provider'), {}, caller)
  const staged = await member.stage(bundles, new ObjectIds())
  const recipient = owner('host')
  const graph = handoff(staged, recipient)
  staged.exports.release({ type: 'delivery', recipient, id: graph.z.references[0].delivery.id })
  assert.throws(() => staged.commit(graph, scopes(recipient)), (error: any) => error.code === 'StaleObject')
  for (const service of Object.values(graph)) for (const reference of service.references) assert.equal(staged.exports.pins(reference.delivery.object), 0)
  assert.throws(() => staged.commit(graph, scopes(recipient)), (error: any) => error.code === 'ScopeClosed')
  await member.native.stop(); await root.fiber.dispose()
})

test('necessary child export loss closes old original contexts and late native object registration', async () => {
  const root = new Context()
  const ports = new NativePorts()
  ports.provide('session', 'nativeSession', session, bundles, exportInterfaceSession)
  let remove!: () => unknown
  const member = await ports.mount(root, { async apply(ctx) {
    await ctx.plugin({ apply(child) { remove = child.provide('nativeSession', cyclic()) } })
  } }, {}, owner('provider'), {}, caller)
  const staged = await member.stage(bundles, new ObjectIds())
  const original = member.native.nativeContext!
  await remove()
  assert.equal(member.native.gate.isOpen, false)
  assert.throws(() => original.provide('late', {}), /inactive context/)
  assert.throws(() => staged.exports.register({}), (error: any) => error.code === 'ScopeClosed')
  await member.native.stop(); await root.fiber.dispose()
})

test('native database adapter dispatch retains its actual original Cordis apply context', async () => {
  const root = new Context()
  const ports = new NativePorts()
  const contract = { ...session, interface: 'Database' }
  ports.provide('db', 'database', contract, bundles, exportInterfaceDatabase)
  let count = 0
  const member = await ports.mount(root, { apply(ctx) {
    ctx.provide('database', {
      async connect(context: CallContext) { assert.equal(context.native(), ctx); count++; throw new Error('stateful native result') },
      async inspect() { return true }, async withCallback() { return null },
    })
  } }, {}, owner('provider'), {}, caller)
  const staged = await member.stage(bundles, new ObjectIds())
  const native = staged.table.services.db.graph.references[0].source
  assert.equal(native.kind, 'own')
  const object = sourceObject(native)
  const pin = { type: 'execution', caller: owner('host'), call: '1' } as const
  staged.exports.pin(object, pin)
  const context = new CallContext(caller, member.native.nativeContext)
  await assert.rejects(staged.exporter('db').dispatcher(object, sourceView(native)).dispatch(pin, context, 'connect', { name: 'state' }), /stateful native result/)
  await context.finish(); staged.exports.release(pin)
  assert.equal(count, 1)
  await member.native.stop(); await root.fiber.dispose()
})

test('native port declaration and typed binding errors precede native plugin apply', async () => {
  assert.throws(() => new Bundles([raw, Buffer.concat([raw, Buffer.from('\n')])]), (error: any) => error.code === 'InterfaceMismatch')
  assert.throws(() => bundles.service({ ...session, version: '2.0.0' }), (error: any) => error.code === 'InterfaceMismatch')
  const root = new Context()
  const ports = new NativePorts()
  ports.require('session', 'nativeSession', session, bundles, bindInterfaceSession)
  assert.throws(() => ports.require('other', 'nativeSession', session, bundles, bindInterfaceSession), (error: any) => error.code === 'InvalidParams')
  assert.throws(() => ports.check({ config_sha256: '0'.repeat(64), provides: {}, requires: {} }), (error: any) => error.code === 'InterfaceMismatch')
  let applied = false
  const plugin = { apply() { applied = true } }
  await assert.rejects(ports.mount(root, plugin, {}, owner('consumer'), {}, caller), (error: any) => error.code === 'InterfaceMismatch')
  await assert.rejects(ports.mount(root, { ...plugin, inject: ['nativeSession'] }, {}, owner('consumer'), { session: {} }, caller), (error: any) => error.code === 'InterfaceMismatch')
  assert.equal(applied, false)
  assert.equal(root.get('nativeSession'), undefined)
  await root.fiber.dispose()
})

test('explicit local dependencies use native readiness and invalidate the original context on loss', async () => {
  const root = new Context()
  let applied = 0
  let original!: Context
  const nativeValue = { count: 3 }
  const member = await new NativePorts().mount(root, { inject: ['localSettings'], apply(ctx) {
    original = ctx
    assert.equal(ctx.get('localSettings'), nativeValue)
    applied++
  } }, {}, owner('local-consumer'), {}, caller, new ActivationGate(), ['localSettings'])
  assert.equal(applied, 0, 'missing local dependency must not enter apply')
  const provider = root.plugin({ apply(ctx) { ctx.provide('localSettings', nativeValue) } })
  await provider
  await member.native.ready()
  assert.equal(applied, 1)
  await provider.dispose()
  assert.equal(member.native.gate.isOpen, false)
  assert.throws(() => original.effect(() => () => {}), /inactive context/)
  await member.native.stop(); await root.fiber.dispose()
})

test('local dependency declarations reject duplicate names and protocol port overlap before apply', async () => {
  const root = new Context()
  let applied = false
  const plugin = { inject: ['nativeSession'], apply() { applied = true } }
  const ports = new NativePorts()
  ports.provide('session', 'nativeSession', session, bundles, exportInterfaceSession)
  for (const names of [['nativeSession'], ['nativeSession', 'nativeSession'], ['not a service']]) {
    await assert.rejects(ports.mount(root, plugin, {}, owner('provider'), {}, caller, new ActivationGate(), names), (error: any) => error.code === 'InterfaceMismatch')
  }
  assert.equal(applied, false)
  await root.fiber.dispose()
})

test('native provider registration failure joins rollback of every earlier isolated import', async () => {
  const root = new Context()
  const { member: source, staged } = await provider(root, owner('provider'))
  const recipient = owner('consumer')
  const graphs = handoff(staged, recipient); staged.commit(graphs, scopes(recipient))
  const imports = new Imports(); imports.openScope(scopes(recipient).scope)
  const value = imports.receiveGraph(bundle, serviceType(session), graphs.session, scopes(recipient))
  const ports = new NativePorts()
  ports.require('a', 'nativeSession', session, bundles, bindInterfaceSession)
  ports.require('z', 'blockedImport', session, bundles, bindInterfaceSession)
  const removeAccessor = root.reflect.accessor('blockedImport', { get() { return undefined } })
  let scope!: Context
  let applied = false
  const remove = root.on('internal/service', function (name) { if (name === 'nativeSession') scope = this }, { global: true })
  await assert.rejects(ports.mount(root, { inject: ['nativeSession', 'blockedImport'], apply() { applied = true } }, {}, recipient, { a: value, z: value }, caller), /already declared/)
  assert.equal(applied, false)
  assert.ok(scope)
  assert.equal(scope.get('nativeSession'), undefined, 'failed mount cannot retain the first private provider')
  await removeAccessor(); await remove(); imports.closeScope(scopes(recipient).scope); await source.native.stop(); await root.fiber.dispose()
})

test('empty native service handoff cannot confirm after its original owner closes', async () => {
  const root = new Context()
  const member = await new NativePorts().mount(root, { apply() {} }, {}, owner('empty'), {}, caller)
  const staged = await member.stage(bundles, new ObjectIds())
  assert.deepEqual(Object.keys(staged.table.services), [])
  await member.native.stop()
  assert.throws(() => staged.commit({}, scopes(owner('host'))), (error: any) => error.code === 'ScopeClosed')
  await root.fiber.dispose()
})

test('exact method registry resolves nested callback signatures without prototype selectors', () => {
  const raw = JSON.parse(rpcRaw.toString())
  const callback = raw.interfaces.Database.methods.withCallback.params
  const nested = { kind: 'callback', params: callback, result: { kind: 'value', schema: { type: 'null' } }, ownership: 'borrow' }
  raw.id = 'lookup.bundle'
  raw.interfaces.Database.methods.nested = { params: { kind: 'record', fields: { callbacks: { kind: 'list', item: { kind: 'optional', item: callback } } } }, result: { kind: 'value', schema: { type: 'null' } } }
  raw.interfaces.Database.methods.nestedCallback = { params: nested, result: { kind: 'value', schema: { type: 'null' } } }
  const bytes = Buffer.from(JSON.stringify(raw)); const admitted = admit(bytes); const registry = new Bundles([bytes])
  const view = (interfaceName: string) => ({ interface: interfaceName, bundle_sha256: admitted.sha256, source: 'full-view-is-authorized-by-broker' })
  for (const type of [callback, nested]) assert.equal(canonical(registry.method(view(callbackKey(type)), 'call')), canonical({ params: type.params, result: type.result }))
  for (const method of ['toString', 'constructor', 'missing']) assert.throws(() => registry.method(view('Database'), method), (e: any) => e.code === 'CapabilityDenied')
  assert.throws(() => registry.method(view('$callback:unprepared'), 'call'), (e: any) => e.code === 'CapabilityDenied')
  assert.throws(() => registry.method({ ...view(callbackKey(callback)), bundle_sha256: '0'.repeat(64) }, 'call'), (e: any) => e.code === 'InterfaceMismatch')
})
