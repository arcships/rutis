import { ProtocolError } from './error.ts'
import { Peer } from './frame.ts'
import type { Hello, Runner } from './lifecycle.ts'
import { Bundles } from './services.ts'
import { RuntimeObjects } from './session.ts'

/** Private hello supplies the one immutable epoch identity before construction. */
export class ObjectSession {
  private actor?: RuntimeObjects
  private peer?: Peer
  constructor(private readonly bundles: Bundles) {}
  admit(hello: Hello): void {
    if (this.actor) throw new ProtocolError('Unavailable', 'hello', 'object session cannot rebind')
    const objects = new RuntimeObjects({ runtime: hello.identity.runtime, epoch: hello.identity.epoch }, this.bundles)
    if (this.peer) objects.attach(this.peer)
    this.actor = objects
  }
  objects(): RuntimeObjects {
    if (!this.actor) throw new ProtocolError('Unavailable', 'object_session', 'private hello has not completed')
    return this.actor
  }
  attach(peer: Peer): void {
    if (this.peer) throw new ProtocolError('Unavailable', 'object_session', 'private peer cannot rebind')
    this.actor?.attach(peer); this.peer = peer
  }
  handler(runner: Runner): (method: string, value: unknown) => Promise<unknown> {
    return (method, value) => {
      if (this.actor) return this.actor.lifecycleHandler(runner)(method, value)
      if (method.startsWith('object/')) return Promise.reject(new ProtocolError('Unavailable', 'object_session', 'private hello has not completed'))
      return runner.handle(method, value)
    }
  }
}
