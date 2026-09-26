// Generated from raw bundle SHA-256 85761ee1bd6171c18bb18219fd31912097241bda151f82f565d05af47b8d0b60. Regenerate; do not edit.
import { Client, CallContext, facade, object, json, foreign, deniedMethod, type Caller, type Outbound } from '../src/sdk.ts'
import { ObjectProxy } from '../src/imports.ts'
import { Exports, type PinKey } from '../src/exports.ts'
export const BUNDLE_SHA256 = "85761ee1bd6171c18bb18219fd31912097241bda151f82f565d05af47b8d0b60"
export interface BorrowCallback0 { "call"(params: (Array<BorrowCallback0Method0ParamsItemItem> | null)): Promise<BorrowCallback0Method0Result>
 }
export interface BorrowCallback0Service { "call"(context: CallContext, params: (Array<BorrowCallback0Method0ParamsItemItem> | null)): Promise<BorrowCallback0Method0Result>
 }
export function bindBorrowCallback0(proxy: ObjectProxy, caller: Caller): BorrowCallback0 { const client = new Client(proxy, caller, BUNDLE_SHA256, "$callback:9df9212785473f83e272628f613c6fb6b5a5ab42625e45001e6566cd89ee272f"); return facade<BorrowCallback0>(client, { "call": async (params: (Array<BorrowCallback0Method0ParamsItemItem> | null)) => { const value = await client.call("call", { kind: 'optional' as const, value: (params) === null ? null : ({ kind: 'list' as const, items: (params).map(item => (json(item))) }) }); return ((value) as BorrowCallback0Method0Result) } }, {  }) }
class BorrowCallback0Dispatch { constructor(private exports: Exports, private source: WeakRef<BorrowCallback0Service>) {} async dispatch(key: PinKey, context: CallContext, method: string, params: unknown): Promise<Outbound> { const native = this.exports.executionObject(key) as BorrowCallback0Service; if (native !== this.source.deref()) throw deniedMethod(); const caller = context.caller; switch (method) { case "call": { const input = (params) === null ? null : ((params as unknown[]).map(item => ((item) as BorrowCallback0Method0ParamsItemItem))); const value = await native["call"](context, input); return (json(value)) }
 default: throw deniedMethod() } } }
export function exportBorrowCallback0(native: BorrowCallback0Service): Outbound { return { kind: 'own', value: { register(exports) { const identity = exports.register(native); return { identity, interface: "$callback:9df9212785473f83e272628f613c6fb6b5a5ab42625e45001e6566cd89ee272f", bundleSha256: BUNDLE_SHA256, dispatcher: new BorrowCallback0Dispatch(exports, new WeakRef(native)) } }, snapshot() { return {  } } } } }

export interface InterfaceResource { "client"(params: InterfaceResource): Promise<InterfaceResourceMethod0Result>
 }
export interface InterfaceResourceService { "client"(context: CallContext, params: InterfaceResource): Promise<InterfaceResourceMethod0Result>
 }
export function bindInterfaceResource(proxy: ObjectProxy, caller: Caller): InterfaceResource { const client = new Client(proxy, caller, BUNDLE_SHA256, "Resource"); return facade<InterfaceResource>(client, { "client": async (params: InterfaceResource) => { const value = await client.call("client", foreign(params)); return ((value) as InterfaceResourceMethod0Result) } }, {  }) }
class InterfaceResourceDispatch { constructor(private exports: Exports, private source: WeakRef<InterfaceResourceService>) {} async dispatch(key: PinKey, context: CallContext, method: string, params: unknown): Promise<Outbound> { const native = this.exports.executionObject(key) as InterfaceResourceService; if (native !== this.source.deref()) throw deniedMethod(); const caller = context.caller; switch (method) { case "client": { const input = bindInterfaceResource(object(params), caller); const value = await native["client"](context, input); return (json(value)) }
 default: throw deniedMethod() } } }
export function exportInterfaceResource(native: InterfaceResourceService): Outbound { return { kind: 'own', value: { register(exports) { const identity = exports.register(native); return { identity, interface: "Resource", bundleSha256: BUNDLE_SHA256, dispatcher: new InterfaceResourceDispatch(exports, new WeakRef(native)) } }, snapshot() { return {  } } } } }

export interface InterfaceValues { "compose"(params: { "callback": (BorrowCallback0Service | null); "items": Array<InterfaceResource>; "type": InterfaceValuesMethod0ParamsField2 }): Promise<(Array<InterfaceResource> | null)>
"dto"(params: InterfaceValuesMethod1Params): Promise<InterfaceValuesMethod1Result>
"then$"(params: InterfaceValuesMethod2Params): Promise<InterfaceValuesMethod2Result>
 }
export interface InterfaceValuesService { "compose"(context: CallContext, params: { "callback": (BorrowCallback0 | null); "items": Array<InterfaceResource>; "type": InterfaceValuesMethod0ParamsField2 }): Promise<(Array<InterfaceResourceService> | null)>
"dto"(context: CallContext, params: InterfaceValuesMethod1Params): Promise<InterfaceValuesMethod1Result>
"then"(context: CallContext, params: InterfaceValuesMethod2Params): Promise<InterfaceValuesMethod2Result>
 }
export function bindInterfaceValues(proxy: ObjectProxy, caller: Caller): InterfaceValues { const client = new Client(proxy, caller, BUNDLE_SHA256, "Values"); return facade<InterfaceValues>(client, { "compose": async (params: { "callback": (BorrowCallback0Service | null); "items": Array<InterfaceResource>; "type": InterfaceValuesMethod0ParamsField2 }) => { const value = await client.call("compose", { kind: 'record' as const, fields: { "callback": ({ kind: 'optional' as const, value: ((params)["callback"]) === null ? null : (exportBorrowCallback0((params)["callback"])) }),"items": ({ kind: 'list' as const, items: ((params)["items"]).map(item => (foreign(item))) }),"type": (json((params)["type"])) } }); return ((value) === null ? null : ((value as unknown[]).map(item => (bindInterfaceResource(object(item), caller))))) },"dto": async (params: InterfaceValuesMethod1Params) => { const value = await client.call("dto", json(params)); return ((value) as InterfaceValuesMethod1Result) },"then$": async (params: InterfaceValuesMethod2Params) => { const value = await client.call("then", json(params)); return ((value) as InterfaceValuesMethod2Result) } }, {  }) }
class InterfaceValuesDispatch { constructor(private exports: Exports, private source: WeakRef<InterfaceValuesService>) {} async dispatch(key: PinKey, context: CallContext, method: string, params: unknown): Promise<Outbound> { const native = this.exports.executionObject(key) as InterfaceValuesService; if (native !== this.source.deref()) throw deniedMethod(); const caller = context.caller; switch (method) { case "compose": { const input = { "callback": (((params as Record<string, unknown>)["callback"]) === null ? null : (bindBorrowCallback0(object((params as Record<string, unknown>)["callback"]), caller))),"items": (((params as Record<string, unknown>)["items"] as unknown[]).map(item => (bindInterfaceResource(object(item), caller)))),"type": (((params as Record<string, unknown>)["type"]) as InterfaceValuesMethod0ParamsField2) }; const value = await native["compose"](context, input); return ({ kind: 'optional' as const, value: (value) === null ? null : ({ kind: 'list' as const, items: (value).map(item => (exportInterfaceResource(item))) }) }) }
case "dto": { const input = (params) as InterfaceValuesMethod1Params; const value = await native["dto"](context, input); return (json(value)) }
case "then": { const input = (params) as InterfaceValuesMethod2Params; const value = await native["then"](context, input); return (json(value)) }
 default: throw deniedMethod() } } }
export function exportInterfaceValues(native: InterfaceValuesService): Outbound { return { kind: 'own', value: { register(exports) { const identity = exports.register(native); return { identity, interface: "Values", bundleSha256: BUNDLE_SHA256, dispatcher: new InterfaceValuesDispatch(exports, new WeakRef(native)) } }, snapshot() { return {  } } } } }

export type BorrowCallback0Method0ParamsItemItem = string
export type BorrowCallback0Method0Result = null
export type InterfaceResourceMethod0Result = boolean
export type InterfaceValuesMethod0ParamsField2 = number
export type InterfaceValuesMethod1Params = { "choice": { "tag": "first"; "text": string } | { "count": number; "tag": "second" }; "labels": Array<"red" | "green">; "nullable"?: null }
export type InterfaceValuesMethod1Result = { "choice": { "tag": "first"; "text": string } | { "count": number; "tag": "second" }; "labels": Array<"red" | "green">; "nullable"?: null }
export type InterfaceValuesMethod2Params = string
export type InterfaceValuesMethod2Result = string
