// The plugin SDK (`@arcships/rutis`, from this checkout) works under
// `bun test`: plugins written with it are tested without a host.
import { expect, test } from 'bun:test'
import { definePlugin } from '../../../node/rutis/src/index.mjs'
import { load } from '../../../node/rutis/src/testing.mjs'

const greeter = definePlugin({
  inject: ['clock'],
  provides: { greeter: { hello: 'sync' } },
  apply(ctx: any, config: any) {
    const clock = ctx.use('clock')
    ctx.provide('greeter', { hello: (name: string) => `${config.greeting}, ${name} at ${clock.now()}` })
  },
})

test('a plugin runs with the SDK testing helpers', async () => {
  const t = await load(greeter, { config: { greeting: 'Hi' }, services: { clock: { now: () => 7 } } })
  expect(t.service('greeter').hello('Ada')).toBe('Hi, Ada at 7')
  await t.unload()
})
