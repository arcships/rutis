import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { Bundles, NativePorts } from './node_modules/@rutis/protocol/src/services.js'
import { BUNDLE_SHA256, bindInterfaceDatabase, exportBorrowCallback0 } from './node_modules/@rutis/protocol/generated/rpc.js'
import { bindNative, handle } from './node_modules/@rutis/protocol/src/sdk.js'
console.log(JSON.stringify({ loaded: 'node-consumer' }))
const bundles = new Bundles([readFileSync(new URL('./rpc.json', import.meta.url))])
export const protocolPorts = new NativePorts()
protocolPorts.require('rpc', 'nativeDatabase', { interface: 'Database', version: '1.0.0', bundle_sha256: BUNDLE_SHA256 }, bundles, bindInterfaceDatabase)
export default {
  inject: ['nativeDatabase'],
  async apply(ctx, config) {
    ctx.effect(() => () => console.log(JSON.stringify({ cleaned: config.label })))
    const database = ctx.get('nativeDatabase')
    const first = await database.connect({ name: 'first' })
    assert.equal(first, await database.connect({ name: 'second' }))
    assert.equal(first.session, first.session.agent.session)
    assert.equal(await database.inspect(first), true)
    const callbacks = []
    await ctx.plugin({ async apply(child) {
      assert.notEqual(child, ctx)
      const callback = { async call(context, text) {
        assert.equal(context.native(), child)
        assert.equal(context.native().get('nativeDatabase'), child.get('nativeDatabase'))
        callbacks.push(text)
        await first.query({ sql: text })
        return null
      } }
      bindNative(handle(database).caller, child, exportBorrowCallback0(callback))
      await database.withCallback(callback)
    } })
    assert.deepEqual(callbacks, ['root', 'child'])
    console.log(JSON.stringify({ exercised: config.label, owner: (await first.query({ sql: 'ordinary' }))[0].owner, callbacks: callbacks.length }))
  },
}
