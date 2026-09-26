// Generated from raw bundle SHA-256 bd6a60c25d4bd27f52ba37893da07a6c18833171d0e31fa1746fc890ebc6ea38. Regenerate; do not edit.
import { Client, CallContext, facade, object, json, foreign, deniedMethod, type Caller, type Outbound } from '../src/sdk.ts'
import { ObjectProxy } from '../src/imports.ts'
import { Exports, type PinKey } from '../src/exports.ts'
export const BUNDLE_SHA256 = "bd6a60c25d4bd27f52ba37893da07a6c18833171d0e31fa1746fc890ebc6ea38"
export interface BorrowCallback0 { "call"(params: BorrowCallback0Method0Params): Promise<BorrowCallback0Method0Result>
 }
export interface BorrowCallback0Service { "call"(context: CallContext, params: BorrowCallback0Method0Params): Promise<BorrowCallback0Method0Result>
 }
export function bindBorrowCallback0(proxy: ObjectProxy, caller: Caller): BorrowCallback0 { const client = new Client(proxy, caller, BUNDLE_SHA256, "$callback:ef6e854d2f3354a0f169b69c18abd1e193cf6547d7e4dab395c5d6c4faaae90d"); return facade<BorrowCallback0>(client, { "call": async (params: BorrowCallback0Method0Params) => { const value = await client.call("call", json(params)); return ((value) as BorrowCallback0Method0Result) } }, {  }) }
class BorrowCallback0Dispatch { constructor(private exports: Exports, private source: WeakRef<BorrowCallback0Service>) {} async dispatch(key: PinKey, context: CallContext, method: string, params: unknown): Promise<Outbound> { const native = this.exports.executionObject(key) as BorrowCallback0Service; if (native !== this.source.deref()) throw deniedMethod(); const caller = context.caller; switch (method) { case "call": { const input = (params) as BorrowCallback0Method0Params; const value = await native["call"](context, input); return (json(value)) }
 default: throw deniedMethod() } } }
export function exportBorrowCallback0(native: BorrowCallback0Service): Outbound { return { kind: 'own', value: { register(exports) { const identity = exports.register(native); return { identity, interface: "$callback:ef6e854d2f3354a0f169b69c18abd1e193cf6547d7e4dab395c5d6c4faaae90d", bundleSha256: BUNDLE_SHA256, dispatcher: new BorrowCallback0Dispatch(exports, new WeakRef(native)) } }, snapshot() { return {  } } } } }

export interface InterfaceAgent { readonly "session": InterfaceSession
 }
export interface InterfaceAgentService { readonly "session": InterfaceSessionService
 }
export function bindInterfaceAgent(proxy: ObjectProxy, caller: Caller): InterfaceAgent { const client = new Client(proxy, caller, BUNDLE_SHA256, "Agent"); return facade<InterfaceAgent>(client, {  }, { "session": () => (bindInterfaceSession(object(client.property("session")), caller)) }) }
class InterfaceAgentDispatch { constructor(private exports: Exports, private source: WeakRef<InterfaceAgentService>) {} async dispatch(key: PinKey, context: CallContext, method: string, params: unknown): Promise<Outbound> { const native = this.exports.executionObject(key) as InterfaceAgentService; if (native !== this.source.deref()) throw deniedMethod(); const caller = context.caller; switch (method) {  default: throw deniedMethod() } } }
export function exportInterfaceAgent(native: InterfaceAgentService): Outbound { return { kind: 'own', value: { register(exports) { const identity = exports.register(native); return { identity, interface: "Agent", bundleSha256: BUNDLE_SHA256, dispatcher: new InterfaceAgentDispatch(exports, new WeakRef(native)) } }, snapshot() { return { "session": (exportInterfaceSession(native["session"])) } } } } }

export interface InterfaceConnection { "query"(params: InterfaceConnectionMethod0Params): Promise<InterfaceConnectionMethod0Result>
readonly "session": InterfaceSession
 }
export interface InterfaceConnectionService { "query"(context: CallContext, params: InterfaceConnectionMethod0Params): Promise<InterfaceConnectionMethod0Result>
readonly "session": InterfaceSessionService
 }
export function bindInterfaceConnection(proxy: ObjectProxy, caller: Caller): InterfaceConnection { const client = new Client(proxy, caller, BUNDLE_SHA256, "Connection"); return facade<InterfaceConnection>(client, { "query": async (params: InterfaceConnectionMethod0Params) => { const value = await client.call("query", json(params)); return ((value) as InterfaceConnectionMethod0Result) } }, { "session": () => (bindInterfaceSession(object(client.property("session")), caller)) }) }
class InterfaceConnectionDispatch { constructor(private exports: Exports, private source: WeakRef<InterfaceConnectionService>) {} async dispatch(key: PinKey, context: CallContext, method: string, params: unknown): Promise<Outbound> { const native = this.exports.executionObject(key) as InterfaceConnectionService; if (native !== this.source.deref()) throw deniedMethod(); const caller = context.caller; switch (method) { case "query": { const input = (params) as InterfaceConnectionMethod0Params; const value = await native["query"](context, input); return (json(value)) }
 default: throw deniedMethod() } } }
export function exportInterfaceConnection(native: InterfaceConnectionService): Outbound { return { kind: 'own', value: { register(exports) { const identity = exports.register(native); return { identity, interface: "Connection", bundleSha256: BUNDLE_SHA256, dispatcher: new InterfaceConnectionDispatch(exports, new WeakRef(native)) } }, snapshot() { return { "session": (exportInterfaceSession(native["session"])) } } } } }

export interface InterfaceDatabase { "connect"(params: InterfaceDatabaseMethod0Params): Promise<InterfaceConnection>
"inspect"(params: InterfaceConnection): Promise<InterfaceDatabaseMethod1Result>
"withCallback"(params: BorrowCallback0Service): Promise<InterfaceDatabaseMethod2Result>
 }
export interface InterfaceDatabaseService { "connect"(context: CallContext, params: InterfaceDatabaseMethod0Params): Promise<InterfaceConnectionService>
"inspect"(context: CallContext, params: InterfaceConnection): Promise<InterfaceDatabaseMethod1Result>
"withCallback"(context: CallContext, params: BorrowCallback0): Promise<InterfaceDatabaseMethod2Result>
 }
export function bindInterfaceDatabase(proxy: ObjectProxy, caller: Caller): InterfaceDatabase { const client = new Client(proxy, caller, BUNDLE_SHA256, "Database"); return facade<InterfaceDatabase>(client, { "connect": async (params: InterfaceDatabaseMethod0Params) => { const value = await client.call("connect", json(params)); return (bindInterfaceConnection(object(value), caller)) },"inspect": async (params: InterfaceConnection) => { const value = await client.call("inspect", foreign(params)); return ((value) as InterfaceDatabaseMethod1Result) },"withCallback": async (params: BorrowCallback0Service) => { const value = await client.call("withCallback", exportBorrowCallback0(params)); return ((value) as InterfaceDatabaseMethod2Result) } }, {  }) }
class InterfaceDatabaseDispatch { constructor(private exports: Exports, private source: WeakRef<InterfaceDatabaseService>) {} async dispatch(key: PinKey, context: CallContext, method: string, params: unknown): Promise<Outbound> { const native = this.exports.executionObject(key) as InterfaceDatabaseService; if (native !== this.source.deref()) throw deniedMethod(); const caller = context.caller; switch (method) { case "connect": { const input = (params) as InterfaceDatabaseMethod0Params; const value = await native["connect"](context, input); return (exportInterfaceConnection(value)) }
case "inspect": { const input = bindInterfaceConnection(object(params), caller); const value = await native["inspect"](context, input); return (json(value)) }
case "withCallback": { const input = bindBorrowCallback0(object(params), caller); const value = await native["withCallback"](context, input); return (json(value)) }
 default: throw deniedMethod() } } }
export function exportInterfaceDatabase(native: InterfaceDatabaseService): Outbound { return { kind: 'own', value: { register(exports) { const identity = exports.register(native); return { identity, interface: "Database", bundleSha256: BUNDLE_SHA256, dispatcher: new InterfaceDatabaseDispatch(exports, new WeakRef(native)) } }, snapshot() { return {  } } } } }

export interface InterfaceSession { readonly "agent": InterfaceAgent
 }
export interface InterfaceSessionService { readonly "agent": InterfaceAgentService
 }
export function bindInterfaceSession(proxy: ObjectProxy, caller: Caller): InterfaceSession { const client = new Client(proxy, caller, BUNDLE_SHA256, "Session"); return facade<InterfaceSession>(client, {  }, { "agent": () => (bindInterfaceAgent(object(client.property("agent")), caller)) }) }
class InterfaceSessionDispatch { constructor(private exports: Exports, private source: WeakRef<InterfaceSessionService>) {} async dispatch(key: PinKey, context: CallContext, method: string, params: unknown): Promise<Outbound> { const native = this.exports.executionObject(key) as InterfaceSessionService; if (native !== this.source.deref()) throw deniedMethod(); const caller = context.caller; switch (method) {  default: throw deniedMethod() } } }
export function exportInterfaceSession(native: InterfaceSessionService): Outbound { return { kind: 'own', value: { register(exports) { const identity = exports.register(native); return { identity, interface: "Session", bundleSha256: BUNDLE_SHA256, dispatcher: new InterfaceSessionDispatch(exports, new WeakRef(native)) } }, snapshot() { return { "agent": (exportInterfaceAgent(native["agent"])) } } } } }

export type BorrowCallback0Method0Params = string
export type BorrowCallback0Method0Result = null
export type InterfaceConnectionMethod0Params = { "sql": string }
export type InterfaceConnectionMethod0Result = Array<{ [key: string]: unknown }>
export type InterfaceDatabaseMethod0Params = { "name": string }
export type InterfaceDatabaseMethod1Result = boolean
export type InterfaceDatabaseMethod2Result = null
