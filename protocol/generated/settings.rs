// Generated from raw bundle SHA-256 bd0ff221406d16687579ea09cfa20c78ac82fd66dbcb9aaa5ba6513eb5a7b19f. Regenerate; do not edit.
use rutis_protocol::sdk::*;
use rutis_protocol::{error::Result, exports::{Exports, PinKey}, graph::DecodedValue};
use std::{sync::Arc, collections::BTreeMap};
pub const BUNDLE_SHA256: &str = "bd0ff221406d16687579ea09cfa20c78ac82fd66dbcb9aaa5ba6513eb5a7b19f";
#[allow(non_snake_case)] pub trait BorrowCallback0Service: Send + Sync + 'static { fn call(&self, context: CallContext, params: BTreeMap<String, serde_json::Value>) -> RpcFuture<BTreeMap<String, serde_json::Value>>;
 }
#[derive(Clone)] pub struct BorrowCallback0Client(Client);
impl ClientHandle for BorrowCallback0Client { const INTERFACE: &'static str = "$callback:58b61d2f232ae6322d4fdb611a85253b0e817a258b7f2ce7e5a94b5f5c10a414"; const BUNDLE_SHA256: &'static str = BUNDLE_SHA256; fn client(&self) -> &Client { &self.0 } fn from_client(client: Client) -> Self { Self(client) } }
#[allow(non_snake_case, unused_variables)] impl BorrowCallback0Client { pub async fn call(&self, params: BTreeMap<String, serde_json::Value>) -> Result<BTreeMap<String, serde_json::Value>> { let caller = self.0.caller(); let value = self.0.call("call", Outbound::json(params)?).await?; json::<BTreeMap<String, serde_json::Value>>(value) }
 }
struct BorrowCallback0Export(Arc<dyn BorrowCallback0Service>);
struct BorrowCallback0Dispatch { exports: Exports, identity: rutis_protocol::identity::ObjectIdentity }
#[allow(unused_variables)] impl Dispatcher for BorrowCallback0Dispatch { fn dispatch(&self, key: PinKey, context: CallContext, method: String, params: DecodedValue) -> RpcFuture<Outbound> { let exports = self.exports.clone(); let identity = self.identity.clone(); Box::pin(async move { let object = exports.execution_trait::<dyn BorrowCallback0Service>(&key)?; if !exports.is_native(&identity, &object) { return Err(denied_method()); } let caller = context.caller(); match method.as_str() { "call" => { let params = json::<BTreeMap<String, serde_json::Value>>(params)?; let value = object.call(context, params).await?; Outbound::json(value) },
 _ => Err(denied_method()) } }) } }
impl NativeExport for BorrowCallback0Export { fn register(&self, exports: &Exports) -> Result<RegisteredExport> { let identity = exports.register_trait(&self.0)?; Ok(RegisteredExport { identity: identity.clone(), interface: "$callback:58b61d2f232ae6322d4fdb611a85253b0e817a258b7f2ce7e5a94b5f5c10a414", bundle_sha256: BUNDLE_SHA256, dispatcher: Arc::new(BorrowCallback0Dispatch { exports: exports.clone(), identity }) }) } fn snapshot(&self) -> Result<BTreeMap<String, Outbound>> { Ok(BTreeMap::from([])) } }
#[allow(non_snake_case)] pub fn exportBorrowCallback0(native: Arc<dyn BorrowCallback0Service>) -> Outbound { Outbound::Own(Arc::new(BorrowCallback0Export(native))) }

#[allow(non_snake_case)] pub trait InterfaceEventListenerService: Send + Sync + 'static { fn call(&self, context: CallContext, params: InterfaceEventListenerMethod0ParamsParamRecord) -> RpcFuture<InterfaceEventListenerMethod0ResultResultRecord>;
 }
#[derive(Clone)] pub struct InterfaceEventListenerClient(Client);
impl ClientHandle for InterfaceEventListenerClient { const INTERFACE: &'static str = "EventListener"; const BUNDLE_SHA256: &'static str = BUNDLE_SHA256; fn client(&self) -> &Client { &self.0 } fn from_client(client: Client) -> Self { Self(client) } }
#[allow(non_snake_case, unused_variables)] impl InterfaceEventListenerClient { pub async fn call(&self, params: InterfaceEventListenerMethod0ParamsClientRecord) -> Result<InterfaceEventListenerMethod0ResultParamRecord> { let caller = self.0.caller(); let value = self.0.call("call", Outbound::Record(BTreeMap::from([("section".into(), Outbound::Foreign(ClientHandle::client(&((params).section)).proxy().clone())),("value".into(), Outbound::json((params).value)?)]))).await?; Ok::<_, rutis_protocol::error::ProtocolError>({ let mut fields = record(value)?; InterfaceEventListenerMethod0ResultParamRecord { returned: json::<bool>(field(&mut fields, "returned")?)?,value: optional(field(&mut fields, "value")?)?.map(|item| json::<serde_json::Value>(item)).transpose()? } }) }
 }
struct InterfaceEventListenerExport(Arc<dyn InterfaceEventListenerService>);
struct InterfaceEventListenerDispatch { exports: Exports, identity: rutis_protocol::identity::ObjectIdentity }
#[allow(unused_variables)] impl Dispatcher for InterfaceEventListenerDispatch { fn dispatch(&self, key: PinKey, context: CallContext, method: String, params: DecodedValue) -> RpcFuture<Outbound> { let exports = self.exports.clone(); let identity = self.identity.clone(); Box::pin(async move { let object = exports.execution_trait::<dyn InterfaceEventListenerService>(&key)?; if !exports.is_native(&identity, &object) { return Err(denied_method()); } let caller = context.caller(); match method.as_str() { "call" => { let params = { let mut fields = record(params)?; InterfaceEventListenerMethod0ParamsParamRecord { section: bind::<InterfaceSettingsSectionClient>(field(&mut fields, "section")?, caller.clone())?,value: json::<BTreeMap<String, serde_json::Value>>(field(&mut fields, "value")?)? } }; let value = object.call(context, params).await?; Ok::<_, rutis_protocol::error::ProtocolError>(Outbound::Record(BTreeMap::from([("returned".into(), Outbound::json((value).returned)?),("value".into(), Outbound::Optional(((value).value).map(|item| Ok::<_, rutis_protocol::error::ProtocolError>(Box::new(Outbound::json(item)?))).transpose()?))]))) },
 _ => Err(denied_method()) } }) } }
impl NativeExport for InterfaceEventListenerExport { fn register(&self, exports: &Exports) -> Result<RegisteredExport> { let identity = exports.register_trait(&self.0)?; Ok(RegisteredExport { identity: identity.clone(), interface: "EventListener", bundle_sha256: BUNDLE_SHA256, dispatcher: Arc::new(InterfaceEventListenerDispatch { exports: exports.clone(), identity }) }) } fn snapshot(&self) -> Result<BTreeMap<String, Outbound>> { Ok(BTreeMap::from([])) } }
#[allow(non_snake_case)] pub fn exportInterfaceEventListener(native: Arc<dyn InterfaceEventListenerService>) -> Outbound { Outbound::Own(Arc::new(InterfaceEventListenerExport(native))) }

#[allow(non_snake_case)] pub trait InterfaceEventPublisherService: Send + Sync + 'static { fn dispatch(&self, context: CallContext, params: InterfaceEventPublisherMethod0ParamsParamRecord) -> RpcFuture<InterfaceEventPublisherMethod0ResultResultRecord>;
 }
#[derive(Clone)] pub struct InterfaceEventPublisherClient(Client);
impl ClientHandle for InterfaceEventPublisherClient { const INTERFACE: &'static str = "EventPublisher"; const BUNDLE_SHA256: &'static str = BUNDLE_SHA256; fn client(&self) -> &Client { &self.0 } fn from_client(client: Client) -> Self { Self(client) } }
#[allow(non_snake_case, unused_variables)] impl InterfaceEventPublisherClient { pub async fn dispatch(&self, params: InterfaceEventPublisherMethod0ParamsClientRecord) -> Result<InterfaceEventPublisherMethod0ResultParamRecord> { let caller = self.0.caller(); let value = self.0.call("dispatch", Outbound::Record(BTreeMap::from([("mode".into(), Outbound::json((params).mode)?),("payload".into(), Outbound::Record(BTreeMap::from([("section".into(), Outbound::Foreign(ClientHandle::client(&(((params).payload).section)).proxy().clone())),("value".into(), Outbound::json(((params).payload).value)?)])))]))).await?; Ok::<_, rutis_protocol::error::ProtocolError>({ let mut fields = record(value)?; InterfaceEventPublisherMethod0ResultParamRecord { errors: json::<Vec<BTreeMap<String, serde_json::Value>>>(field(&mut fields, "errors")?)?,returned: json::<bool>(field(&mut fields, "returned")?)?,value: optional(field(&mut fields, "value")?)?.map(|item| json::<serde_json::Value>(item)).transpose()? } }) }
 }
struct InterfaceEventPublisherExport(Arc<dyn InterfaceEventPublisherService>);
struct InterfaceEventPublisherDispatch { exports: Exports, identity: rutis_protocol::identity::ObjectIdentity }
#[allow(unused_variables)] impl Dispatcher for InterfaceEventPublisherDispatch { fn dispatch(&self, key: PinKey, context: CallContext, method: String, params: DecodedValue) -> RpcFuture<Outbound> { let exports = self.exports.clone(); let identity = self.identity.clone(); Box::pin(async move { let object = exports.execution_trait::<dyn InterfaceEventPublisherService>(&key)?; if !exports.is_native(&identity, &object) { return Err(denied_method()); } let caller = context.caller(); match method.as_str() { "dispatch" => { let params = { let mut fields = record(params)?; InterfaceEventPublisherMethod0ParamsParamRecord { mode: json::<InterfaceEventPublisherMethod0ParamsField0>(field(&mut fields, "mode")?)?,payload: { let mut fields = record(field(&mut fields, "payload")?)?; InterfaceEventPublisherMethod0ParamsField1ParamRecord { section: bind::<InterfaceSettingsSectionClient>(field(&mut fields, "section")?, caller.clone())?,value: json::<BTreeMap<String, serde_json::Value>>(field(&mut fields, "value")?)? } } } }; let value = object.dispatch(context, params).await?; Ok::<_, rutis_protocol::error::ProtocolError>(Outbound::Record(BTreeMap::from([("errors".into(), Outbound::json((value).errors)?),("returned".into(), Outbound::json((value).returned)?),("value".into(), Outbound::Optional(((value).value).map(|item| Ok::<_, rutis_protocol::error::ProtocolError>(Box::new(Outbound::json(item)?))).transpose()?))]))) },
 _ => Err(denied_method()) } }) } }
impl NativeExport for InterfaceEventPublisherExport { fn register(&self, exports: &Exports) -> Result<RegisteredExport> { let identity = exports.register_trait(&self.0)?; Ok(RegisteredExport { identity: identity.clone(), interface: "EventPublisher", bundle_sha256: BUNDLE_SHA256, dispatcher: Arc::new(InterfaceEventPublisherDispatch { exports: exports.clone(), identity }) }) } fn snapshot(&self) -> Result<BTreeMap<String, Outbound>> { Ok(BTreeMap::from([])) } }
#[allow(non_snake_case)] pub fn exportInterfaceEventPublisher(native: Arc<dyn InterfaceEventPublisherService>) -> Outbound { Outbound::Own(Arc::new(InterfaceEventPublisherExport(native))) }

#[allow(non_snake_case)] pub trait InterfaceSettingsService: Send + Sync + 'static { fn inspect(&self, context: CallContext, params: InterfaceSettingsSectionClient) -> RpcFuture<BTreeMap<String, serde_json::Value>>;
fn open(&self, context: CallContext, params: String) -> RpcFuture<Arc<dyn InterfaceSettingsSectionService>>;
 }
#[derive(Clone)] pub struct InterfaceSettingsClient(Client);
impl ClientHandle for InterfaceSettingsClient { const INTERFACE: &'static str = "Settings"; const BUNDLE_SHA256: &'static str = BUNDLE_SHA256; fn client(&self) -> &Client { &self.0 } fn from_client(client: Client) -> Self { Self(client) } }
#[allow(non_snake_case, unused_variables)] impl InterfaceSettingsClient { pub async fn inspect(&self, params: InterfaceSettingsSectionClient) -> Result<BTreeMap<String, serde_json::Value>> { let caller = self.0.caller(); let value = self.0.call("inspect", Outbound::Foreign(ClientHandle::client(&(params)).proxy().clone())).await?; json::<BTreeMap<String, serde_json::Value>>(value) }
pub async fn open(&self, params: String) -> Result<InterfaceSettingsSectionClient> { let caller = self.0.caller(); let value = self.0.call("open", Outbound::json(params)?).await?; bind::<InterfaceSettingsSectionClient>(value, caller.clone()) }
 }
struct InterfaceSettingsExport(Arc<dyn InterfaceSettingsService>);
struct InterfaceSettingsDispatch { exports: Exports, identity: rutis_protocol::identity::ObjectIdentity }
#[allow(unused_variables)] impl Dispatcher for InterfaceSettingsDispatch { fn dispatch(&self, key: PinKey, context: CallContext, method: String, params: DecodedValue) -> RpcFuture<Outbound> { let exports = self.exports.clone(); let identity = self.identity.clone(); Box::pin(async move { let object = exports.execution_trait::<dyn InterfaceSettingsService>(&key)?; if !exports.is_native(&identity, &object) { return Err(denied_method()); } let caller = context.caller(); match method.as_str() { "inspect" => { let params = bind::<InterfaceSettingsSectionClient>(params, caller.clone())?; let value = object.inspect(context, params).await?; Outbound::json(value) },
"open" => { let params = json::<String>(params)?; let value = object.open(context, params).await?; Ok::<_, rutis_protocol::error::ProtocolError>(exportInterfaceSettingsSection(value)) },
 _ => Err(denied_method()) } }) } }
impl NativeExport for InterfaceSettingsExport { fn register(&self, exports: &Exports) -> Result<RegisteredExport> { let identity = exports.register_trait(&self.0)?; Ok(RegisteredExport { identity: identity.clone(), interface: "Settings", bundle_sha256: BUNDLE_SHA256, dispatcher: Arc::new(InterfaceSettingsDispatch { exports: exports.clone(), identity }) }) } fn snapshot(&self) -> Result<BTreeMap<String, Outbound>> { Ok(BTreeMap::from([])) } }
#[allow(non_snake_case)] pub fn exportInterfaceSettings(native: Arc<dyn InterfaceSettingsService>) -> Outbound { Outbound::Own(Arc::new(InterfaceSettingsExport(native))) }

#[allow(non_snake_case)] pub trait InterfaceSettingsSectionService: Send + Sync + 'static { fn read(&self, context: CallContext, params: ()) -> RpcFuture<BTreeMap<String, serde_json::Value>>;
fn replace(&self, context: CallContext, params: BTreeMap<String, serde_json::Value>) -> RpcFuture<()>;
fn visit(&self, context: CallContext, params: BorrowCallback0Client) -> RpcFuture<BTreeMap<String, serde_json::Value>>;
fn write(&self, context: CallContext, params: BTreeMap<String, serde_json::Value>) -> RpcFuture<()>;
fn namespace(&self) -> Result<String>;
 }
#[derive(Clone)] pub struct InterfaceSettingsSectionClient(Client);
impl ClientHandle for InterfaceSettingsSectionClient { const INTERFACE: &'static str = "SettingsSection"; const BUNDLE_SHA256: &'static str = BUNDLE_SHA256; fn client(&self) -> &Client { &self.0 } fn from_client(client: Client) -> Self { Self(client) } }
#[allow(non_snake_case, unused_variables)] impl InterfaceSettingsSectionClient { pub async fn read(&self, params: ()) -> Result<BTreeMap<String, serde_json::Value>> { let caller = self.0.caller(); let value = self.0.call("read", Outbound::json(params)?).await?; json::<BTreeMap<String, serde_json::Value>>(value) }
pub async fn replace(&self, params: BTreeMap<String, serde_json::Value>) -> Result<()> { let caller = self.0.caller(); let value = self.0.call("replace", Outbound::json(params)?).await?; json::<()>(value) }
pub async fn visit(&self, params: Arc<dyn BorrowCallback0Service>) -> Result<BTreeMap<String, serde_json::Value>> { let caller = self.0.caller(); let value = self.0.call("visit", exportBorrowCallback0(params)).await?; json::<BTreeMap<String, serde_json::Value>>(value) }
pub async fn write(&self, params: BTreeMap<String, serde_json::Value>) -> Result<()> { let caller = self.0.caller(); let value = self.0.call("write", Outbound::json(params)?).await?; json::<()>(value) }
pub fn namespace(&self) -> Result<String> { let caller = self.0.caller(); let value = self.0.property("namespace")?; json::<String>(value) }
 }
struct InterfaceSettingsSectionExport(Arc<dyn InterfaceSettingsSectionService>);
struct InterfaceSettingsSectionDispatch { exports: Exports, identity: rutis_protocol::identity::ObjectIdentity }
#[allow(unused_variables)] impl Dispatcher for InterfaceSettingsSectionDispatch { fn dispatch(&self, key: PinKey, context: CallContext, method: String, params: DecodedValue) -> RpcFuture<Outbound> { let exports = self.exports.clone(); let identity = self.identity.clone(); Box::pin(async move { let object = exports.execution_trait::<dyn InterfaceSettingsSectionService>(&key)?; if !exports.is_native(&identity, &object) { return Err(denied_method()); } let caller = context.caller(); match method.as_str() { "read" => { json::<()>(params)?; let value = object.read(context, ()).await?; Outbound::json(value) },
"replace" => { let params = json::<BTreeMap<String, serde_json::Value>>(params)?; object.replace(context, params).await?; Outbound::json(()) },
"visit" => { let params = bind::<BorrowCallback0Client>(params, caller.clone())?; let value = object.visit(context, params).await?; Outbound::json(value) },
"write" => { let params = json::<BTreeMap<String, serde_json::Value>>(params)?; object.write(context, params).await?; Outbound::json(()) },
 _ => Err(denied_method()) } }) } }
impl NativeExport for InterfaceSettingsSectionExport { fn register(&self, exports: &Exports) -> Result<RegisteredExport> { let identity = exports.register_trait(&self.0)?; Ok(RegisteredExport { identity: identity.clone(), interface: "SettingsSection", bundle_sha256: BUNDLE_SHA256, dispatcher: Arc::new(InterfaceSettingsSectionDispatch { exports: exports.clone(), identity }) }) } fn snapshot(&self) -> Result<BTreeMap<String, Outbound>> { Ok(BTreeMap::from([("namespace".into(), Outbound::json(self.0.namespace()?)?)])) } }
#[allow(non_snake_case)] pub fn exportInterfaceSettingsSection(native: Arc<dyn InterfaceSettingsSectionService>) -> Outbound { Outbound::Own(Arc::new(InterfaceSettingsSectionExport(native))) }

#[derive(Clone)] pub struct InterfaceEventListenerMethod0ParamsClientRecord { pub section: InterfaceSettingsSectionClient,
pub value: BTreeMap<String, serde_json::Value>,
 }
#[derive(Clone)] pub struct InterfaceEventListenerMethod0ParamsParamRecord { pub section: InterfaceSettingsSectionClient,
pub value: BTreeMap<String, serde_json::Value>,
 }
#[derive(Clone)] pub struct InterfaceEventListenerMethod0ResultParamRecord { pub returned: bool,
pub value: Option<serde_json::Value>,
 }
#[derive(Clone)] pub struct InterfaceEventListenerMethod0ResultResultRecord { pub returned: bool,
pub value: Option<serde_json::Value>,
 }
#[derive(Clone)] pub struct InterfaceEventPublisherMethod0ParamsClientRecord { pub mode: InterfaceEventPublisherMethod0ParamsField0,
pub payload: InterfaceEventPublisherMethod0ParamsField1ClientRecord,
 }
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)] #[serde(crate = "rutis_protocol::sdk::serde")] pub enum InterfaceEventPublisherMethod0ParamsField0 { #[serde(rename = "parallel")] V0,#[serde(rename = "serial")] V1 }
#[derive(Clone)] pub struct InterfaceEventPublisherMethod0ParamsField1ClientRecord { pub section: InterfaceSettingsSectionClient,
pub value: BTreeMap<String, serde_json::Value>,
 }
#[derive(Clone)] pub struct InterfaceEventPublisherMethod0ParamsField1ParamRecord { pub section: InterfaceSettingsSectionClient,
pub value: BTreeMap<String, serde_json::Value>,
 }
#[derive(Clone)] pub struct InterfaceEventPublisherMethod0ParamsParamRecord { pub mode: InterfaceEventPublisherMethod0ParamsField0,
pub payload: InterfaceEventPublisherMethod0ParamsField1ParamRecord,
 }
#[derive(Clone)] pub struct InterfaceEventPublisherMethod0ResultParamRecord { pub errors: Vec<BTreeMap<String, serde_json::Value>>,
pub returned: bool,
pub value: Option<serde_json::Value>,
 }
#[derive(Clone)] pub struct InterfaceEventPublisherMethod0ResultResultRecord { pub errors: Vec<BTreeMap<String, serde_json::Value>>,
pub returned: bool,
pub value: Option<serde_json::Value>,
 }
