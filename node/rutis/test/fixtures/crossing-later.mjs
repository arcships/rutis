// A plugin that calls a service of the test while the test, after an
// `await`, calls a function the plugin handed it (#987): `withLock` calls
// the plugin's work, `other` is called meanwhile, from `apply`
// (../testing-crossing.test.mjs).
import { definePlugin } from '../../src/index.mjs'

export default definePlugin({
  name: 'crossing-later',
  inject: ['files'],
  provides: { probe: { go: 'async' } },
  async apply(ctx) {
    const files = ctx.use('files')
    const locked = files.withLock(async () => 'worked')
    const other = await files.other()
    ctx.provide('probe', { go: async () => [await locked, other] })
  },
})
