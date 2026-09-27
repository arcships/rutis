/** Explicit object adapter for existing dsh-settings owner scopes.
 * Registrations and persistence stay in the native service. Only scopes passed
 * by the application are exported; arbitrary namespaces are not discovered. */
import type { SettingsScope } from '@deepseek-ai/dsh-settings'
import { ProtocolError } from '../../protocol/ts/src/error.ts'
import type { InterfaceSettingsService, InterfaceSettingsSectionService } from '../../protocol/ts/generated/settings.ts'

const sections = new WeakMap<SettingsScope<Record<string, unknown>>, InterfaceSettingsSectionService>()
export function settingsSection(namespace: string, scope: SettingsScope<Record<string, unknown>>): InterfaceSettingsSectionService {
  const previous = sections.get(scope)
  if (previous) {
    if (previous.namespace !== namespace) throw new ProtocolError('InterfaceMismatch', 'settings', 'native scope has another namespace')
    return previous
  }
  const section: InterfaceSettingsSectionService = {
      namespace,
      async read() { return structuredClone(scope.get()) },
      async write(_context, patch) { await scope.update(patch); return null },
      async replace(_context, section) { await scope.replace(section); return null },
      async visit(_context, callback) { return callback.call(structuredClone(scope.get())) },
  }
  sections.set(scope, section)
  return section
}

export function settingsObjects(scopes: ReadonlyMap<string, SettingsScope<Record<string, unknown>>>): InterfaceSettingsService {
  const exported = new Map([...scopes].map(([namespace, scope]) => [namespace, settingsSection(namespace, scope)]))
  return {
    async open(_context, namespace) {
      const section = exported.get(namespace)
      if (!section) throw new ProtocolError('CapabilityDenied', 'settings', 'namespace is not exported')
      return section
    },
    // Preserve the owner-returned facade: this operation still goes through its
    // authorized interface, using the exact original SettingsScope instance.
    async inspect(_context, section) { return section.read(null) },
  }
}
