import assert from 'node:assert/strict'
import { test } from 'node:test'
import { Imports, parseDelivery, sequence, type Delivery, type Scope } from '../src/imports.ts'

const owner = { runtime: 'rust', epoch: '1', activation: '1' }
const caller = { runtime: 'node', epoch: '1', activation: '1' }
const root: Scope = { activation: caller, scope: '1' }
function delivery(id: string, recipient = root, source = 'route-a'): Delivery {
  return { id, token: 'test-token-' + id, object: { owner, object: '1' }, recipient, view: { interface: 'Connection', bundle_sha256: 'a'.repeat(64), source } }
}
test('T04/T07: stable identity, independent tokens and no revival of released aliases', () => {
  const imports = new Imports(); imports.openScope(root)
  const first = imports.receive(delivery('1'))
  assert.equal(first, imports.receive(delivery('2')))
  const d3 = delivery('3')
  first.release()
  assert.throws(() => first.delivery(), (e: any) => e.code === 'ScopeClosed')
  assert.throws(() => imports.receive(delivery('1')), (e: any) => e.code === 'StaleObject')
  const second = imports.receive(d3)
  assert.notEqual(first, second)
  assert.equal(first.sameObject(second), true)
  assert.equal(second.delivery().id, '3')
  assert.deepEqual(imports.takeControls().filter(c => c.type === 'release').map(c => c.id), ['1', '2'])
  assert.throws(() => Object.assign(first, { active: true }))
})
test('T05/T06: source views and independent child scopes do not merge revocation', () => {
  const imports = new Imports(); imports.openScope(root)
  const a = { ...root, scope: '2' }; const b = { ...root, scope: '3' }
  imports.openScope(a, root); imports.openScope(b, root)
  const first = imports.receive(delivery('1', a))
  const second = imports.receive(delivery('2', b))
  const otherView = imports.receive(delivery('3', b, 'route-b'))
  assert.notEqual(second, otherView)
  assert.ok(second.sameObject(otherView))
  imports.closeScope(a)
  assert.throws(() => first.delivery())
  assert.equal(second.delivery().id, '2')
  imports.closeScope(root)
  assert.throws(() => second.delivery()); assert.throws(() => otherView.delivery())
  assert.throws(() => imports.openScope(a), (e: any) => e.code === 'StaleObject')
  assert.throws(() => imports.receive(delivery('4')), (e: any) => e.code === 'ScopeClosed')
})
test('T07: out-of-order terminal receipts cannot retire gaps or revive old tokens', () => {
  const imports = new Imports(); imports.openScope(root)
  const second = imports.receive(delivery('2')); second.release()
  assert.throws(() => imports.acknowledgeRetirement('2'))
  const first = imports.receive(delivery('1')); first.release()
  imports.acknowledgeRetirement('2'); imports.acknowledgeRetirement('2')
  assert.throws(() => imports.receive(delivery('1')), (e: any) => e.code === 'StaleObject')
  const next = imports.receive(delivery('3'))
  assert.equal(next.delivery().id, '3')
})
test('wire sequences preserve u64 exactly and invalid decimal/numeric ids are rejected', () => {
  sequence('18446744073709551615')
  for (const invalid of ['0', '01', '-1', '18446744073709551616', 9007199254740992]) assert.throws(() => sequence(invalid))
  const d = delivery('1'); const copied = parseDelivery(d)
  d.object.object = '9'
  assert.equal(copied.object.object, '1')
  assert.throws(() => parseDelivery({ ...d, extra: true }))
})
