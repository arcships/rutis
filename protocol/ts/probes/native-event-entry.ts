import assert from 'node:assert/strict'
import { Context, getTraceable, type EventsService } from '@deepseek-ai/cordis'

// Feasibility probe only: replace an inherited public event service without
// changing the native plugin or Context/fiber identity. This is not an IPC test.
function eventScope(parent: Context, seen: Context[]): Context {
  const original = parent.events
  const cache = new WeakMap<Context, EventsService>()
  return parent.extend({
    get events() {
      const actual = this as unknown as Context
      let service = cache.get(actual)
      if (!service) {
        const native = getTraceable(actual, original)
        service = new Proxy(native, {
          get(target, property, receiver) {
            const value = Reflect.get(target, property, receiver)
            if (!['on', 'once', 'parallel', 'serial'].includes(String(property))) return value
            return (...args: unknown[]) => {
              seen.push(actual)
              return Reflect.apply(value, target, args)
            }
          },
        })
        cache.set(actual, service)
      }
      return service
    },
  })
}

declare module '@deepseek-ai/cordis' {
  interface Events {
    'probe/serial'(this: { [Context.filter](owner: Context): boolean }): number | false | null
    'probe/once'(): void
  }
}

const observations: unknown[][] = []
let current: unknown[]
let businessContext: Context
const unchangedPlugin = {
  async apply(ctx: Context) {
    businessContext = ctx
    const calls: string[] = []
    ctx.on('probe/serial', (() => { calls.push('false'); return false }))
    ctx.events.on('probe/serial', (() => { calls.push('null'); return null }))
    ctx.on('probe/serial', (() => { calls.push('zero'); return 0 }))
    ctx.once('probe/once', (() => { calls.push('once') }))
    const filter = { [Context.filter](owner: Context) { assert.equal(owner, ctx); return true } }
    const value = await ctx.serial(filter, 'probe/serial')
    await ctx.parallel('probe/once')
    await ctx.events.parallel('probe/once')
    ctx.effect(() => () => { calls.push('cleanup') })
    current = [value, calls]
  },
}

for (const routed of [false, true]) {
  const root = new Context()
  const seen: Context[] = []
  const parent = routed ? eventScope(root, seen) : root
  const fiber = parent.plugin(unchangedPlugin)
  await fiber
  if (routed) {
    assert.ok(seen.includes(businessContext!))
    assert.equal(businessContext!.fiber.uid, fiber.uid)
  }
  await fiber.dispose()
  observations.push(current!)
  await root.fiber.dispose()
}
assert.deepEqual(observations[0], observations[1])
assert.deepEqual(observations[0], [0, ['false', 'null', 'zero', 'once', 'cleanup']])
console.log('Same plugin, native event entrypoints, exact registration context, false/null/0, once and cleanup: equal')
