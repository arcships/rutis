// This fixed fixture verifies launch artifacts. It is not the protocol runner:
// RuntimeReady, private IPC and HostActive publication use a separate workflow.
import assert from 'node:assert/strict'
import { pathToFileURL } from 'node:url'
import { Context } from '@deepseek-ai/cordis'
import { ManagedActivation, NativeState } from '@rutis/protocol/managed'
const [firstPath, secondPath] = process.argv.slice(2)
const first = await import(pathToFileURL(firstPath).href)
const second = await import(pathToFileURL(secondPath).href)
assert.notEqual(first, second, 'distinct code packages have distinct module identities')
assert.equal(first.Context, second.Context)
assert.equal(first.Context, Context)
assert.equal(first.ManagedActivation, second.ManagedActivation)
assert.equal(first.ManagedActivation, ManagedActivation)
assert.equal(first.AliasedManaged, ManagedActivation, 'internal dependency symlinks preserve module identity')
assert.equal(second.AliasedManaged, ManagedActivation)
const root = new Context()
const a = new ManagedActivation(root, first.plugin, { label: 'a' }, {}, undefined, ['result'])
const b = new ManagedActivation(root, second.plugin, { label: 'b' }, {}, undefined, ['result'])
await Promise.all([a.ready(), b.ready()])
assert.equal(a.native.state, NativeState.ACTIVE)
assert.equal(b.native.state, NativeState.ACTIVE)
assert.notEqual(first.instances[0], second.instances[0])
assert.equal(first.instances[0].get('result').label, 'a')
assert.equal(second.instances[0].get('result').label, 'b')
assert.equal(root.get('result'), undefined)
const old = first.instances[0]
await a.stop()
assert.equal(b.native.state, NativeState.ACTIVE)
assert.deepEqual(first.cleaned, ['a'])
assert.throws(() => old.provide('late', {}), /inactive context/)
await b.stop()
assert.deepEqual(second.cleaned, ['b'])
await root.fiber.dispose()
console.log(JSON.stringify({ canonical_dependencies: true, isolated_instances: 2, cleaned: ['a', 'b'] }))
