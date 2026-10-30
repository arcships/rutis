// A plugin that injects `rutis/sessions`, a service the host provides itself
// (../testing-taken.test.mjs).
import { definePlugin } from '../../src/index.mjs'

export default definePlugin({
  name: 'taken',
  inject: ['rutis/sessions'],
  provides: { probe: { go: 'async' } },
  apply(ctx) {
    ctx.provide('probe', { go: async () => ctx.use('rutis/sessions').x() })
  },
})
