import { ProtocolError } from './error.ts'

export const MAX_JSON_BYTES = 16 * 1024 * 1024
export const MAX_JSON_DEPTH = 64
const invalid = (message: string): never => { throw new ProtocolError('InvalidParams', 'decode', message) }

export function scalarUnicode(text: string): boolean {
  for (let i = 0; i < text.length; i++) {
    const unit = text.charCodeAt(i)
    if (unit >= 0xd800 && unit <= 0xdbff) {
      const next = text.charCodeAt(++i)
      if (!(next >= 0xdc00 && next <= 0xdfff)) return false
    } else if (unit >= 0xdc00 && unit <= 0xdfff) return false
  }
  return true
}

/** Do not overwrite duplicate keys, strip a BOM, accept lone surrogates, or
 * depend on the engine's maximum recursive call stack. */
export function decodeJson(bytes: Uint8Array): unknown {
  if (bytes.byteLength > MAX_JSON_BYTES) invalid('JSON exceeds decoder byte bound')
  let text: string
  try { text = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(bytes) }
  catch { return invalid('invalid UTF-8 JSON') }
  let at = 0
  const space = () => { while (/[\x20\t\r\n]/.test(text[at] ?? '') && at < text.length) at++ }
  const string = (): string => {
    const start = at++
    while (at < text.length) {
      const char = text[at++]
      if (char === '\\') { at++; continue }
      if (char === '"') {
        let value: string
        try { value = JSON.parse(text.slice(start, at)) }
        catch { return invalid('invalid JSON string') }
        if (!scalarUnicode(value)) invalid('JSON string contains a lone surrogate')
        return value
      }
    }
    return invalid('unterminated JSON string')
  }
  const value = (depth: number): unknown => {
    if (depth > MAX_JSON_DEPTH) invalid('JSON exceeds decoder depth bound')
    space()
    const char = text[at]
    if (char === '"') return string()
    if (char === '{') {
      at++; space()
      const fields: Record<string, unknown> = Object.create(null)
      if (text[at] === '}') { at++; return fields }
      while (true) {
        if (text[at] !== '"') invalid('expected JSON object key')
        const key = string()
        if (Object.hasOwn(fields, key)) invalid('duplicate JSON key')
        space(); if (text[at++] !== ':') invalid('expected colon')
        fields[key] = value(depth + 1)
        space()
        if (text[at] === '}') { at++; return fields }
        if (text[at++] !== ',') invalid('expected comma')
        space()
      }
    }
    if (char === '[') {
      at++; space()
      const items: unknown[] = []
      if (text[at] === ']') { at++; return items }
      while (true) {
        items.push(value(depth + 1)); space()
        if (text[at] === ']') { at++; return items }
        if (text[at++] !== ',') invalid('expected comma')
      }
    }
    for (const [literal, result] of [['true', true], ['false', false], ['null', null]] as const) {
      if (text.startsWith(literal, at)) { at += literal.length; return result }
    }
    const match = /^-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?/.exec(text.slice(at))
    if (!match) return invalid('invalid JSON token')
    at += match[0].length
    const number = Number(match[0])
    if (!Number.isFinite(number) || (Number.isInteger(number) && !Number.isSafeInteger(number))) invalid('unsafe JSON number')
    return number === 0 ? 0 : number
  }
  const decoded = value(0)
  space()
  if (at !== text.length) invalid('trailing JSON content')
  return decoded
}

export function signatureCanonical(value: unknown): string {
  if (value === null) return 'z'
  if (typeof value === 'boolean') return value ? 't' : 'f'
  if (typeof value === 'number') {
    const bytes = Buffer.alloc(8)
    bytes.writeDoubleBE(value === 0 ? 0 : value)
    return 'n' + bytes.toString('hex') + ';'
  }
  if (typeof value === 'string') return 's' + Buffer.byteLength(value) + ':' + value
  if (Array.isArray(value)) return 'a' + value.length + ':' + value.map(signatureCanonical).join('')
  if (value && typeof value === 'object') {
    const fields = value as Record<string, unknown>
    const keys = Object.keys(fields).sort((a, b) => Buffer.compare(Buffer.from(a), Buffer.from(b)))
    return 'o' + keys.length + ':' + keys.map(k => signatureCanonical(k) + signatureCanonical(fields[k])).join('')
  }
  return invalid('not a JSON value')
}
