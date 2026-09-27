import { Socket } from 'node:net'
import { readFileSync } from 'node:fs'
import { Context } from '@deepseek-ai/cordis'
import { SettingsProvider, settingsNamespace, type SettingsNamespace } from '@deepseek-ai/dsh-settings'
import z from '@deepseek-ai/schemastery'
import { settingsObjects, settingsSection } from '../../src/protocol-settings.ts'
import { onProtocolEvent, emitProtocolEvent } from '../../src/protocol-events.ts'
import { Peer } from '../../../protocol/ts/src/frame.ts'
import { Exports } from '../../../protocol/ts/src/exports.ts'
import { ActivationGate } from '../../../protocol/ts/src/managed.ts'
import { Bundles, NativePorts, type NativeServices } from '../../../protocol/ts/src/services.ts'
import { RuntimeObjects, root as memberRoot } from '../../../protocol/ts/src/session.ts'
import { object } from '../../../protocol/ts/src/sdk.ts'
import { canonical, keys } from '../../../protocol/ts/src/contract.ts'
import type { WireGraph } from '../../../protocol/ts/src/graph.ts'
import * as api from '../../../protocol/ts/generated/settings.ts'

// Only storage is supplied by the fixture. Namespace resolution, validation,
// update serialization, registrations and teardown use the published service.
class MemorySettings extends SettingsProvider {
  readonly writable = true
  protected async load() { return {} }
  protected async persist(_ns: SettingsNamespace, _section: Record<string, unknown>) {}
}
const root = new Context()
const provider = root.plugin(MemorySettings)
await provider
const owner = { runtime: 'settings', epoch: '1', activation: '1' }
const bundles = new Bundles([readFileSync(new URL('../../../protocol/fixtures/settings.bundle.json', import.meta.url))])
const runtime = new RuntimeObjects({ runtime: owner.runtime, epoch: owner.epoch }, bundles)
const gate = new ActivationGate()
runtime.reserve(owner, gate)
let member: NativeServices<{}> | undefined
let bound: Exports | undefined
let borrowed: api.BorrowCallback0 | undefined
let section: api.InterfaceSettingsSectionService | undefined
let publisher: api.InterfaceEventPublisher | undefined
let original: Context | undefined
let cleanups = 0
const ports = new NativePorts()
const contract = { interface: 'Settings', version: '1.0.0', bundle_sha256: api.BUNDLE_SHA256 }
const eventContract = { interface: 'EventListener', version: '1.0.0', bundle_sha256: api.BUNDLE_SHA256 }
ports.provide('settings', 'protocolSettings', contract, bundles, api.exportInterfaceSettings)
ports.provide('events', 'protocolEvents', eventContract, bundles, api.exportInterfaceEventListener)
const peer = new Peer(new Socket({ fd: 3, readable: true, writable: true }), runtime.handler(async (method, value) => {
  switch (method) {
    case 'fixture/start': {
      member = await ports.mount(root, { inject: ['settings'], apply(ctx) {
        original = ctx
        const exports = Exports.managed(ctx, owner, runtime.ids, () => gate.isOpen)
        bound = exports
        runtime.bind(owner, ctx, exports)
        const scope = ctx.settings.register(settingsNamespace('protocol-example'), z.object({ count: z.number().min(0).default(2), label: z.string().default('native') }))
        const objects = settingsObjects(new Map([['protocol-example', scope]]))
        section = settingsSection('protocol-example', scope)
        const open = objects.open
        objects.open = async (context, ns) => {
          if (context.native() !== ctx) throw new Error('wrong native settings creator')
          const section = await open(context, ns)
          const visit = section.visit
          section.visit = async (context, callback) => { borrowed = callback; return visit(context, callback) }
          return section
        }
        ctx.effect(() => () => { cleanups++ })
        ctx.provide('protocolSettings', objects)
        const listener = onProtocolEvent(ctx, async payload => {
          const current = await payload.section.read(null)
          if (current.count !== payload.value.count) throw new Error('event object is not the original settings scope')
          return payload.value.result
        })
        ctx.provide('protocolEvents', listener)
      } }, {}, owner, {}, runtime.caller(owner), gate, ['settings'])
      await member.native.native
      const staged = await member.stage(bundles, runtime.ids, bound)
      runtime.stageServices(staged)
      runtime.publish(owner)
      return { table: staged.table, contracts: { settings: contract, events: eventContract } }
    }
    case 'fixture/publisher': {
      const offered = value as { activation: typeof owner; contracts: Record<string, unknown>; graphs: Record<string, WireGraph> }
      const expected = { interface: 'EventPublisher', version: '1.0.0', bundle_sha256: api.BUNDLE_SHA256 }
      if (canonical(offered.activation) !== canonical(owner) || canonical(offered.contracts) !== canonical({ publisher: expected })) throw new Error('publisher grant differs')
      keys(offered.graphs, ['publisher'])
      const scope = memberRoot(owner)
      const endpoint = runtime.imports.receiveGraph(bundles.service(expected), { kind: 'object', interface: 'EventPublisher', ownership: 'scope' }, offered.graphs.publisher, { scope, borrow: scope })
      publisher = api.bindInterfaceEventPublisher(object(endpoint), runtime.caller(owner))
      await runtime.flush()
      return { ready: true }
    }
    case 'fixture/emit': {
      const request = value as { mode: 'parallel' | 'serial'; result: unknown }
      return emitProtocolEvent(original!, publisher!, request.mode, section!, { count: 2, result: request.result })
    }
    case 'fixture/expired': return borrowed!.call({ expired: true })
    case 'fixture/stop': runtime.closeMember(owner); await member!.native.stop(); return { cleanups, registrations: root.settings.describe().length }
    case 'fixture/close': runtime.close(); await member?.native.stop(); await root.fiber.dispose(); return { cleanups }
    default: throw new Error(`unknown settings fixture method ${method}`)
  }
}))
runtime.attach(peer)
