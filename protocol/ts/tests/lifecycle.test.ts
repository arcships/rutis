import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { Duplex } from 'node:stream'
import { test } from 'node:test'
import { Context } from '@deepseek-ai/cordis'
import { Peer } from '../src/frame.ts'
import { ProtocolError } from '../src/error.ts'
import { ActivationGate, ManagedActivation } from '../src/managed.ts'
import { CORDIS_VERSION, FAMILY, VERSION, NativeModuleDriver, Runner, parseHello, parseNodeCatalog, type Driver, type Hello, type MountRequest, type Mounted } from '../src/lifecycle.ts'
import { Bundles, NativePorts } from '../src/services.ts'
import type { Activation } from '../src/imports.ts'

const digest = (value: string) => createHash('sha256').update(value).digest('hex')
const schema = '{"type":"object","properties":{"label":{"type":"string"}},"required":["label"],"additionalProperties":false}\n'
const declaration = () => ({ config_sha256: digest(schema), provides: {}, requires: {} })
function plan(): Hello {
  return {
    protocol_family: FAMILY, protocol_version: VERSION,
    identity: { runtime: 'node', epoch: '1', kind: 'node-cordis', framework_version: CORDIS_VERSION, environment_sha256: digest('env'), code_sha256: digest('code'), capabilities: [] },
    members: Object.fromEntries(['slow', 'fast'].map(name => [name, {
      entry: { kind: 'node', entry: '/snapshot/plugin.mjs' }, config: { label: name }, config_schema: schema, contracts: declaration(),
    }])),
  }
}
const id = (n: string): Activation => ({ runtime: 'node', epoch: '1', activation: n })
const select = (n: string) => ({ activation: id(n) })
function deferred() {
  let resolve!: () => void
  const promise = new Promise<void>(yes => { resolve = yes })
  return { promise, resolve }
}
const code = (expected: string) => (error: any) => error.code === expected
class Probe implements Driver {
  readonly contexts = new Map<string, Context>()
  readonly mounts: { instance: string; activation: Activation }[] = []
  readonly cleaned: string[] = []
  readonly entered = deferred()
  readonly load = deferred()
  readonly cleanupEntered = deferred()
  readonly cleanup = deferred()
  slowLoad = false
  slowCleanup = false
  badServices = false
  constructor(readonly root: Context) {}
  admit(): void {}
  mount({ member, activation, instance }: MountRequest, admission: ActivationGate): Mounted {
    const label = (member.config as { label: string }).label
    this.mounts.push({ instance, activation })
    const driver = this
    const native = new ManagedActivation(this.root, {
      async apply(ctx) {
        driver.contexts.set(label, ctx)
        ctx.effect(() => async () => {
          if (driver.slowCleanup) { driver.cleanupEntered.resolve(); await driver.cleanup.promise }
          driver.cleaned.push(label)
        })
        if (label === 'slow' && driver.slowLoad) { driver.entered.resolve(); await driver.load.promise }
      },
    }, {}, {}, undefined, [], admission)
    return { native, services: () => Promise.resolve(this.badServices ? { unexpected: {} } : {}) }
  }
}
class Wire extends Duplex {
  other!: Wire
  _read(): void {}
  _write(bytes: Buffer, _: string, done: (error?: Error | null) => void): void { this.other.push(Buffer.from(bytes)); done() }
  _destroy(error: Error | null, done: (error?: Error | null) => void): void { this.other.push(null); done(error) }
}
function session(driver: Driver) {
  const a = new Wire(); const b = new Wire(); a.other = b; b.other = a
  const runner = new Runner(driver)
  const client = new Peer(a, async () => null)
  const server = new Peer(b, driver.objectSession?.handler(runner) ?? runner.handle)
  driver.objectSession?.attach(server); runner.attach(server)
  return { runner, client, server }
}
const closed = () => new ProtocolError('Unavailable', 'test', 'closed')

test('native members start independently and remain unpublished until activate', async () => {
  const driver = new Probe(new Context()); driver.slowLoad = true
  const { runner, client, server } = session(driver)
  try {
    await client.request('runtime/hello', plan())
    assert.equal(driver.contexts.size, 0, 'hello does not run business code')
    const slow = client.request('plugin/start', { instance: 'slow', activation: id('1') })
    const slowRejected = assert.rejects(slow, code('Cancelled'))
    await driver.entered.promise
    await client.request('plugin/start', { instance: 'fast', activation: id('2') })
    assert.deepEqual(driver.mounts.find(value => value.instance === 'fast')!.activation, id('2'))
    assert.throws(() => runner.requirePublished(id('2')), code('Unavailable'))
    await client.request('plugin/activate', select('2')); runner.requirePublished(id('2'))
    assert.notEqual(driver.contexts.get('slow'), driver.contexts.get('fast'))
    const stopped = client.request('plugin/stop', select('1'))
    // Receiving the state request is a causal barrier after stop admission.
    const state: any = await client.request('plugin/state', select('1'))
    assert.equal(state.phase, 'closing')
    assert.throws(() => driver.contexts.get('slow')!.effect(() => () => {}), /inactive context/)
    driver.load.resolve(); await slowRejected; await stopped
    runner.requirePublished(id('2'))
    await assert.rejects(client.request('plugin/start', { instance: 'slow', activation: id('1') }), code('Unavailable'))
    await assert.rejects(client.request('plugin/activate', select('1')), code('Unavailable'))
    await client.request('runtime/stop', {})
    assert.deepEqual(driver.cleaned.sort(), ['fast', 'slow'])
  } finally { driver.load.resolve(); client.close(closed()); server.close(closed()); await runner.stopped(); await driver.root.fiber.dispose() }
})

test('stop joins cleanup independently of its waiter and replacement stays blocked', async () => {
  const driver = new Probe(new Context()); driver.slowCleanup = true
  const { runner, client, server } = session(driver)
  try {
    await client.request('runtime/hello', plan())
    await client.request('plugin/start', { instance: 'fast', activation: id('1') })
    await client.request('plugin/activate', select('1'))
    void client.request('plugin/stop', select('1')).catch(() => {})
    await driver.cleanupEntered.promise
    assert.throws(() => runner.requirePublished(id('1')), code('Unavailable'))
    await assert.rejects(client.request('plugin/start', { instance: 'fast', activation: id('2') }), code('Unavailable'))
    const cleanup = client.request('plugin/stop', select('1'))
    driver.cleanup.resolve(); await cleanup
    await client.request('plugin/start', { instance: 'fast', activation: id('2') })
    assert.equal((await client.request('plugin/state', select('2')) as any).phase, 'staged')
    await client.request('runtime/stop', {})
    assert.deepEqual(driver.cleaned, ['fast', 'fast'])
  } finally { driver.cleanup.resolve(); client.close(closed()); server.close(closed()); await runner.stopped(); await driver.root.fiber.dispose() }
})

for (const configured of [false, true]) test(`stop during module loading prevents late native apply with service transport ${configured}`, async () => {
  const root = new Context(); const entered = deferred(); const imported = deferred(); let applies = 0; let imports = 0
  const driver = new NativeModuleDriver(root, digest('env'), digest('code'), [{
    entry: '/snapshot/plugin.mjs', contracts: declaration(),
    async load() { imports++; entered.resolve(); await imported.promise; return { default: { apply() { applies++ } } } },
  }], configured ? new Bundles([]) : undefined)
  const { runner, client, server } = session(driver)
  try {
    await client.request('runtime/hello', plan()); assert.equal(imports, 0)
    const start = client.request('plugin/start', { instance: 'slow', activation: id('1') })
    const rejected = assert.rejects(start, code('Cancelled'))
    await entered.promise
    const cleanup = client.request('plugin/stop', select('1'))
    assert.equal((await client.request('plugin/state', select('1')) as any).phase, 'closing')
    imported.resolve(); await rejected; await cleanup
    assert.equal(applies, 0)
    await client.request('plugin/start', { instance: 'slow', activation: id('2') })
    assert.equal(applies, 1)
    await client.request('runtime/stop', {})
  } finally { imported.resolve(); client.close(closed()); server.close(closed()); await runner.stopped(); await root.fiber.dispose() }
})

test('service driver rejects missing exact bundle and mismatched ports before native apply', async () => {
  const raw = readFileSync(new URL('../../fixtures/rpc.bundle.json', import.meta.url))
  const rpc = { interface: 'Database', version: '1.0.0', bundle_sha256: digest(raw.toString()) }
  const root = new Context(); let imports = 0; let applies = 0
  const contracts = { ...declaration(), provides: { rpc } }
  const modules = [{ entry: '/snapshot/plugin.mjs', contracts, async load() { imports++; return { protocolPorts: new NativePorts(), default: { apply() { applies++ } } } } }]
  const requested = plan(); requested.identity.capabilities = ['object.scope', 'callback.borrow']
  for (const member of Object.values(requested.members)) member.contracts = contracts
  const missing = new Runner(new NativeModuleDriver(root, digest('env'), digest('code'), modules, new Bundles([])))
  await assert.rejects(missing.handle('runtime/hello', requested), code('InterfaceMismatch')); assert.equal(imports, 0)
  await missing.stopped()
  const driver = new NativeModuleDriver(root, digest('env'), digest('code'), modules, new Bundles([raw]))
  const { runner, client, server } = session(driver)
  try {
    const events = structuredClone(requested); events.identity.capabilities.push('event.serial')
    await assert.rejects(client.request('runtime/hello', events), code('UnsupportedCapability')); assert.equal(imports, 0)
    await client.request('runtime/hello', requested); assert.equal(imports, 0)
    await assert.rejects(client.request('plugin/start', { instance: 'fast', activation: id('1') }), code('InterfaceMismatch'))
    assert.equal(imports, 1); assert.equal(applies, 0)
    await client.request('runtime/stop', {})
  } finally { client.close(closed()); server.close(closed()); await runner.stopped(); await root.fiber.dispose() }
})

test('frozen Node catalog retains exact raw bundle bytes and checks their digest before imports', () => {
  const raw = readFileSync(new URL('../../fixtures/rpc.bundle.json', import.meta.url), 'utf8')
  const catalog = { protocol_family: FAMILY, protocol_version: VERSION, framework_version: CORDIS_VERSION, environment_sha256: digest('env'), code_sha256: digest('code'), modules: { '/snapshot/plugin.mjs': declaration() }, bundles: { [digest(raw)]: raw } }
  const parsed = parseNodeCatalog(catalog)
  assert.equal(new Bundles(Object.values(parsed.bundles!).map(raw => Buffer.from(raw))).exact(digest(raw)).sha256, digest(raw))
  catalog.bundles[digest(raw)] += '\n'
  assert.throws(() => parseNodeCatalog(catalog), code('InterfaceMismatch'))
})

test('peer close synchronously closes native contexts before cleanup awaits', async () => {
  const driver = new Probe(new Context()); driver.slowCleanup = true
  const { runner, client, server } = session(driver)
  try {
    await client.request('runtime/hello', plan())
    await client.request('plugin/start', { instance: 'fast', activation: id('1') })
    await client.request('plugin/activate', select('1'))
    server.close(closed())
    assert.throws(() => runner.requirePublished(id('1')), code('Unavailable'))
    assert.throws(() => driver.contexts.get('fast')!.provide('late', {}), /inactive context/)
    await driver.cleanupEntered.promise
    let joined = false; const cleanup = runner.stopped().then(() => { joined = true })
    await Promise.resolve(); assert.equal(joined, false)
    driver.cleanup.resolve(); await cleanup
    assert.equal(joined, true)
  } finally { driver.cleanup.resolve(); client.close(closed()); server.close(closed()); await runner.stopped(); await driver.root.fiber.dispose() }
})

test('staging mismatch rolls back real native effects and hello cannot rebind', async () => {
  const driver = new Probe(new Context()); driver.badServices = true
  const runner = new Runner(driver)
  try {
    await runner.handle('runtime/hello', plan())
    await assert.rejects(runner.handle('plugin/start', { instance: 'fast', activation: id('1') }), code('InterfaceMismatch'))
    await runner.handle('plugin/stop', select('1'))
    assert.deepEqual(driver.cleaned, ['fast'])
    assert.throws(() => driver.contexts.get('fast')!.effect(() => () => {}), /inactive context/)
    await assert.rejects(runner.handle('runtime/hello', plan()), code('Unavailable'))
  } finally { await runner.stopped(); await driver.root.fiber.dispose() }
})

test('declarations are immutable and invalid hello or activation never imports a module', async () => {
  const root = new Context(); let imports = 0
  const driver = new NativeModuleDriver(root, digest('env'), digest('code'), [{ entry: '/snapshot/plugin.mjs', contracts: declaration(), async load() { imports++; return { default: { apply() {} } } } }])
  const runner = new Runner(driver)
  try {
    const unsupported = plan(); unsupported.identity.capabilities = ['object.scope']
    await assert.rejects(runner.handle('runtime/hello', unsupported), code('UnsupportedCapability'))
    const duplicate = plan(); duplicate.identity.capabilities = ['object.scope', 'object.scope']
    await assert.rejects(runner.handle('runtime/hello', duplicate), code('InvalidParams'))
    const badSchema = plan(); badSchema.members.fast.config_schema += ' '
    await assert.rejects(runner.handle('runtime/hello', badSchema), code('InterfaceMismatch'))
    const original = plan(); const frozen = parseHello(original)
    assert.throws(() => { (frozen.members.fast.config as any).label = 'mutation' }, TypeError)
    await runner.handle('runtime/hello', original)
    original.members.fast.config = { label: 'invalidated caller copy' }
    await assert.rejects(runner.handle('plugin/start', { instance: 'fast', activation: { ...id('1'), epoch: '2' } }), code('Unavailable'))
    await assert.rejects(runner.handle('plugin/start', { instance: 'unknown', activation: id('1') }), code('InvalidParams'))
    assert.equal(imports, 0)
  } finally { await runner.stopped(); await root.fiber.dispose() }
})

test('independent member ids can arrive out of order but stale ids cannot rebind', async () => {
  const driver = new Probe(new Context()); const runner = new Runner(driver)
  try {
    await runner.handle('runtime/hello', plan())
    await runner.handle('plugin/start', { instance: 'fast', activation: id('2') })
    await runner.handle('plugin/start', { instance: 'slow', activation: id('1') })
    await runner.handle('plugin/stop', select('2'))
    await assert.rejects(runner.handle('plugin/start', { instance: 'fast', activation: id('1') }), code('Unavailable'))
    await assert.rejects(runner.handle('plugin/start', { instance: 'fast', activation: id('2') }), code('Unavailable'))
    await runner.handle('plugin/start', { instance: 'fast', activation: id('3') })
    await runner.handle('runtime/stop', {})
    assert.equal(driver.cleaned.length, 3)
  } finally { await runner.stopped(); await driver.root.fiber.dispose() }
})

test('shared lifecycle hello corpus has the same strict admission on both SDKs', () => {
  const cases = JSON.parse(readFileSync(new URL('../../fixtures/lifecycle-corpus.json', import.meta.url), 'utf8'))
  for (const item of cases) {
    if (item.error) assert.throws(() => parseHello(item.hello), code(item.error), item.name)
    else assert.equal(parseHello(item.hello).identity.runtime, 'group')
  }
})

test('native cleanup diagnostics prevent a false stop confirmation and replacement', async () => {
  const root = new Context(); const cleanupRan: string[] = []
  const driver: Driver = {
    admit() {},
    mount({ member }, admission) {
      const native = new ManagedActivation(root, {
        apply(ctx) {
          ctx.plugin({ apply(child) {
            child.effect(() => () => { cleanupRan.push('child'); throw new Error('child cleanup failed') })
          } })
          ctx.effect(() => () => { cleanupRan.push('parent') })
        },
      }, member.config, {}, undefined, [], admission)
      return { native, services: async () => ({}) }
    },
  }
  const runner = new Runner(driver)
  await runner.handle('runtime/hello', plan())
  await runner.handle('plugin/start', { instance: 'fast', activation: id('1') })
  await runner.handle('plugin/activate', select('1'))
  await assert.rejects(runner.handle('plugin/stop', select('1')), (error: any) => error.code === 'Business' && error.message.includes('child cleanup failed'))
  assert.equal((await runner.handle('plugin/state', select('1')) as any).phase, 'closing')
  assert.deepEqual(cleanupRan.sort(), ['child', 'parent'])
  await assert.rejects(runner.handle('plugin/start', { instance: 'fast', activation: id('2') }), code('Unavailable'))
  await assert.rejects(runner.handle('runtime/stop', {}), code('Business'))
  await assert.rejects(runner.stopped(), code('Business'))
  await root.fiber.dispose()
})
