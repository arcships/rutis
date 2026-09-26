/** Compiled launch entry. The embedding package pins this runner and SDK in
 * its admitted inventory; Host passes its generated frozen catalog as argv[2]. */
import { readFile } from 'node:fs/promises'
import { isAbsolute } from 'node:path'
import { Socket } from 'node:net'
import { Context } from '@deepseek-ai/cordis'
import { decodeJson } from './json.ts'
import { NativeModuleDriver, nodeModule, parseNodeCatalog, serve } from './lifecycle.ts'

if (process.argv.length !== 3 || !isAbsolute(process.argv[2])) throw new Error('expected an absolute frozen Node catalog path')
const catalog = parseNodeCatalog(decodeJson(await readFile(process.argv[2])))
const root = new Context()
const driver = new NativeModuleDriver(root, catalog.environment_sha256, catalog.code_sha256,
  Object.entries(catalog.modules).map(([entry, contracts]) => nodeModule(entry, contracts)))
try { await serve(new Socket({ fd: 3, readable: true, writable: true }), driver) }
finally { await root.fiber.dispose() }
