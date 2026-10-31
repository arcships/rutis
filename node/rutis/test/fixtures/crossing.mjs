// A plugin that calls a service of the test while the test calls a
// function the plugin handed it (#987): `withLock` calls the plugin's
// callback, `ping` is called meanwhile, from `apply`
// (../testing-crossing.test.mjs).
import { definePlugin } from '../../src/index.mjs'

export default definePlugin({
  name: 'crossing',
  inject: ['files', 'other'],
  provides: { probe: { go: 'async' } },
  async apply(ctx) {
    const scope = await ctx.use('files').open()
    const locked = scope.withLock(() => 'got')
    const other = await ctx.use('other').ping()
    ctx.provide('probe', { go: async () => [await locked, other] })
  },
})
