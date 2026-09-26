import { createHash } from 'node:crypto'
import { ProtocolError } from './error.ts'
import { decodeJson, scalarUnicode, signatureCanonical } from './json.ts'
export { ProtocolError, type ErrorCode } from './error.ts'
export type Schema = Record<string, any>
export type TypeExpr =
  | { kind: 'value'; schema: Schema }
  | { kind: 'object'; interface: string; ownership: 'scope' | 'borrow' }
  | { kind: 'record'; fields: Record<string, TypeExpr> }
  | { kind: 'list' | 'optional' | 'stream'; item: TypeExpr }
  | { kind: 'callback'; params: TypeExpr; result: TypeExpr; ownership: 'scope' | 'borrow' }
export type WireValue =
  | { kind: 'value'; value: unknown }
  | { kind: 'ref'; index: number }
  | { kind: 'record'; fields: Record<string, WireValue> }
  | { kind: 'list'; items: WireValue[] }
  | { kind: 'optional'; value: WireValue | null }
export interface Method { params: TypeExpr; result: TypeExpr }
export interface Bundle {
  id: string; version: string
  interfaces: Record<string, { methods: Record<string, Method>; properties?: Record<string, TypeExpr> }>
  events?: Record<string, Method & { modes: string[] }>
  required_capabilities?: string[]
}
const invalid = (message: string): never => { throw new ProtocolError('InvalidParams', 'contract', message) }
const unsupported = (message: string): never => { throw new ProtocolError('UnsupportedCapability', 'prepare', message) }
export function identifier(name: string): boolean {
  return /^[A-Za-z_][A-Za-z_0-9./-]*$/.test(name) && !['__proto__', 'prototype', 'constructor'].includes(name)
}
export function record(value: unknown): asserts value is Record<string, any> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) invalid('expected an object')
}
export function keys(value: unknown, required: string[], optional: string[] = []): asserts value is Record<string, any> {
  record(value)
  if (required.some(k => !Object.hasOwn(value, k)) || Object.keys(value).some(k => !required.includes(k) && !optional.includes(k))) invalid('missing or unknown field')
}
export function canonical(value: unknown): string {
  if (Array.isArray(value)) return '[' + value.map(canonical).join(',') + ']'
  if (value && typeof value === 'object') return '{' + Object.keys(value).sort().map(k => JSON.stringify(k) + ':' + canonical((value as any)[k])).join(',') + '}'
  return JSON.stringify(value)
}
export function callbackKey(type: TypeExpr): string { return '$callback:' + createHash('sha256').update(signatureCanonical(type)).digest('hex') }

// Match Rust's serde boundary: the complete descriptor shape is decoded before
// capability admission. Malformed nested types cannot hide behind an earlier
// unsupported mode or stream, and strings are never coerced into map keys.
function typeStructure(type: any): void {
  record(type)
  switch (type.kind) {
    case 'value': keys(type, ['kind', 'schema']); return
    case 'object':
      keys(type, ['kind', 'interface', 'ownership'])
      if (typeof type.interface !== 'string' || !['scope', 'borrow'].includes(type.ownership)) invalid('invalid object type')
      return
    case 'callback':
      keys(type, ['kind', 'params', 'result', 'ownership'])
      if (!['scope', 'borrow'].includes(type.ownership)) invalid('invalid callback ownership')
      typeStructure(type.params); typeStructure(type.result); return
    case 'record':
      keys(type, ['kind', 'fields']); record(type.fields)
      for (const expr of Object.values(type.fields)) typeStructure(expr)
      return
    case 'list': case 'optional': case 'stream':
      keys(type, ['kind', 'item']); typeStructure(type.item); return
    default: invalid('unknown type expression')
  }
}
function bundleStructure(bundle: any): void {
  keys(bundle, ['id', 'version', 'interfaces'], ['events', 'required_capabilities'])
  if (typeof bundle.id !== 'string' || typeof bundle.version !== 'string') invalid('bundle identity must be strings')
  record(bundle.interfaces)
  if (bundle.required_capabilities !== undefined && (!Array.isArray(bundle.required_capabilities) || bundle.required_capabilities.some((c: unknown) => typeof c !== 'string'))) invalid('capabilities must be strings')
  for (const iface of Object.values(bundle.interfaces)) {
    keys(iface, ['methods'], ['properties']); record(iface.methods)
    for (const method of Object.values(iface.methods)) {
      keys(method, ['params', 'result']); typeStructure(method.params); typeStructure(method.result)
    }
    if (iface.properties !== undefined) {
      record(iface.properties)
      for (const property of Object.values(iface.properties)) typeStructure(property)
    }
  }
  if (bundle.events !== undefined) {
    record(bundle.events)
    for (const event of Object.values(bundle.events)) {
      keys(event, ['params', 'result', 'modes'])
      if (!Array.isArray(event.modes) || event.modes.some((m: unknown) => !['parallel', 'serial', 'emit', 'bail', 'waterfall'].includes(m as string))) invalid('unknown event mode')
      typeStructure(event.params); typeStructure(event.result)
    }
  }
}

export interface AdmittedBundle { readonly bundle: Bundle; readonly sha256: string }
function freezeTree(value: unknown): void {
  if (value && typeof value === 'object') {
    for (const child of Object.values(value)) freezeTree(child)
    Object.freeze(value)
  }
}
export function admit(bytes: Uint8Array): AdmittedBundle {
  const bundle: any = decodeJson(bytes)
  bundleStructure(bundle)
  keys(bundle, ['id', 'version', 'interfaces'], ['events', 'required_capabilities'])
  if (typeof bundle.id !== 'string' || !identifier(bundle.id) || typeof bundle.version !== 'string' || !/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(bundle.version)) invalid('invalid bundle identity')
  record(bundle.interfaces)
  if (!Object.keys(bundle.interfaces).length) invalid('interfaces are required')
  if (bundle.required_capabilities !== undefined && !Array.isArray(bundle.required_capabilities)) invalid('capabilities must be an array')
  if (new Set(bundle.required_capabilities ?? []).size !== (bundle.required_capabilities ?? []).length) invalid('duplicate required capability')
  for (const capability of bundle.required_capabilities ?? []) {
    if (typeof capability !== 'string') invalid('capability must be a string')
    if (!['object.scope', 'callback.borrow', 'event.parallel', 'event.serial'].includes(capability)) unsupported('capability is not implemented')
  }
  for (const [name, iface] of Object.entries(bundle.interfaces)) {
    if (!identifier(name)) invalid('invalid interface name')
    keys(iface, ['methods'], ['properties'])
    record(iface.methods)
    for (const [name, method] of Object.entries(iface.methods)) {
      if (!identifier(name) || Object.hasOwn(iface.properties ?? {}, name)) invalid('invalid or duplicate member')
      keys(method, ['params', 'result'])
      checkType(method.params, bundle, false)
      checkType(method.result, bundle, true)
    }
    if (iface.properties !== undefined) record(iface.properties)
    for (const [name, type] of Object.entries(iface.properties ?? {})) {
      if (!identifier(name)) invalid('invalid property name')
      checkType(type, bundle, true)
      if (containsCallback(type as TypeExpr)) unsupported('callbacks cannot be snapshot properties')
    }
  }
  if (bundle.events !== undefined) record(bundle.events)
  for (const [name, event] of Object.entries(bundle.events ?? {})) {
    if (!identifier(name)) invalid('invalid event name')
    keys(event, ['params', 'result', 'modes'])
    if (!Array.isArray(event.modes) || !event.modes.length) invalid('event modes required')
    if (new Set(event.modes).size !== event.modes.length) invalid('duplicate event mode')
    for (const mode of event.modes) {
      if (!['parallel', 'serial', 'emit', 'bail', 'waterfall'].includes(mode)) invalid('unknown event mode')
      if (!['parallel', 'serial'].includes(mode)) unsupported('event mode requires an extension')
    }
    checkType(event.params, bundle, true)
    checkType(event.result, bundle, true)
  }
  freezeTree(bundle)
  return Object.freeze({ bundle: bundle as unknown as Bundle, sha256: createHash('sha256').update(bytes).digest('hex') })
}

function containsCallback(type: TypeExpr): boolean {
  if (type.kind === 'callback') return true
  if (type.kind === 'record') return Object.values(type.fields).some(containsCallback)
  if (type.kind === 'list' || type.kind === 'optional') return containsCallback(type.item)
  return false
}
function checkType(type: any, bundle: Record<string, any>, result: boolean): void {
  record(type)
  switch (type.kind) {
    case 'value': keys(type, ['kind', 'schema']); checkSchema(type.schema); break
    case 'object':
      keys(type, ['kind', 'interface', 'ownership'])
      if (typeof type.interface !== 'string' || !Object.hasOwn(bundle.interfaces, type.interface)) invalid('unknown object interface')
      if (!['scope', 'borrow'].includes(type.ownership)) invalid('unknown ownership')
      if (result && type.ownership !== 'scope') invalid('returned objects must belong to a scope')
      break
    case 'callback':
      keys(type, ['kind', 'params', 'result', 'ownership'])
      if (!['scope', 'borrow'].includes(type.ownership)) invalid('unknown ownership')
      if (result || type.ownership !== 'borrow') unsupported('persistent callbacks are not implemented')
      checkType(type.params, bundle, false); checkType(type.result, bundle, true); break
    case 'record':
      keys(type, ['kind', 'fields']); record(type.fields)
      for (const [name, value] of Object.entries(type.fields)) { if (!identifier(name)) invalid('invalid record field'); checkType(value, bundle, result) }
      break
    case 'list': case 'optional': keys(type, ['kind', 'item']); checkType(type.item, bundle, result); break
    case 'stream': keys(type, ['kind', 'item']); unsupported('streams are not implemented')
    default: invalid('unknown type expression')
  }
}
const keywords = ['$schema', '$defs', '$ref', 'type', 'properties', 'required', 'additionalProperties', 'items', 'enum', 'const', 'oneOf', 'minimum', 'maximum', 'minItems', 'maxItems', 'minLength', 'maxLength', 'title', 'description']
function resolve(root: Schema, path: string): Schema {
  if (!path.startsWith('#/$defs/')) unsupported('only local value $ref is supported')
  let value: any = root
  for (const part of path.slice(2).split('/')) value = value?.[part.replace(/~1/g, '/').replace(/~0/g, '~')]
  if (value === undefined) invalid('missing value $ref')
  return value
}
export function checkSchema(schema: Schema, root = schema, stack = new Set<string>()): void {
  record(schema)
  for (const keyword of Object.keys(schema)) if (!keywords.includes(keyword)) unsupported('schema keyword is unsupported')
  if (schema.$schema !== undefined && schema.$schema !== 'https://json-schema.org/draft/2020-12/schema') unsupported('only JSON Schema 2020-12 is supported')
  for (const key of ['title', 'description']) if (schema[key] !== undefined && typeof schema[key] !== 'string') invalid('schema annotation must be a string')
  if (schema.$ref !== undefined) {
    if (typeof schema.$ref !== 'string') unsupported('only local value $ref is supported')
    if (stack.has(schema.$ref)) invalid('recursive value schema')
    stack.add(schema.$ref); checkSchema(resolve(root, schema.$ref), root, stack); stack.delete(schema.$ref)
  }
  if (schema.type !== undefined && !['object', 'array', 'boolean', 'null', 'number', 'integer', 'string'].includes(schema.type)) unsupported('schema type is unsupported')
  for (const key of ['properties', '$defs']) {
    if (schema[key] !== undefined) {
      record(schema[key])
      for (const [name, value] of Object.entries(schema[key])) { if (!identifier(name)) invalid('invalid schema field'); checkSchema(value as Schema, root, stack) }
    }
  }
  if (schema.items !== undefined) checkSchema(schema.items, root, stack)
  if (schema.oneOf !== undefined) {
    if (!Array.isArray(schema.oneOf) || !schema.oneOf.length) invalid('oneOf requires nonempty branches')
    for (const branch of schema.oneOf) checkSchema(branch, root, stack)
    const tagged = Object.keys(schema.oneOf[0].properties ?? {}).some(tag => {
      const constants = new Set<string>()
      return schema.oneOf.every((branch: Schema) => {
        const field = branch.properties?.[tag]
        if (branch.type !== 'object' || !branch.required?.includes(tag) || field?.type !== 'string' || typeof field.const !== 'string' || constants.has(field.const)) return false
        constants.add(field.const); return true
      })
    })
    if (!tagged) unsupported('oneOf requires a distinct required string discriminant')
  }
  if (schema.required !== undefined) {
    if (!Array.isArray(schema.required)) invalid('required must be an array')
    if (new Set(schema.required).size !== schema.required.length) invalid('duplicate required field')
    for (const field of schema.required) if (typeof field !== 'string' || !Object.hasOwn(schema.properties ?? {}, field)) invalid('required field is not declared')
  }
  if (schema.additionalProperties !== undefined && typeof schema.additionalProperties !== 'boolean') unsupported('additionalProperties schemas are unsupported')
  if (schema.enum !== undefined && (!Array.isArray(schema.enum) || !schema.enum.length)) invalid('enum must be nonempty')
  if (schema.enum !== undefined) {
    if (schema.enum.some((v: unknown) => !safeJson(v)) || new Set(schema.enum.map(canonical)).size !== schema.enum.length) invalid('unsafe or duplicate enum value')
  }
  if (Object.hasOwn(schema, 'const') && !safeJson(schema.const)) invalid('unsafe const value')
  for (const key of ['minimum', 'maximum', 'minItems', 'maxItems', 'minLength', 'maxLength']) {
    if (schema[key] === undefined) continue
    if (typeof schema[key] !== 'number' || !Number.isFinite(schema[key])) invalid('schema bound must be finite')
    if (!safeJson(schema[key])) invalid('unsafe schema numeric bound')
    if (!['minimum', 'maximum'].includes(key) && (!Number.isInteger(schema[key]) || schema[key] < 0)) invalid('size bound must be nonnegative integer')
  }
  for (const [min, max] of [['minimum', 'maximum'], ['minItems', 'maxItems'], ['minLength', 'maxLength']]) {
    if (schema[min] !== undefined && schema[max] !== undefined && schema[min] > schema[max]) invalid('inverted schema bounds')
  }
}
function safeJson(value: any): boolean {
  if (typeof value === 'number') return Number.isFinite(value) && (!Number.isInteger(value) || Number.isSafeInteger(value))
  if (typeof value === 'string') return scalarUnicode(value)
  if (Array.isArray(value)) return Object.getPrototypeOf(value) === Array.prototype && Object.keys(value).length === value.length && value.every(safeJson)
  if (value && typeof value === 'object') return [null, Object.prototype].includes(Object.getPrototypeOf(value)) && Object.entries(value).every(([k, v]) => !['__proto__', 'prototype', 'constructor'].includes(k) && safeJson(v))
  return value === null || ['string', 'boolean'].includes(typeof value)
}
export function validateJson(schema: Schema, value: unknown): void { checkSchema(schema); validateInner(schema, schema, value) }
function bounds(schema: Schema, min: string, max: string, value: number): void {
  if ((schema[min] !== undefined && value < schema[min]) || (schema[max] !== undefined && value > schema[max])) invalid('value outside bounds')
}
function validateInner(schema: Schema, root: Schema, value: any): void {
  if (!safeJson(value)) invalid('unsafe numeric or reflection value')
  if (schema.$ref) validateInner(resolve(root, schema.$ref), root, value)
  if (schema.type) {
    const valid = schema.type === 'object' ? value !== null && typeof value === 'object' && !Array.isArray(value)
      : schema.type === 'array' ? Array.isArray(value) : schema.type === 'null' ? value === null
      : schema.type === 'integer' ? typeof value === 'number' && Number.isInteger(value)
      : typeof value === schema.type
    if (!valid) invalid('schema type mismatch')
  }
  if (Object.hasOwn(schema, 'const') && canonical(value) !== canonical(schema.const)) invalid('const mismatch')
  if (schema.enum && !schema.enum.some((v: unknown) => canonical(value) === canonical(v))) invalid('enum mismatch')
  if (schema.oneOf) {
    let matches = 0
    for (const branch of schema.oneOf) { try { validateInner(branch, root, value); matches++ } catch {} }
    if (matches !== 1) invalid('oneOf requires exactly one matching branch')
  }
  if (value !== null && typeof value === 'object' && !Array.isArray(value)) {
    for (const field of schema.required ?? []) if (!Object.hasOwn(value, field)) invalid('missing required field')
    for (const [name, field] of Object.entries(value)) {
      if (Object.hasOwn(schema.properties ?? {}, name)) validateInner(schema.properties[name], root, field)
      else if (schema.additionalProperties === false) invalid('unexpected field')
    }
  }
  if (Array.isArray(value)) {
    bounds(schema, 'minItems', 'maxItems', value.length)
    if (schema.items) for (const item of value) validateInner(schema.items, root, item)
  }
  if (typeof value === 'string') bounds(schema, 'minLength', 'maxLength', [...value].length)
  if (typeof value === 'number') bounds(schema, 'minimum', 'maximum', value)
}
export function validateWire(type: TypeExpr, value: any, references: string[]): void {
  record(value)
  if (type.kind === 'value' && value.kind === 'value') { keys(value, ['kind', 'value']); validateJson(type.schema, value.value); return }
  if (value.kind === 'ref') {
    keys(value, ['kind', 'index'])
    if (!Number.isSafeInteger(value.index) || value.index < 0) invalid('invalid reference index')
    if (type.kind === 'object' && references[value.index] === type.interface) return
    if (type.kind === 'callback' && references[value.index] === callbackKey(type)) return
    invalid('reference interface mismatch')
  }
  if (type.kind === 'record' && value.kind === 'record') {
    keys(value, ['kind', 'fields']); keys(value.fields, Object.keys(type.fields))
    for (const [name, expr] of Object.entries(type.fields)) validateWire(expr, value.fields[name], references)
    return
  }
  if (type.kind === 'list' && value.kind === 'list') {
    keys(value, ['kind', 'items']); if (!Array.isArray(value.items)) invalid('items must be an array')
    for (const item of value.items) validateWire(type.item, item, references)
    return
  }
  if (type.kind === 'optional' && value.kind === 'optional') {
    keys(value, ['kind', 'value']); if (value.value !== null) validateWire(type.item, value.value, references)
    return
  }
  invalid('wire tag or structure mismatch')
}
