/** One native Cordis listener endpoint selected by the Host event registry.
 * It uses the existing object transport, including grants for payload objects. */
import type { Context } from '@deepseek-ai/cordis'
import type { InterfaceEventListenerService } from '../../protocol/ts/generated/settings.ts'
import { exportInterfaceSettingsSection, type InterfaceSettingsSectionService, type InterfaceEventPublisher } from '../../protocol/ts/generated/settings.ts'
import { handle, bindNative, json, type Outbound } from '../../protocol/ts/src/sdk.ts'
import { ProtocolError } from '../../protocol/ts/src/error.ts'

/** A publisher service grants exactly the Host-selected event and scope.
 * Own payloads retain their original native creator and native object identity. */
export async function emitProtocolEvent(ctx: Context, publisher: InterfaceEventPublisher, mode: 'parallel' | 'serial', section: InterfaceSettingsSectionService, value: Record<string, unknown>): Promise<Awaited<ReturnType<InterfaceEventPublisher['dispatch']>>> {
  const client = handle(publisher)
  const payload: Outbound = { kind: 'record', fields: {
    mode: json(mode),
    payload: { kind: 'record', fields: { section: exportInterfaceSettingsSection(section), value: json(value) } },
  } }
  bindNative(client.caller, ctx, payload)
  return await client.call('dispatch', payload) as Awaited<ReturnType<InterfaceEventPublisher['dispatch']>>
}

/** Cordis treats raw false/null as Continue. Use an explicit decision wrapper
 * on exported listeners so the protocol preserves Return(false)/Return(null).
 * Each endpoint is one Host registration; it never rebroadcasts through a
 * second local listener list. Native effect and export execution own cleanup. */
export function onProtocolEvent(ctx: Context, listener: (payload: Parameters<InterfaceEventListenerService['call']>[1]) => Promise<unknown>): InterfaceEventListenerService {
  let active = true
  ctx.effect(() => () => { active = false })
  return { async call(context, payload) {
    if (!active) throw new ProtocolError('ScopeClosed', 'events', 'native event listener is closed')
    if (context.native() !== ctx) throw new ProtocolError('CapabilityDenied', 'events', 'event endpoint has another native creator')
    const value = await listener(payload)
    return { returned: value !== undefined, value: value ?? null }
  } }
}
