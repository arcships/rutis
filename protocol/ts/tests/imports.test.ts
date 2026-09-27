import assert from 'node:assert/strict'
import { test } from 'node:test'
import { Imports, parseDelivery, sequence, type Delivery, type Scope } from '../src/imports.ts'

const owner = { runtime: 'rust', epoch: '1', activation: '1' }
const caller = { runtime: 'node', epoch: '1', activation: '1' }
const root: Scope = { activation: caller, scope: '1' }
function delivery(id: string, recipient = root, source = 'route-a'): Delivery {
  return { id, token: 'test-token-' + id, object: { owner, object: '1' }, recipient, view: { interface: 'Connection', bundle_sha256: 'a'.repeat(64), source } }
}
test('receiver retirement proposals never cross a receipt gap or live token', () => {
  const imports = new Imports(); imports.openScope(root)
  imports.receive(delivery('2', root, 'route-b')).release(); assert.equal(imports.retirement(), null)
  const first = imports.receive(delivery('1')); assert.equal(imports.retirement(), null); first.release()
  assert.deepEqual(imports.retirement(), { received_through: '2', terminal_through: '2' }); imports.acknowledgeRetirement('2')
  imports.receive(delivery('4', root, 'route-b')).release(); assert.deepEqual(imports.retirement(), { received_through: '2', terminal_through: '2' })
  assert.throws(() => imports.acknowledgeRetirement('4'))
  const third = imports.receive(delivery('3')); assert.deepEqual(imports.retirement(), { received_through: '4', terminal_through: '2' }); third.release()
  assert.deepEqual(imports.retirement(), { received_through: '4', terminal_through: '4' })
})
test('individual object revocation closes every source and scope without reviving late envelopes', () => {
  const imports = new Imports(); imports.openScope(root)
  const child = { ...root, scope: '2' }; imports.openScope(child, root)
  const first = imports.receive(delivery('1'))
  const alias = imports.receive(delivery('2', child, 'route-b'))
  const unrelated = { ...delivery('3'), object: { owner, object: '2' } }
  const live = imports.receive(unrelated)
  imports.revokeObjects([{ owner, object: '1' }, { owner, object: '1' }])
  assert.throws(() => first.delivery()); assert.throws(() => alias.delivery())
  assert.equal(live.delivery().id, '3')
  assert.throws(() => imports.receive(delivery('1')), (e: any) => e.code === 'StaleObject')
  assert.throws(() => imports.receive(delivery('4')), (e: any) => e.code === 'StaleObject')
  imports.revokeObjects([{ owner, object: '99' }])
  const late = { ...delivery('5'), object: { owner, object: '99' } }
  assert.throws(() => imports.receive(late), (e: any) => e.code === 'StaleObject')
  const fresh = { ...delivery('6'), object: { owner, object: '3' } }
  assert.throws(() => imports.receiveBatch([fresh, late]))
  assert.equal(live.delivery().id, '3')
  assert.deepEqual(imports.takeControls().filter(c => c.type === 'release').map(c => c.id), ['1', '2', '4', '5', '6'])
})
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
test('failed graph rejects its new handoffs atomically and preserves older aliases', () => {
  const imports = new Imports(); imports.openScope(root)
  const first = delivery('1')
  const alias = imports.receive(first); imports.takeControls()
  const next = delivery('2')
  assert.throws(() => imports.receiveBatch([next, { ...first, token: 'forged' }]), (e: any) => e.code === 'CapabilityDenied')
  assert.equal(alias.delivery().id, '1')
  assert.deepEqual(imports.takeControls(), [{ type: 'release', id: '2', token: next.token }])
  const rejected = delivery('3')
  imports.reject([first, rejected])
  assert.equal(alias.delivery().id, '1')
  assert.deepEqual(imports.takeControls(), [{ type: 'release', id: '3', token: rejected.token }])
  const fourth = delivery('4')
  const batch = imports.receiveBatch([fourth, fourth])
  assert.equal(batch[0], alias); assert.equal(batch[1], alias)
  alias.release(); imports.takeControls(); imports.acknowledgeRetirement('4')
  assert.throws(() => imports.receive(delivery('2')), (e: any) => e.code === 'StaleObject')
})
