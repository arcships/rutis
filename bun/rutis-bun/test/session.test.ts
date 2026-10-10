import { expect, test } from 'bun:test'
import { CAPABILITIES } from '../src/session.ts'
import { encode, decode } from '../src/codec.ts'

test('the runtime declares that it is re-entrant', () => {
  expect(CAPABILITIES).toContain('reentrant-sync')
})

// That an unrelated call runs while a synchronous call waits is checked
// against real processes: crates/rutis-bridge/tests/bun_runtime.rs
// (`incoming_calls_run_while_a_synchronous_call_waits`) and
// crates/rutis-loader/tests/bun_multilang.rs.

test('codec escapes newlines and refuses non-finite numbers', () => {
  expect(encode({ text: 'a\nb' })).not.toContain('\n')
  expect(() => encode({ n: Infinity })).toThrow('non-finite')
})
