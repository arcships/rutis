// A listener, its disposer and one that stays, for
// test/runner-events.test.mjs: a host that picked the withdrawn one for a
// dispatch before its `events.off` arrived still calls it (#1135), and the one
// a row's unload takes is the failure it was.
import { definePlugin } from '../../../rutis/src/index.mjs'

export default definePlugin({
  name: 'events-withdraw',
  apply(ctx) {
    const off = ctx.on('t/x', () => 'x')
    ctx.on('t/keep', () => 'kept')
    ctx.provide('events-withdraw', {
      off() { off() },
    })
  },
})
