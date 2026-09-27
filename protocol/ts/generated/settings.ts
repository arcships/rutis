// Generated from raw bundle SHA-256 bd0ff221406d16687579ea09cfa20c78ac82fd66dbcb9aaa5ba6513eb5a7b19f. Regenerate; do not edit.
import { Client, CallContext, facade, object, json, foreign, deniedMethod, type Caller, type Outbound } from '../src/sdk.ts'
import { ObjectProxy } from '../src/imports.ts'
import { Exports, type PinKey } from '../src/exports.ts'
export const BUNDLE_SHA256 = "bd0ff221406d16687579ea09cfa20c78ac82fd66dbcb9aaa5ba6513eb5a7b19f"
export interface BorrowCallback0 { "call"(params: BorrowCallback0Method0Params): Promise<BorrowCallback0Method0Result>
 }
export interface BorrowCallback0Service { "call"(context: CallContext, params: BorrowCallback0Method0Params): Promise<BorrowCallback0Method0Result>
 }
export function bindBorrowCallback0(proxy: ObjectProxy, caller: Caller): BorrowCallback0 { const client = new Client(proxy, caller, BUNDLE_SHA256, "$callback:58b61d2f232ae6322d4fdb611a85253b0e817a258b7f2ce7e5a94b5f5c10a414"); return facade<BorrowCallback0>(client, { "call": async (params: BorrowCallback0Method0Params) => { const value = await client.call("call", json(params)); return ((value) as BorrowCallback0Method0Result) } }, {  }) }
class BorrowCallback0Dispatch { constructor(private exports: Exports, private source: WeakRef<BorrowCallback0Service>) {} async dispatch(key: PinKey, context: CallContext, method: string, params: unknown): Promise<Outbound> { const native = this.exports.executionObject(key) as BorrowCallback0Service; if (native !== this.source.deref()) throw deniedMethod(); const caller = context.caller; switch (method) { case "call": { const input = (params) as BorrowCallback0Method0Params; const value = await native["call"](context, input); return (json(value)) }
 default: throw deniedMethod() } } }
export function exportBorrowCallback0(native: BorrowCallback0Service): Outbound { return { kind: 'own', value: { register(exports) { const identity = exports.register(native); return { identity, interface: "$callback:58b61d2f232ae6322d4fdb611a85253b0e817a258b7f2ce7e5a94b5f5c10a414", bundleSha256: BUNDLE_SHA256, dispatcher: new BorrowCallback0Dispatch(exports, new WeakRef(native)) } }, snapshot() { return {  } } } } }

export interface InterfaceEventListener { "call"(params: { "section": InterfaceSettingsSection; "value": InterfaceEventListenerMethod0ParamsField1 }): Promise<{ "returned": InterfaceEventListenerMethod0ResultField0; "value": (InterfaceEventListenerMethod0ResultField1Item | null) }>
 }
export interface InterfaceEventListenerService { "call"(context: CallContext, params: { "section": InterfaceSettingsSection; "value": InterfaceEventListenerMethod0ParamsField1 }): Promise<{ "returned": InterfaceEventListenerMethod0ResultField0; "value": (InterfaceEventListenerMethod0ResultField1Item | null) }>
 }
export function bindInterfaceEventListener(proxy: ObjectProxy, caller: Caller): InterfaceEventListener { const client = new Client(proxy, caller, BUNDLE_SHA256, "EventListener"); return facade<InterfaceEventListener>(client, { "call": async (params: { "section": InterfaceSettingsSection; "value": InterfaceEventListenerMethod0ParamsField1 }) => { const value = await client.call("call", { kind: 'record' as const, fields: { "section": (foreign((params)["section"])),"value": (json((params)["value"])) } }); return ({ "returned": (((value as Record<string, unknown>)["returned"]) as InterfaceEventListenerMethod0ResultField0),"value": (((value as Record<string, unknown>)["value"]) === null ? null : (((value as Record<string, unknown>)["value"]) as InterfaceEventListenerMethod0ResultField1Item)) }) } }, {  }) }
class InterfaceEventListenerDispatch { constructor(private exports: Exports, private source: WeakRef<InterfaceEventListenerService>) {} async dispatch(key: PinKey, context: CallContext, method: string, params: unknown): Promise<Outbound> { const native = this.exports.executionObject(key) as InterfaceEventListenerService; if (native !== this.source.deref()) throw deniedMethod(); const caller = context.caller; switch (method) { case "call": { const input = { "section": (bindInterfaceSettingsSection(object((params as Record<string, unknown>)["section"]), caller)),"value": (((params as Record<string, unknown>)["value"]) as InterfaceEventListenerMethod0ParamsField1) }; const value = await native["call"](context, input); return ({ kind: 'record' as const, fields: { "returned": (json((value)["returned"])),"value": ({ kind: 'optional' as const, value: ((value)["value"]) === null ? null : (json((value)["value"])) }) } }) }
 default: throw deniedMethod() } } }
export function exportInterfaceEventListener(native: InterfaceEventListenerService): Outbound { return { kind: 'own', value: { register(exports) { const identity = exports.register(native); return { identity, interface: "EventListener", bundleSha256: BUNDLE_SHA256, dispatcher: new InterfaceEventListenerDispatch(exports, new WeakRef(native)) } }, snapshot() { return {  } } } } }

export interface InterfaceEventPublisher { "dispatch"(params: { "mode": InterfaceEventPublisherMethod0ParamsField0; "payload": { "section": InterfaceSettingsSection; "value": InterfaceEventPublisherMethod0ParamsField1Field1 } }): Promise<{ "errors": InterfaceEventPublisherMethod0ResultField0; "returned": InterfaceEventPublisherMethod0ResultField1; "value": (InterfaceEventPublisherMethod0ResultField2Item | null) }>
 }
export interface InterfaceEventPublisherService { "dispatch"(context: CallContext, params: { "mode": InterfaceEventPublisherMethod0ParamsField0; "payload": { "section": InterfaceSettingsSection; "value": InterfaceEventPublisherMethod0ParamsField1Field1 } }): Promise<{ "errors": InterfaceEventPublisherMethod0ResultField0; "returned": InterfaceEventPublisherMethod0ResultField1; "value": (InterfaceEventPublisherMethod0ResultField2Item | null) }>
 }
export function bindInterfaceEventPublisher(proxy: ObjectProxy, caller: Caller): InterfaceEventPublisher { const client = new Client(proxy, caller, BUNDLE_SHA256, "EventPublisher"); return facade<InterfaceEventPublisher>(client, { "dispatch": async (params: { "mode": InterfaceEventPublisherMethod0ParamsField0; "payload": { "section": InterfaceSettingsSection; "value": InterfaceEventPublisherMethod0ParamsField1Field1 } }) => { const value = await client.call("dispatch", { kind: 'record' as const, fields: { "mode": (json((params)["mode"])),"payload": ({ kind: 'record' as const, fields: { "section": (foreign(((params)["payload"])["section"])),"value": (json(((params)["payload"])["value"])) } }) } }); return ({ "errors": (((value as Record<string, unknown>)["errors"]) as InterfaceEventPublisherMethod0ResultField0),"returned": (((value as Record<string, unknown>)["returned"]) as InterfaceEventPublisherMethod0ResultField1),"value": (((value as Record<string, unknown>)["value"]) === null ? null : (((value as Record<string, unknown>)["value"]) as InterfaceEventPublisherMethod0ResultField2Item)) }) } }, {  }) }
class InterfaceEventPublisherDispatch { constructor(private exports: Exports, private source: WeakRef<InterfaceEventPublisherService>) {} async dispatch(key: PinKey, context: CallContext, method: string, params: unknown): Promise<Outbound> { const native = this.exports.executionObject(key) as InterfaceEventPublisherService; if (native !== this.source.deref()) throw deniedMethod(); const caller = context.caller; switch (method) { case "dispatch": { const input = { "mode": (((params as Record<string, unknown>)["mode"]) as InterfaceEventPublisherMethod0ParamsField0),"payload": ({ "section": (bindInterfaceSettingsSection(object(((params as Record<string, unknown>)["payload"] as Record<string, unknown>)["section"]), caller)),"value": ((((params as Record<string, unknown>)["payload"] as Record<string, unknown>)["value"]) as InterfaceEventPublisherMethod0ParamsField1Field1) }) }; const value = await native["dispatch"](context, input); return ({ kind: 'record' as const, fields: { "errors": (json((value)["errors"])),"returned": (json((value)["returned"])),"value": ({ kind: 'optional' as const, value: ((value)["value"]) === null ? null : (json((value)["value"])) }) } }) }
 default: throw deniedMethod() } } }
export function exportInterfaceEventPublisher(native: InterfaceEventPublisherService): Outbound { return { kind: 'own', value: { register(exports) { const identity = exports.register(native); return { identity, interface: "EventPublisher", bundleSha256: BUNDLE_SHA256, dispatcher: new InterfaceEventPublisherDispatch(exports, new WeakRef(native)) } }, snapshot() { return {  } } } } }

export interface InterfaceSettings { "inspect"(params: InterfaceSettingsSection): Promise<InterfaceSettingsMethod0Result>
"open"(params: InterfaceSettingsMethod1Params): Promise<InterfaceSettingsSection>
 }
export interface InterfaceSettingsService { "inspect"(context: CallContext, params: InterfaceSettingsSection): Promise<InterfaceSettingsMethod0Result>
"open"(context: CallContext, params: InterfaceSettingsMethod1Params): Promise<InterfaceSettingsSectionService>
 }
export function bindInterfaceSettings(proxy: ObjectProxy, caller: Caller): InterfaceSettings { const client = new Client(proxy, caller, BUNDLE_SHA256, "Settings"); return facade<InterfaceSettings>(client, { "inspect": async (params: InterfaceSettingsSection) => { const value = await client.call("inspect", foreign(params)); return ((value) as InterfaceSettingsMethod0Result) },"open": async (params: InterfaceSettingsMethod1Params) => { const value = await client.call("open", json(params)); return (bindInterfaceSettingsSection(object(value), caller)) } }, {  }) }
class InterfaceSettingsDispatch { constructor(private exports: Exports, private source: WeakRef<InterfaceSettingsService>) {} async dispatch(key: PinKey, context: CallContext, method: string, params: unknown): Promise<Outbound> { const native = this.exports.executionObject(key) as InterfaceSettingsService; if (native !== this.source.deref()) throw deniedMethod(); const caller = context.caller; switch (method) { case "inspect": { const input = bindInterfaceSettingsSection(object(params), caller); const value = await native["inspect"](context, input); return (json(value)) }
case "open": { const input = (params) as InterfaceSettingsMethod1Params; const value = await native["open"](context, input); return (exportInterfaceSettingsSection(value)) }
 default: throw deniedMethod() } } }
export function exportInterfaceSettings(native: InterfaceSettingsService): Outbound { return { kind: 'own', value: { register(exports) { const identity = exports.register(native); return { identity, interface: "Settings", bundleSha256: BUNDLE_SHA256, dispatcher: new InterfaceSettingsDispatch(exports, new WeakRef(native)) } }, snapshot() { return {  } } } } }

export interface InterfaceSettingsSection { "read"(params: InterfaceSettingsSectionMethod0Params): Promise<InterfaceSettingsSectionMethod0Result>
"replace"(params: InterfaceSettingsSectionMethod1Params): Promise<InterfaceSettingsSectionMethod1Result>
"visit"(params: BorrowCallback0Service): Promise<InterfaceSettingsSectionMethod2Result>
"write"(params: InterfaceSettingsSectionMethod3Params): Promise<InterfaceSettingsSectionMethod3Result>
readonly "namespace": InterfaceSettingsSectionProperty0
 }
export interface InterfaceSettingsSectionService { "read"(context: CallContext, params: InterfaceSettingsSectionMethod0Params): Promise<InterfaceSettingsSectionMethod0Result>
"replace"(context: CallContext, params: InterfaceSettingsSectionMethod1Params): Promise<InterfaceSettingsSectionMethod1Result>
"visit"(context: CallContext, params: BorrowCallback0): Promise<InterfaceSettingsSectionMethod2Result>
"write"(context: CallContext, params: InterfaceSettingsSectionMethod3Params): Promise<InterfaceSettingsSectionMethod3Result>
readonly "namespace": InterfaceSettingsSectionProperty0
 }
export function bindInterfaceSettingsSection(proxy: ObjectProxy, caller: Caller): InterfaceSettingsSection { const client = new Client(proxy, caller, BUNDLE_SHA256, "SettingsSection"); return facade<InterfaceSettingsSection>(client, { "read": async (params: InterfaceSettingsSectionMethod0Params) => { const value = await client.call("read", json(params)); return ((value) as InterfaceSettingsSectionMethod0Result) },"replace": async (params: InterfaceSettingsSectionMethod1Params) => { const value = await client.call("replace", json(params)); return ((value) as InterfaceSettingsSectionMethod1Result) },"visit": async (params: BorrowCallback0Service) => { const value = await client.call("visit", exportBorrowCallback0(params)); return ((value) as InterfaceSettingsSectionMethod2Result) },"write": async (params: InterfaceSettingsSectionMethod3Params) => { const value = await client.call("write", json(params)); return ((value) as InterfaceSettingsSectionMethod3Result) } }, { "namespace": () => ((client.property("namespace")) as InterfaceSettingsSectionProperty0) }) }
class InterfaceSettingsSectionDispatch { constructor(private exports: Exports, private source: WeakRef<InterfaceSettingsSectionService>) {} async dispatch(key: PinKey, context: CallContext, method: string, params: unknown): Promise<Outbound> { const native = this.exports.executionObject(key) as InterfaceSettingsSectionService; if (native !== this.source.deref()) throw deniedMethod(); const caller = context.caller; switch (method) { case "read": { const input = (params) as InterfaceSettingsSectionMethod0Params; const value = await native["read"](context, input); return (json(value)) }
case "replace": { const input = (params) as InterfaceSettingsSectionMethod1Params; const value = await native["replace"](context, input); return (json(value)) }
case "visit": { const input = bindBorrowCallback0(object(params), caller); const value = await native["visit"](context, input); return (json(value)) }
case "write": { const input = (params) as InterfaceSettingsSectionMethod3Params; const value = await native["write"](context, input); return (json(value)) }
 default: throw deniedMethod() } } }
export function exportInterfaceSettingsSection(native: InterfaceSettingsSectionService): Outbound { return { kind: 'own', value: { register(exports) { const identity = exports.register(native); return { identity, interface: "SettingsSection", bundleSha256: BUNDLE_SHA256, dispatcher: new InterfaceSettingsSectionDispatch(exports, new WeakRef(native)) } }, snapshot() { return { "namespace": (json(native["namespace"])) } } } } }

export type BorrowCallback0Method0Params = { [key: string]: unknown }
export type BorrowCallback0Method0Result = { [key: string]: unknown }
export type InterfaceEventListenerMethod0ParamsField1 = { [key: string]: unknown }
export type InterfaceEventListenerMethod0ResultField0 = boolean
export type InterfaceEventListenerMethod0ResultField1Item = unknown
export type InterfaceEventPublisherMethod0ParamsField0 = "parallel" | "serial"
export type InterfaceEventPublisherMethod0ParamsField1Field1 = { [key: string]: unknown }
export type InterfaceEventPublisherMethod0ResultField0 = Array<{ [key: string]: unknown }>
export type InterfaceEventPublisherMethod0ResultField1 = boolean
export type InterfaceEventPublisherMethod0ResultField2Item = unknown
export type InterfaceSettingsMethod0Result = { [key: string]: unknown }
export type InterfaceSettingsMethod1Params = string
export type InterfaceSettingsSectionMethod0Params = null
export type InterfaceSettingsSectionMethod0Result = { [key: string]: unknown }
export type InterfaceSettingsSectionMethod1Params = { [key: string]: unknown }
export type InterfaceSettingsSectionMethod1Result = null
export type InterfaceSettingsSectionMethod2Result = { [key: string]: unknown }
export type InterfaceSettingsSectionMethod3Params = { [key: string]: unknown }
export type InterfaceSettingsSectionMethod3Result = null
export type InterfaceSettingsSectionProperty0 = string
