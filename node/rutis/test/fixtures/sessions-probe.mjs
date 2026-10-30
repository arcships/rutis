// A plugin using the host's own service `sessions` (#986).
import { definePlugin } from '../../src/index.mjs'

export default definePlugin({
  name: 'probe',
  inject: ['sessions'],
  provides: { probe: { go: 'async' } },
  apply(ctx) { ctx.provide('probe', { go: async () => ctx.use('sessions').x() }) },
})
