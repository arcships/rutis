// @ts-nocheck: the same plugin as the Node runtime's (node/rutis-runtime/src/testing/service.mjs).
// A service of the test (`@arcships/rutis/testing`), as a row of its own
// that the driver (./rutis-test.ts) loads: `{ name, methods }`, each
// method calling the test's object. A name rutis already has fails this
// row, as it fails any later provider, and so only the `load` that gave
// it, not the driver (#988).
import { PLUGIN_API } from '../plugin.ts'

const LEAF = Symbol.for('rutis.leaf-plugin')
// The driver's session with the test (./rutis-test.ts).
export const CONTROL = Symbol.for('rutis.test.control')

export default Object.freeze({
  [LEAF]: true,
  name: 'rutis-test-service',
  inject: [],
  provides: {},
  announces: true,
  api: PLUGIN_API,
  apply(ctx, { name, methods }) {
    const control = globalThis[CONTROL]
    ctx.provide(name, Object.fromEntries(methods.map(method => [method, (...args) => control.call(name, method, args)])))
  },
})
