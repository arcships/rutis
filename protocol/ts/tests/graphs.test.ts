import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { admit, callbackKey, canonical, ProtocolError, type TypeExpr } from '../src/contract.ts'
import { Imports, ObjectProxy } from '../src/imports.ts'
import type { GraphScopes, WireGraph } from '../src/graph.ts'

const admitted = admit(readFileSync(new URL('../../fixtures/database.bundle.json', import.meta.url)))
const root = { activation: { runtime: 'node', epoch: '1', activation: '1' }, scope: '1' }
const scopes: GraphScopes = { scope: root, borrow: { ...root, scope: '2' } }
const session = admitted.bundle.interfaces.Agent.properties!.session
const fixture = JSON.parse(readFileSync(new URL('../../fixtures/session.graph.json', import.meta.url), 'utf8'))
const corpus = JSON.parse(readFileSync(new URL('../../fixtures/graph-corpus.json', import.meta.url), 'utf8'))
function graph(input = fixture): WireGraph {
  const graph = structuredClone(input)
  for (const reference of graph.references ?? []) {
    const view = reference.delivery.view
    if (view.bundle_sha256 === '$bundle') view.bundle_sha256 = admitted.sha256
    if (view.interface === '$callback') view.interface = callbackKey(admitted.bundle.interfaces.Database.methods.withCallback.params)
  }
  return graph
}
function imports(receiving = scopes): Imports {
  const imports = new Imports()
  imports.openScope(receiving.scope); imports.openScope(receiving.borrow, canonical(receiving.scope.activation) === canonical(receiving.borrow.activation) ? receiving.scope : undefined)
  return imports
}
function proxy(value: unknown): ObjectProxy { assert.ok(value instanceof ObjectProxy); return value }
function renew(graph: WireGraph, start: number): WireGraph {
  graph.references.forEach((r, i) => { r.delivery.id = String(start + i); r.delivery.token = 'fixture-' + (start + i) })
  return graph
}

test('T22: shared graph contracts, table validation and ownership corpus', () => {
  for (const entry of corpus) {
    const runtime = imports(entry.scopes)
    let error: string | null = null
    try { runtime.receiveGraph(admitted, entry.type, graph(entry.graph), entry.scopes) }
    catch (caught) { assert.ok(caught instanceof ProtocolError); error = caught.code }
    assert.equal(error, entry.error, entry.name)
  }
})
test('T04/T10: cyclic immutable relationships and reordered deliveries keep proxy identity', () => {
  const runtime = imports()
  const first = proxy(runtime.receiveGraph(admitted, session, graph(), scopes))
  const agent = proxy(first.property('agent'))
  assert.equal(agent.property('session'), first)
  assert.equal(runtime.retainedObjects(), 2)
  assert.throws(() => first.property('unknown'), (e: any) => e.code === 'CapabilityDenied')
  const reordered = renew(graph(), 3)
  reordered.references.reverse(); reordered.root = { kind: 'ref', index: 1 }
  reordered.references[0].properties.session = { kind: 'ref', index: 1 }
  reordered.references[1].properties.agent = { kind: 'ref', index: 0 }
  assert.equal(runtime.receiveGraph(admitted, session, reordered, scopes), first)
  agent.release()
  assert.throws(() => first.property('agent'), (e: any) => e.code === 'ScopeClosed')
  assert.equal(runtime.retainedObjects(), 1)
  assert.equal(runtime.receiveGraph(admitted, session, renew(graph(), 5), scopes), first)
  const fresh = proxy(first.property('agent'))
  assert.notEqual(fresh, agent); assert.ok(fresh.sameObject(agent))
  assert.throws(() => agent.delivery())
  runtime.closeScope(scopes.scope)
  assert.equal(runtime.retainedObjects(), 0)
  assert.throws(() => first.property('agent')); assert.throws(() => fresh.property('session'))
})
test('T07: invalid fresh snapshot releases only new grants and preserves earlier aliases', () => {
  const runtime = imports()
  const first = proxy(runtime.receiveGraph(admitted, session, graph(), scopes))
  const original = first.property('agent'); runtime.takeControls()
  const changed = renew(graph(), 3); changed.references[1].delivery.object.object = '99'
  assert.throws(() => runtime.receiveGraph(admitted, session, changed, scopes), (e: any) => e.code === 'InvalidParams')
  assert.deepEqual(runtime.takeControls().map(c => [c.type, c.id]), [['release', '3'], ['release', '4']])
  assert.equal(first.property('agent'), original)
  assert.equal(runtime.retainedObjects(), 2)
})
test('T06/T08: borrow graph expires without closing separately scoped persistent aliases', () => {
  const entry = corpus.find((e: any) => e.name === 'same identity with independent durable and borrow grants')
  const runtime = imports()
  const result = runtime.receiveGraph(admitted, entry.type, graph(entry.graph), scopes) as any
  const durable = proxy(result.durable), temporary = proxy(result.temporary)
  assert.notEqual(durable, temporary); assert.ok(durable.sameObject(temporary))
  const child = proxy(temporary.property('agent'))
  assert.deepEqual(child.scope, scopes.borrow)
  runtime.closeScope(scopes.borrow)
  assert.throws(() => temporary.delivery()); assert.throws(() => child.delivery())
  assert.ok(durable.property('agent')); assert.equal(runtime.retainedObjects(), 2)
  runtime.closeScope(scopes.scope); assert.equal(runtime.retainedObjects(), 0)
})
test('reference-free late result still checks the receiving scope', () => {
  const runtime = imports(); runtime.closeScope(scopes.scope)
  const type: TypeExpr = { kind: 'value', schema: { type: 'null' } }
  assert.throws(() => runtime.receiveGraph(admitted, type, { root: { kind: 'value', value: null }, references: [] }, scopes), (e: any) => e.code === 'ScopeClosed')
})
