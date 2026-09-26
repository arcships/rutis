import { Context, type Fiber, type Plugin } from '@deepseek-ai/cordis'

// Cordis 4.0.1 declares FiberState as a const enum and exports no runtime
// enum. These values are pinned to that package and exercised by M0 tests.
export const NativeState = { PENDING: 0, LOADING: 1, ACTIVE: 2, FAILED: 3, DISPOSED: 4, UNLOADING: 5 } as const

export type RevocationReason = 'stop' | 'native-invalidated' | 'necessary-export-lost'

/** A synchronous native registration failure still owns rollback work. SDK
 * async mounters join this barrier before returning an error to the Host. */
export class NativeMountError extends Error {
  constructor(cause: unknown, readonly cleanup: Promise<void>) {
    super(cause instanceof Error ? cause.message : String(cause), { cause })
    void cleanup.catch(() => {})
  }
}

/** Reserved before module loading. Stop closes the same admission object that
 * the eventual native mount uses, so a late import cannot start old code. */
export class ActivationGate {
  private closed = false
  private observers = new Set<() => void>()
  private resolve!: () => void
  readonly revoked = new Promise<void>(resolve => { this.resolve = resolve })
  get isOpen(): boolean { return !this.closed }
  onClose(observer: () => void): () => void {
    if (this.closed) observer()
    else this.observers.add(observer)
    return () => { this.observers.delete(observer) }
  }
  close(): void {
    if (this.closed) return
    this.closed = true
    this.resolve()
    const observers = [...this.observers]; this.observers.clear()
    for (const observer of observers) observer()
  }
}

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
  private cleanupErrors: unknown[] = []
  private readonly scoped: Context
  private context?: Context

  constructor(
    parent: Context,
    plugin: Plugin.Object<C>,
    config: C,
    dependencies: Record<string, object> = {},
    private readonly revoked: (reason: RevocationReason) => void = () => {},
    exports: string[] = [],
    readonly gate = new ActivationGate(),
    checks: Record<string, () => boolean> = {},
  ) {
    if (!gate.isOpen) throw new Error('activation was stopped before native mount')
    let scoped = parent
    for (const name of exports) scoped = scoped.isolate(name)
    for (const name of Object.keys(dependencies)) scoped = scoped.isolate(name)
    this.scoped = scoped
    try {
      // Dependencies are installed into activation-specific scopes, never the
      // runtime's global service slots. The parent owns their native effects.
      for (const [name, value] of Object.entries(dependencies)) {
        this.removeHooks.push(scoped.reflect.provide(name, value, checks[name]))
      }
      // Cordis joins native unload work but reports disposer failures through its
      // public logger exporter rather than rejecting Fiber.dispose(). During
      // closing, retain descendant error diagnostics as an unconfirmed cleanup.
      // An author error log during closing is conservatively unconfirmed too.
      this.removeHooks.push(parent.logger.exporter({
        levels: { default: 0 },
        export: message => {
          if (!this.closed || message.type !== 'error' || !this.fiber) return
          let fiber = message.fiber?.deref()
          while (fiber) {
            if (fiber === this.fiber) { this.cleanupErrors.push(...message.args); return }
            const outer: Fiber = fiber.parent.fiber
            if (outer === fiber) return
            fiber = outer
          }
        },
      }))
      const apply = (ctx: Context, value: C) => {
        if (this.closed || this.entered) throw new Error('activation cannot be reused')
        this.entered = true
        this.context = ctx
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
      this.removeHooks.push(gate.onClose(() => this.invalidate('stop')))
      const registered = scoped.plugin({ ...plugin, apply }, config)
      // ctx.plugin() returns a thenable wrapper inheriting from the actual
      // Fiber. Notifications carry the actual Fiber; keep the one observed
      // during internal/plugin rather than comparing it to that wrapper.
      if (!this.fiber) {
        this.removeHooks.push(() => registered.dispose())
        throw new Error('Cordis internal/plugin did not publish the native fiber')
      }
    } catch (error) {
      this.closed = true
      this.gate.close()
      throw new NativeMountError(error, this.beginStop())
    }
  }

  static async mount<C>(...args: ConstructorParameters<typeof ManagedActivation<C>>): Promise<ManagedActivation<C>> {
    try { return new ManagedActivation(...args) }
    catch (error) {
      if (error instanceof NativeMountError) {
        try { await error.cleanup }
        catch (cleanup) { throw new AggregateError([error.cause, cleanup], 'native mount and rollback failed') }
      }
      throw error
    }
  }

  get native(): Fiber { return this.fiber! }
  get isOpen(): boolean { return !this.closed }
  get nativeContext(): Context | undefined { return this.context }

  /** SDK control handlers call this after import revocation, using Cordis's
   * native availability checks and invalidation hooks. */
  refreshDependencies(names: string[]): void { this.scoped.reflect.notify(names) }

  /** Export routes are registered by the SDK, including services owned by
   * internal child fibers. Losing a necessary export invalidates the root. */
  requireExport(name: string): void { this.necessary.add(name) }

  private invalidate(reason: RevocationReason): void {
    if (this.closed) return
    this.closed = true
    this.gate.close()
    // dispose clears uid synchronously, so old contexts immediately reject
    // effects, listeners, providers and child plugins, including while Loading.
    this.beginStop()
    this.revoked(reason)
  }

  private beginStop(): Promise<void> {
    if (this.stopping) return this.cleanup ?? Promise.resolve()
    this.stopping = true
    let resolve!: () => void; let reject!: (reason: unknown) => void
    // Reserve the confirmation before native disposal can synchronously emit
    // a hook that reenters stop(); every waiter joins the same actual cleanup.
    this.cleanup = new Promise<void>((yes, no) => { resolve = yes; reject = no })
    void (async () => {
      try { await this.fiber?.dispose() }
      catch (error) { this.cleanupErrors.push(error) }
      for (const remove of this.removeHooks.splice(0).reverse()) {
        try { await remove() }
        catch (error) { this.cleanupErrors.push(error) }
      }
      if (this.cleanupErrors.length) throw new AggregateError(this.cleanupErrors,
        'native activation cleanup was not confirmed: ' + this.cleanupErrors.map(error => String(error)).join('; '))
    })().then(resolve, reject)
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
