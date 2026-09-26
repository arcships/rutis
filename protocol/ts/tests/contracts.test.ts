import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { admit, validateWire, ProtocolError, type TypeExpr } from '../src/contract.ts'

const bundleBytes = readFileSync(new URL('../../fixtures/database.bundle.json', import.meta.url))
test('T22: shared Rust/TS legal and illegal contract corpus', () => {
  const corpus = JSON.parse(readFileSync(new URL('../../fixtures/contract-corpus.json', import.meta.url), 'utf8'))
  for (const entry of corpus) {
    let error: string | null = null
    try { validateWire(entry.type as TypeExpr, entry.value, entry.references) }
    catch (caught) { assert.ok(caught instanceof ProtocolError); error = caught.code }
    assert.equal(error, entry.error, entry.name)
  }
})
test('object graph descriptor admits cyclic interface relationships and binds raw bytes', () => {
  const admitted = admit(bundleBytes)
  assert.equal(Object.keys(admitted.bundle.interfaces).length, 4)
  assert.notEqual(admitted.sha256, admit(Buffer.concat([bundleBytes, Buffer.from(' ')])).sha256)
})
test('unsupported extensions and recursive/external schemas are rejected before plugin code', () => {
  const mutations = [
    (b: any) => { b.required_capabilities = ['object.delegate'] },
    (b: any) => { b.events['session/event'].modes = ['waterfall'] },
    (b: any) => { b.interfaces.Database.methods.withCallback.params.ownership = 'scope' },
    (b: any) => { b.interfaces.Database.methods.connect.result = { kind: 'stream', item: { kind: 'value', schema: { type: 'string' } } } },
    (b: any) => { b.interfaces.Database.methods.connect.params.schema = { type: 'string', pattern: '.*' } },
    (b: any) => { b.interfaces.Database.methods.connect.params.schema = { $ref: 'https://example.invalid/schema' } },
  ]
  for (const mutate of mutations) {
    const bundle = JSON.parse(bundleBytes.toString())
    mutate(bundle)
    assert.throws(() => admit(Buffer.from(JSON.stringify(bundle))), (error: any) => error.code === 'UnsupportedCapability')
  }
  const bundle = JSON.parse(bundleBytes.toString())
  bundle.interfaces.Database.methods.connect.params.schema = { $defs: { X: { $ref: '#/$defs/X' } }, $ref: '#/$defs/X' }
  assert.throws(() => admit(Buffer.from(JSON.stringify(bundle))), (error: any) => error.code === 'InvalidParams')
})
