import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { spawn } from 'node:child_process'
import { Bundles, NativePorts } from './node_modules/@rutis/protocol/src/services.js'
import { withNative } from './node_modules/@rutis/protocol/src/sdk.js'
import { BUNDLE_SHA256, exportInterfaceDatabase } from './node_modules/@rutis/protocol/generated/rpc.js'
console.log(JSON.stringify({ loaded: 'node-provider' }))
const bundles = new Bundles([readFileSync(new URL('./rpc.json', import.meta.url))])
export const protocolPorts = new NativePorts()
const creators = new WeakMap()
protocolPorts.provide('rpc', 'nativeDatabase', { interface: 'Database', version: '1.0.0', bundle_sha256: BUNDLE_SHA256 }, bundles, value => withNative(creators.get(value), exportInterfaceDatabase(value)))
export default {
  async apply(parent) {
    parent.effect(() => () => console.log(JSON.stringify({ cleaned: 'node-provider' })))
    await parent.plugin({ apply(ctx) {
      assert.notEqual(ctx, parent)
      const session = {}; const agent = { session }; session.agent = agent
      const connection = { session, async query(context, { sql }) {
        assert.equal(context.native(), ctx)
        assert.equal(context.native().get('nativeDatabase'), ctx.get('nativeDatabase'))
        if (sql === 'spawn-detached-descendants') {
          const leaf = "process.on('SIGTERM', () => {}); setInterval(() => {}, 1000)"
          const script = `const { spawn } = require('node:child_process'); const child = spawn(process.execPath, ['-e', ${JSON.stringify(leaf)}], { detached: true, stdio: 'ignore' }); child.once('spawn', () => { process.stdout.write(JSON.stringify({ grandchild: child.pid }) + '\\n'); child.unref(); }); process.on('SIGTERM', () => {}); setInterval(() => {}, 1000)`
          const child = spawn(process.execPath, ['-e', script], { detached: true, stdio: ['ignore', 'pipe', 'ignore'] })
          let bytes = ''
          const grandchild = await new Promise((resolve, reject) => {
            child.once('error', reject)
            child.stdout.on('data', chunk => {
              bytes += chunk.toString()
              if (bytes.includes('\n')) resolve(JSON.parse(bytes.trim()).grandchild)
            })
          })
          child.stdout.destroy(); child.unref()
          return [{ owner: 'node', sql, child: child.pid, grandchild }]
        }
        return [{ owner: 'node', sql }]
      } }
      const database = {
        async connect(context) { assert.equal(context.native(), ctx); return connection },
        async inspect(_, value) { return (await value.query({ sql: 'passback' }))[0].owner === 'node' },
        async withCallback(context, callback) {
          await callback.call('root')
          context.spawn(() => callback.call('child'))
          return null
        },
      }
      creators.set(database, ctx)
      ctx.provide('nativeDatabase', database)
    } })
  },
}
