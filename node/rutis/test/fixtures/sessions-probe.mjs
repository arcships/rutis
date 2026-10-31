// A plugin using the host's own services `sessions` and `loader` (#986).
import { definePlugin } from '../../src/index.mjs'

export default definePlugin({
  name: 'probe',
  inject: ['sessions', 'loader'],
  provides: { probe: { go: 'async' } },
  apply(ctx) {
    ctx.provide('probe', { go: async () => (await ctx.use('sessions').x()) + (await ctx.use('loader').y()) })
  },
})
