import { Context, type Fiber, type Plugin } from '@deepseek-ai/cordis'

// Cordis 4.0.1 declares FiberState as a const enum and exports no runtime
// enum. These values are pinned to that package and exercised by M0 tests.
export const NativeState = { PENDING: 0, LOADING: 1, ACTIVE: 2, FAILED: 3, DISPOSED: 4, UNLOADING: 5 } as const

export type RevocationReason = 'stop' | 'native-invalidated' | 'necessary-export-lost'

/** Each host activation creates a fresh native Cordis fiber. Public synchronous
 * lifecycle hooks permanently dispose it before Cordis can reload user code.
 * No monkey patching, alternate plugin kernel, or patched Cordis package. */
export class ManagedActivation<C = unknown> {
  private fiber?: Fiber
  private entered = false
  private closed = false
  private stopping = false
  private cleanup?: Promise<void>
  private removeHooks: (() => unknown)[] = []
  private necessary = new Set<string>()
  private readonly scoped: Context

  constructor(
    parent: Context,
    plugin: Plugin.Object<C>,
    config: C,
    dependencies: Record<string, object> = {},
    private readonly revoked: (reason: RevocationReason) => void = () => {},
    exports: string[] = [],
  ) {
    let scoped = parent
    for (const name of exports) scoped = scoped.isolate(name)
    // Dependencies are installed into activation-specific scopes, never the
    // runtime's global service slots. The parent owns their native effects.
    for (const [name, value] of Object.entries(dependencies)) {
      scoped = scoped.isolate(name)
      this.removeHooks.push(scoped.provide(name, value))
    }
    this.scoped = scoped
    const apply = (ctx: Context, value: C) => {
      if (this.closed || this.entered) throw new Error('activation cannot be reused')
      this.entered = true
      return plugin.apply(ctx, value)
    }
    this.removeHooks.push(parent.on('internal/plugin', (fiber) => {
      if (fiber.runtime?.callback === apply && fiber.uid !== null) this.fiber = fiber
    }, { global: true }))
    this.removeHooks.push(parent.on('internal/status', (fiber) => {
      if (fiber !== this.fiber) return
      if ((fiber.state === NativeState.UNLOADING || fiber.state === NativeState.FAILED || fiber.state === NativeState.DISPOSED)) {
        this.invalidate('native-invalidated')
      }
    }, { global: true, prepend: true }))
    const activation = this
    this.removeHooks.push(parent.on('internal/service', function (name, value) {
      if (!activation.entered || activation.closed) return
      if (this[Context.isolate][name] !== activation.scoped[Context.isolate][name]) return
      const required = Array.isArray(plugin.inject)
        ? plugin.inject.includes(name) : Object.hasOwn(plugin.inject ?? {}, name)
      const expected = activation.fiber?.store?.[name]
      const current = activation.scoped.reflect.store[activation.scoped[Context.isolate][name]]
      if (required && (expected !== current || current?.fiber.state !== NativeState.ACTIVE)) {
        activation.invalidate('native-invalidated')
      } else if (activation.necessary.has(name) && value === undefined) {
        activation.invalidate('necessary-export-lost')
      }
    }, { global: true, prepend: true }))
    try {
      const registered = scoped.plugin({ ...plugin, apply }, config)
      // ctx.plugin() returns a thenable wrapper inheriting from the actual
      // Fiber. Notifications carry the actual Fiber; keep the one observed
      // during internal/plugin rather than comparing it to that wrapper.
      if (!this.fiber) {
        void registered.dispose()
        throw new Error('Cordis internal/plugin did not publish the native fiber')
      }
    } catch (error) {
      for (const remove of this.removeHooks.splice(0).reverse()) remove()
      throw error
    }
  }

  get native(): Fiber { return this.fiber! }
  get isOpen(): boolean { return !this.closed }

  /** Export routes are registered by the SDK, including services owned by
   * internal child fibers. Losing a necessary export invalidates the root. */
  requireExport(name: string): void { this.necessary.add(name) }

  private invalidate(reason: RevocationReason): void {
    if (this.closed) return
    this.closed = true
    // dispose clears uid synchronously, so old contexts immediately reject
    // effects, listeners, providers and child plugins, including while Loading.
    this.beginStop()
    this.revoked(reason)
  }

  private beginStop(): Promise<void> {
    if (this.stopping) return this.cleanup ?? Promise.resolve()
    this.stopping = true
    const disposed = this.fiber?.dispose()
    this.cleanup = Promise.resolve(disposed).then(async () => {
      for (const remove of this.removeHooks.splice(0).reverse()) await remove()
    })
    // The owner still joins cleanup via stop(); automatic invalidation must
    // not produce an unhandled rejection on the synchronous notification stack.
    void this.cleanup.catch(() => {})
    return this.cleanup
  }

  stop(): Promise<void> {
    this.invalidate('stop')
    return this.beginStop()
  }

  async ready(): Promise<void> {
    await this.native.await()
    if (this.closed || this.native.state !== NativeState.ACTIVE) {
      throw new Error('activation not ready')
    }
  }
}
