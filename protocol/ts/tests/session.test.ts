import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { Context } from '@deepseek-ai/cordis'
import { Exports } from '../src/exports.ts'
import { ActivationGate, ManagedActivation } from '../src/managed.ts'
import { Bundles, serviceType } from '../src/services.ts'
import { RuntimeObjects } from '../src/session.ts'
import { BUNDLE_SHA256, exportInterfaceSession, type InterfaceSessionService } from '../generated/rpc.ts'

const bundles = new Bundles([readFileSync(new URL('../../fixtures/rpc.bundle.json', import.meta.url))])
test('disconnected native admission without execute cannot retain an execution pin', async () => {
  const parent = new Context()
  const owner = { runtime: 'native', epoch: '1', activation: '1' }
  const gate = new ActivationGate(); const runtime = new RuntimeObjects({ runtime: 'native', epoch: '1' }, bundles)
  runtime.reserve(owner, gate)
  let table!: Exports
  const native = await ManagedActivation.mount(parent, { apply(ctx) { table = Exports.managed(ctx, owner, runtime.ids, () => gate.isOpen); runtime.bind(owner, ctx, table) } }, {}, {}, () => {}, [], gate)
  await native.ready()
  let session!: InterfaceSessionService
  const agent = { get session() { return session } }; session = { agent }
  const bundle = bundles.exact(BUNDLE_SHA256)
  const staged = runtime.encode(owner, bundle.sha256, serviceType({ interface: 'Session', version: '1.0.0', bundle_sha256: bundle.sha256 }), exportInterfaceSession(session))
  const source = staged.draft.references[0].source; assert.equal(source.kind, 'own'); if (source.kind !== 'own') throw new Error('expected real own object')
  await runtime.handle('object/pin', { object: source.object, key: { type: 'execution', caller: { runtime: 'host', epoch: '1', activation: '1' }, call: '1' } })
  assert.ok(table.pins(source.object) >= 2)
  runtime.close(); assert.equal(table.pins(source.object), 0)
  await native.stop(); await parent.fiber.dispose()
})
