// The Bun side of the channel contract (crates/rutis-bridge/tests/bun_channel.rs):
// dials the two Unix sockets given as arguments with this runtime's channel
// and relays every message between them, in order, both ways. When either
// ends, the other is closed, and the process exits.
import { open, type Channel } from '../../src/channel.ts'

const [first, second] = process.argv.slice(2)
let a!: Channel, b!: Channel
const done = () => { a?.close(); b?.close(); setTimeout(() => process.exit(0), 50) }
a = await open(first, { message: text => b.send(text), closed: done })
b = await open(second, { message: text => a.send(text), closed: done })
