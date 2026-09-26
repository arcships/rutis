import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { admit, type TypeExpr } from '../src/contract.ts'
import { GraphExporter, draftGraph } from '../src/draft.ts'
import { Exports, ObjectIds } from '../src/exports.ts'
import { Imports } from '../src/imports.ts'
import { bindInterfaceSession, exportInterfaceSession, type InterfaceSessionService } from '../generated/rpc.ts'
import type { Caller } from '../src/sdk.ts'
const bundle = admit(readFileSync(new URL('../../fixtures/rpc.bundle.json', import.meta.url)))
const owner = { runtime: 'author', epoch: '1', activation: '1' }
const receiver = { runtime: 'receiver', epoch: '1', activation: '1' }
const scopes = { scope: { activation: receiver, scope: '1' }, borrow: { activation: receiver, scope: '2' } }
const type: TypeExpr = { kind: 'object', interface: 'Session', ownership: 'scope' }
function cyclic(): InterfaceSessionService { let session: InterfaceSessionService; const agent = { get session() { return session } }; session = { agent }; return session }
const caller: Caller = { async call() { throw new Error('properties cannot dispatch') } }
test('staged native cycle commits broker deliveries before releasing staging', async () => {
  const table = new Exports(owner, new ObjectIds()); const encoder = new GraphExporter(bundle, table, owner, bundle.bundle.id)
  const staged = encoder.encode(type, exportInterfaceSession(cyclic())); assert.equal(staged.draft.references.length, 2)
  const validation = draftGraph(bundle, type, staged.draft, scopes)
  const deliveries = validation.references.map((reference, index) => ({ ...reference.delivery, id: String(index + 1), token: 'a'.repeat(64) }))
  const graph = staged.commit(deliveries, scopes); assert.equal(staged.commit(deliveries, scopes), graph)
  assert.throws(() => staged.commit([{ ...deliveries[0], token: 'f'.repeat(64) }, deliveries[1]], scopes), (e: any) => e.code === 'CapabilityDenied')
  for (const d of deliveries) assert.equal(table.pins(d.object), 1)
  const imports = new Imports(); imports.openScope(scopes.scope); imports.openScope(scopes.borrow, scopes.scope)
  const session = bindInterfaceSession(imports.receiveGraph(bundle, type, graph, scopes) as any, caller)
  assert.equal(session.agent.session, session)
  const foreign = new GraphExporter(bundle, new Exports(receiver, new ObjectIds()), receiver, bundle.bundle.id).encode(type, { kind: 'foreign', value: (await import('../src/sdk.ts')).handle(session).proxy })
  assert.ok(foreign.draft.references.every(r => r.source.kind === 'foreign')); foreign.abort()
  for (const d of deliveries) table.release({ type: 'delivery', recipient: receiver, id: d.id }); await table.join()
})
test('borrowed snapshot children inherit the borrowed scope', () => {
  const table = new Exports(owner, new ObjectIds()); const encoder = new GraphExporter(bundle, table, owner, bundle.bundle.id)
  const staged = encoder.encode({ ...type, ownership: 'borrow' }, exportInterfaceSession(cyclic()))
  assert.ok(staged.draft.references.every(r => r.ownership === 'borrow'))
  const graph = draftGraph(bundle, { ...type, ownership: 'borrow' }, staged.draft, scopes)
  for (const reference of graph.references) assert.deepEqual(reference.delivery.recipient, scopes.borrow)
  staged.abort()
  for (const r of staged.draft.references) { if (r.source.kind === 'own') assert.equal(table.pins(r.source.object), 0) }
})
test('snapshot exceptions and invalid sibling values roll back all staging', () => {
  const table = new Exports(owner, new ObjectIds()); const encoder = new GraphExporter(bundle, table, owner, bundle.bundle.id)
  const native = exportInterfaceSession(cyclic()); if (native.kind !== 'own') throw new Error('expected native')
  const identity = native.value.register(table).identity
  assert.throws(() => encoder.encode(type, { kind: 'own', value: { register: table => native.value.register(table), snapshot() { throw new Error('snapshot failed') } } }), /snapshot failed/)
  assert.equal(table.pins(identity), 0)
  assert.throws(() => encoder.encode({ kind: 'record', fields: { first: type, last: { kind: 'value', schema: { type: 'boolean' } } } }, { kind: 'record', fields: { first: native, last: { kind: 'value', value: 'wrong' } } }))
  assert.equal(table.pins(identity), 0)
  const first = encoder.encode(type, native)
  const second = new GraphExporter(bundle, table, owner, bundle.bundle.id).encode(type, native)
  assert.ok(table.pins(identity) >= 2, 'encoders for different bundles must share the owner staging namespace')
  first.abort(); second.abort(); assert.equal(table.pins(identity), 0)
})
test('closed native admission rejects late reference-free results and cannot reopen', () => {
  let open = true; const table = new Exports(owner, new ObjectIds(), () => open); const encoder = new GraphExporter(bundle, table, owner, bundle.bundle.id)
  const type: TypeExpr = { kind: 'value', schema: { type: 'null' } }; open = false
  assert.throws(() => encoder.encode(type, { kind: 'value', value: null }), (e: any) => e.code === 'ScopeClosed'); open = true
  assert.throws(() => encoder.encode(type, { kind: 'value', value: null }), (e: any) => e.code === 'ScopeClosed')
})
