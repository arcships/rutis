// Runtime fixture for the generic packaged Node entry; stdout is diagnostics.
import { once } from 'node:events'
console.log(JSON.stringify({ loaded: import.meta.url }))
export default {
  async apply(ctx, config) {
    console.log(JSON.stringify({ apply: config.label, ctx: ctx.fiber.uid }))
    ctx.effect(() => () => { console.log(JSON.stringify({ cleaned: config.label })) })
    if (config.label === 'slow') {
      await once(process.stdin, 'data')
      process.stdin.pause()
      let rejected = false
      try { ctx.effect(() => () => {}) } catch { rejected = true }
      console.log(JSON.stringify({ late_rejected: rejected }))
    }
  },
}
