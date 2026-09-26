import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { Bundles, NativePorts } from './node_modules/@rutis/protocol/src/services.js'
import { BUNDLE_SHA256, exportInterfaceDatabase } from './node_modules/@rutis/protocol/generated/rpc.js'
console.log(JSON.stringify({ loaded: 'node-provider' }))
const bundles = new Bundles([readFileSync(new URL('./rpc.json', import.meta.url))])
export const protocolPorts = new NativePorts()
protocolPorts.provide('rpc', 'nativeDatabase', { interface: 'Database', version: '1.0.0', bundle_sha256: BUNDLE_SHA256 }, bundles, exportInterfaceDatabase)
export default {
  apply(ctx) {
    ctx.effect(() => () => console.log(JSON.stringify({ cleaned: 'node-provider' })))
    const session = {}; const agent = { session }; session.agent = agent
    const connection = { session, async query(context, { sql }) {
      assert.equal(context.native().get('nativeDatabase'), ctx.get('nativeDatabase'))
      return [{ owner: 'node', sql }]
    } }
    ctx.provide('nativeDatabase', {
      async connect() { return connection },
      async inspect(_, value) { return (await value.query({ sql: 'passback' }))[0].owner === 'node' },
      async withCallback(context, callback) {
        await callback.call('root')
        context.spawn(() => callback.call('child'))
        return null
      },
    })
  },
}
