import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { Context } from '@deepseek-ai/cordis'
import { ManagedActivation } from '../src/managed.ts'
import { Imports, type Delivery } from '../src/imports.ts'
import { Exports, ObjectIds, type PinKey } from '../src/exports.ts'
import { CallContext, release, sameObject, type Caller, type Outbound } from '../src/sdk.ts'
import { bindInterfaceSession, exportInterfaceDatabase, exportInterfaceConnection, BUNDLE_SHA256, type InterfaceDatabaseService, type InterfaceConnectionService, type InterfaceSessionService, type InterfaceAgentService } from '../generated/database.ts'
import { bindInterfaceValues, BUNDLE_SHA256 as SHAPES_SHA256 } from '../generated/binding-types.ts'

const owner = { runtime: 'owner', epoch: '1', activation: '1' }
const recipient = { activation: { runtime: 'receiver', epoch: '1', activation: '1' }, scope: '1' }
const delivery = (iface: string, hash = BUNDLE_SHA256): Delivery => ({ id: '1', token: 'a'.repeat(64), object: { owner, object: '1' }, recipient, view: { interface: iface, bundle_sha256: hash, source: 'native' } })
test('generated TS properties keep facade identity through cycles and deny undeclared selectors', async () => {
  const admitted = JSON.parse(readFileSync(new URL('../../fixtures/database.bundle.json', import.meta.url), 'utf8'))
  const graph = JSON.parse(readFileSync(new URL('../../fixtures/session.graph.json', import.meta.url), 'utf8'))
  // This is a decoder/binding unit fixture; authentication belongs to the broker.
  const scope = graph.references[0].delivery.recipient
  for (const ref of graph.references) ref.delivery.view.bundle_sha256 = BUNDLE_SHA256
  const imports = new Imports(); imports.openScope(scope)
  const proxy = imports.receiveGraph({ bundle: admitted, sha256: BUNDLE_SHA256 }, admitted.interfaces.Agent.properties.session, graph, { scope, borrow: scope })
  let calls = 0
  const caller: Caller = { async call() { calls++; throw new Error('properties cannot perform calls') } }
  const session = bindInterfaceSession(proxy as any, caller)
  assert.equal(session.agent.session, session)
  assert.equal(bindInterfaceSession(proxy as any, caller), session)
  assert.equal(await Promise.resolve(session), session)
  assert.equal(calls, 0)
  assert.ok(sameObject(session, session.agent.session))
  assert.throws(() => (session as any).constructor, (e: any) => e.code === 'CapabilityDenied')
  assert.throws(() => (session as any).arbitraryMember, (e: any) => e.code === 'CapabilityDenied')
  assert.throws(() => { (session as any).agent = null }, TypeError)
  release(session)
  assert.ok(sameObject(session, session), 'identity comparison grants no call authority')
  assert.throws(() => session.agent, (e: any) => e.code === 'ScopeClosed')
})

test('generated native adapters require execution pins and select only declared methods', async () => {
  const exports = new Exports(owner, new ObjectIds())
  let calls = 0
  let session: InterfaceSessionService
  const agent: InterfaceAgentService = { get session() { return session } }
  session = { agent }
  const connection: InterfaceConnectionService = {
    session,
    async query(_, params) { calls++; return [{ sql: params.sql, count: calls }] },
  }
  let unexpectedGetter = 0
  Object.defineProperty(connection, 'undeclared', { get() { unexpectedGetter++; throw new Error('must not reflect') } })
  const native: InterfaceDatabaseService = {
    async connect(_, params) { assert.equal(params.name, 'db'); return connection },
    async inspect(_, params) { await params.query({ sql: 'inspect' }); return true },
    async withCallback(_, params) { await params.call('callback'); return null },
  }
  const output = exportInterfaceDatabase(native)
  assert.equal(output.kind, 'own'); if (output.kind !== 'own') return
  const registered = output.value.register(exports)
  assert.deepEqual(output.value.register(exports).identity, registered.identity)
  const caller: Caller = { async call() { throw new Error('no imported call in this unit') } }
  const context = new CallContext(caller)
  const key: PinKey = { type: 'execution', caller: recipient.activation, call: '1' }
  await assert.rejects(registered.dispatcher.dispatch(key, context, 'connect', { name: 'db' }), (e: any) => e.code === 'StaleObject')
  exports.pin(registered.identity, key)
  const returned = await registered.dispatcher.dispatch(key, context, 'connect', { name: 'db' })
  assert.equal(returned.kind, 'own'); if (returned.kind !== 'own') return
  const conn = returned.value.register(exports)
  assert.deepEqual(returned.value.snapshot().session.kind, 'own')
  const connKey: PinKey = { type: 'execution', caller: recipient.activation, call: '2' }
  exports.pin(conn.identity, connKey)
  const result = await conn.dispatcher.dispatch(connKey, context, 'query', { sql: 'select' })
  assert.deepEqual(result, { kind: 'value', value: [{ sql: 'select', count: 1 }] })
  await assert.rejects(conn.dispatcher.dispatch(connKey, context, 'arbitraryMember', null), (e: any) => e.code === 'CapabilityDenied')
  const another = exportInterfaceConnection({ ...connection })
  assert.equal(another.kind, 'own'); if (another.kind !== 'own') return
  const other = another.value.register(exports)
  const otherKey: PinKey = { type: 'execution', caller: recipient.activation, call: '3' }
  exports.pin(other.identity, otherKey)
  await assert.rejects(conn.dispatcher.dispatch(otherKey, context, 'query', { sql: 'wrong object' }), (e: any) => e.code === 'CapabilityDenied')
  assert.equal(unexpectedGetter, 0)
  exports.release(key); exports.release(connKey); exports.release(otherKey); exports.close(); await exports.join()
})

test('a declared then selector has a callable alias without Promise assimilation', async () => {
  const imports = new Imports(); imports.openScope(recipient)
  const proxy = imports.receive(delivery('Values', SHAPES_SHA256))
  const calls: string[] = []
  const caller: Caller = { async call(_, method, params: Outbound) { calls.push(method); assert.equal(params.kind, 'value'); return 'typed' } }
  const client = bindInterfaceValues(proxy, caller)
  assert.equal(await Promise.resolve(client), client)
  assert.equal(await client['then$']('hello'), 'typed')
  assert.deepEqual(calls, ['then'])
})

test('native admission gates synchronously reject aliases in child scopes', () => {
  const imports = new Imports(); imports.openScope(recipient)
  const child = { ...recipient, scope: '2' }; imports.openScope(child, recipient)
  let open = true; imports.bindScope(recipient, () => open)
  const proxy = imports.receive({ ...delivery('Values', SHAPES_SHA256), recipient: child })
  assert.ok(proxy.delivery()); open = false
  assert.throws(() => proxy.delivery(), (e: any) => e.code === 'ScopeClosed')
  open = true
  assert.throws(() => proxy.delivery(), (e: any) => e.code === 'ScopeClosed')
  assert.throws(() => imports.bindScope(recipient, () => true), (e: any) => e.code === 'ScopeClosed')
  imports.closeScope(recipient); assert.equal(imports.retainedObjects(), 0)
})
test('registered TS descendants finish before execution closes and report rejection', async () => {
  const caller: Caller = { async call() { throw new Error('unused') } }
  const context = new CallContext(caller)
  const events: string[] = []
  let resolve!: () => void
  const promise = new Promise<void>(done => { resolve = done })
  context.spawn(async () => {
    await promise
    context.spawn(async () => { events.push('descendant') })
  })
  let finished = false
  const join = context.finish().then(() => { finished = true })
  await Promise.resolve(); assert.equal(finished, false); resolve(); await join
  assert.deepEqual(events, ['descendant'])
  assert.throws(() => context.spawn(async () => {}), (e: any) => e.code === 'ScopeClosed')
  const failed = new CallContext(caller); failed.spawn(async () => { throw new Error('child failed') })
  await assert.rejects(failed.finish(), /child failed/)
})
test('an owner revocation also rejects an unseen late handoff', () => {
  const imports = new Imports(); imports.openScope(recipient); imports.revokeOwner(owner)
  assert.throws(() => imports.receive(delivery('Values', SHAPES_SHA256)), (e: any) => e.code === 'StaleObject')
  assert.equal(imports.retainedObjects(), 0)
})

test('generated dispatch uses the original managed Cordis context', async () => {
  const root = new Context()
  const remove = root.provide('dependency', { version: 7 })
  let ctx!: Context
  const managed = new ManagedActivation(root, { inject: ['dependency'], apply(current) { ctx = current } }, {})
  await managed.ready()
  const table = Exports.managed(ctx, owner, new ObjectIds(), () => managed.isOpen)
  let session: InterfaceSessionService
  const agent: InterfaceAgentService = { get session() { return session } }; session = { agent }
  const native: InterfaceConnectionService = { session, async query(context, params) {
    assert.equal(context.native(), ctx)
    assert.deepEqual(context.native().get('dependency'), { version: 7 })
    return [{ sql: params.sql }]
  } }
  const value = exportInterfaceConnection(native)
  assert.equal(value.kind, 'own'); if (value.kind !== 'own') return
  const registered = value.value.register(table)
  const key: PinKey = { type: 'execution', caller: recipient.activation, call: '1' }
  table.pin(registered.identity, key)
  const caller: Caller = { async call() { throw new Error('unused in native adapter unit') } }
  const result = await registered.dispatcher.dispatch(key, new CallContext(caller, ctx), 'query', { sql: 'native' })
  assert.deepEqual(result, { kind: 'value', value: [{ sql: 'native' }] })
  table.release(key)
  const cleanup = remove()
  assert.throws(() => table.pin(registered.identity, { ...key, call: '2' }), (e: any) => e.code === 'ScopeClosed')
  await Promise.all([cleanup, managed.stop()]); await root.fiber.dispose()
})
