import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { admit, callbackKey, validateWire, ProtocolError, type TypeExpr } from '../src/contract.ts'
import { decodeJson, MAX_JSON_BYTES } from '../src/json.ts'

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
test('T22: shared strict JSON and descriptor corpora', () => {
  const corpus = JSON.parse(readFileSync(new URL('../../fixtures/json-corpus.json', import.meta.url), 'utf8'))
  for (const entry of corpus) {
    let error: string | null = null
    try { decodeJson(Buffer.from(entry.hex, 'hex')) }
    catch (caught) { assert.ok(caught instanceof ProtocolError); error = caught.code }
    assert.equal(error, entry.error, entry.name)
  }
  assert.throws(() => decodeJson(Buffer.alloc(MAX_JSON_BYTES + 1)), ProtocolError)
  const descriptors = JSON.parse(readFileSync(new URL('../../fixtures/descriptor-corpus.json', import.meta.url), 'utf8'))
  for (const entry of descriptors) {
    const bundle = JSON.parse(bundleBytes.toString())
    if (entry.path) {
      let parent = bundle
      for (const part of entry.path.slice(0, -1)) parent = parent[part]
      parent[entry.path.at(-1)] = entry.value
    }
    let error: string | null = null
    try { admit(Buffer.from(entry.raw ?? JSON.stringify(bundle))) }
    catch (caught) { assert.ok(caught instanceof ProtocolError); error = caught.code }
    assert.equal(error, entry.error, entry.name)
  }
})
test('callback signature matches across numeric spellings and Unicode key ordering', () => {
  const corpus = JSON.parse(readFileSync(new URL('../../fixtures/callback-corpus.json', import.meta.url), 'utf8'))
  for (const entry of corpus) assert.equal(callbackKey(entry.type), entry.fingerprint, entry.name)
})
test('object graph descriptor admits cyclic interface relationships and binds raw bytes', () => {
  const admitted = admit(bundleBytes)
  assert.equal(Object.keys(admitted.bundle.interfaces).length, 4)
  assert.notEqual(admitted.sha256, admit(Buffer.concat([bundleBytes, Buffer.from(' ')])).sha256)
  assert.throws(() => { admitted.bundle.interfaces.Database.methods.connect.params.kind = 'record' }, TypeError)
  assert.throws(() => { (admitted as any).sha256 = 'changed' }, TypeError)
  const params = admitted.bundle.interfaces.Database.methods.connect.params
  assert.equal(params.kind, 'value')
  if (params.kind === 'value') assert.ok(Object.isFrozen(params.schema))
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
