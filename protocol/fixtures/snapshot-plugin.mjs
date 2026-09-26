// Conformance fixture for canonical dependency modules and native lifecycle.
export { Context } from '@deepseek-ai/cordis'
export { ManagedActivation } from '@rutis/protocol/managed'
export { ManagedActivation as AliasedManaged } from '@rutis/protocol/managedAlias'
export const instances = []
export const cleaned = []
export const plugin = {
  apply(ctx, config) {
    instances.push(ctx)
    ctx.provide('result', { label: config.label })
    return () => { cleaned.push(config.label) }
  },
}
