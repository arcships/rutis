// Generated from raw bundle SHA-256 bd6a60c25d4bd27f52ba37893da07a6c18833171d0e31fa1746fc890ebc6ea38. Regenerate; do not edit.
use rutis_protocol::sdk::*;
use rutis_protocol::{error::Result, exports::{Exports, PinKey}, graph::DecodedValue};
use std::{sync::Arc, collections::BTreeMap};
pub const BUNDLE_SHA256: &str = "bd6a60c25d4bd27f52ba37893da07a6c18833171d0e31fa1746fc890ebc6ea38";
#[allow(non_snake_case)] pub trait BorrowCallback0Service: Send + Sync + 'static { fn call(&self, context: CallContext, params: String) -> RpcFuture<()>;
 }
#[derive(Clone)] pub struct BorrowCallback0Client(Client);
impl ClientHandle for BorrowCallback0Client { const INTERFACE: &'static str = "$callback:ef6e854d2f3354a0f169b69c18abd1e193cf6547d7e4dab395c5d6c4faaae90d"; const BUNDLE_SHA256: &'static str = BUNDLE_SHA256; fn client(&self) -> &Client { &self.0 } fn from_client(client: Client) -> Self { Self(client) } }
#[allow(non_snake_case, unused_variables)] impl BorrowCallback0Client { pub async fn call(&self, params: String) -> Result<()> { let caller = self.0.caller(); let value = self.0.call("call", Outbound::json(params)?).await?; json::<()>(value) }
 }
struct BorrowCallback0Export(Arc<dyn BorrowCallback0Service>);
struct BorrowCallback0Dispatch { exports: Exports, identity: rutis_protocol::identity::ObjectIdentity }
#[allow(unused_variables)] impl Dispatcher for BorrowCallback0Dispatch { fn dispatch(&self, key: PinKey, context: CallContext, method: String, params: DecodedValue) -> RpcFuture<Outbound> { let exports = self.exports.clone(); let identity = self.identity.clone(); Box::pin(async move { let object = exports.execution_trait::<dyn BorrowCallback0Service>(&key)?; if !exports.is_native(&identity, &object) { return Err(denied_method()); } let caller = context.caller(); match method.as_str() { "call" => { let params = json::<String>(params)?; object.call(context, params).await?; Outbound::json(()) },
 _ => Err(denied_method()) } }) } }
impl NativeExport for BorrowCallback0Export { fn register(&self, exports: &Exports) -> Result<RegisteredExport> { let identity = exports.register_trait(&self.0)?; Ok(RegisteredExport { identity: identity.clone(), interface: "$callback:ef6e854d2f3354a0f169b69c18abd1e193cf6547d7e4dab395c5d6c4faaae90d", bundle_sha256: BUNDLE_SHA256, dispatcher: Arc::new(BorrowCallback0Dispatch { exports: exports.clone(), identity }) }) } fn snapshot(&self) -> Result<BTreeMap<String, Outbound>> { Ok(BTreeMap::from([])) } }
#[allow(non_snake_case)] pub fn exportBorrowCallback0(native: Arc<dyn BorrowCallback0Service>) -> Outbound { Outbound::Own(Arc::new(BorrowCallback0Export(native))) }

#[allow(non_snake_case)] pub trait InterfaceAgentService: Send + Sync + 'static { fn session(&self) -> Result<Arc<dyn InterfaceSessionService>>;
 }
#[derive(Clone)] pub struct InterfaceAgentClient(Client);
impl ClientHandle for InterfaceAgentClient { const INTERFACE: &'static str = "Agent"; const BUNDLE_SHA256: &'static str = BUNDLE_SHA256; fn client(&self) -> &Client { &self.0 } fn from_client(client: Client) -> Self { Self(client) } }
#[allow(non_snake_case, unused_variables)] impl InterfaceAgentClient { pub fn session(&self) -> Result<InterfaceSessionClient> { let caller = self.0.caller(); let value = self.0.property("session")?; bind::<InterfaceSessionClient>(value, caller.clone()) }
 }
struct InterfaceAgentExport(Arc<dyn InterfaceAgentService>);
struct InterfaceAgentDispatch { exports: Exports, identity: rutis_protocol::identity::ObjectIdentity }
#[allow(unused_variables)] impl Dispatcher for InterfaceAgentDispatch { fn dispatch(&self, key: PinKey, context: CallContext, method: String, params: DecodedValue) -> RpcFuture<Outbound> { let exports = self.exports.clone(); let identity = self.identity.clone(); Box::pin(async move { let object = exports.execution_trait::<dyn InterfaceAgentService>(&key)?; if !exports.is_native(&identity, &object) { return Err(denied_method()); } let caller = context.caller(); Err(denied_method()) }) } }
impl NativeExport for InterfaceAgentExport { fn register(&self, exports: &Exports) -> Result<RegisteredExport> { let identity = exports.register_trait(&self.0)?; Ok(RegisteredExport { identity: identity.clone(), interface: "Agent", bundle_sha256: BUNDLE_SHA256, dispatcher: Arc::new(InterfaceAgentDispatch { exports: exports.clone(), identity }) }) } fn snapshot(&self) -> Result<BTreeMap<String, Outbound>> { Ok(BTreeMap::from([("session".into(), exportInterfaceSession(self.0.session()?))])) } }
#[allow(non_snake_case)] pub fn exportInterfaceAgent(native: Arc<dyn InterfaceAgentService>) -> Outbound { Outbound::Own(Arc::new(InterfaceAgentExport(native))) }

#[allow(non_snake_case)] pub trait InterfaceConnectionService: Send + Sync + 'static { fn query(&self, context: CallContext, params: InterfaceConnectionMethod0Params) -> RpcFuture<Vec<BTreeMap<String, serde_json::Value>>>;
fn session(&self) -> Result<Arc<dyn InterfaceSessionService>>;
 }
#[derive(Clone)] pub struct InterfaceConnectionClient(Client);
impl ClientHandle for InterfaceConnectionClient { const INTERFACE: &'static str = "Connection"; const BUNDLE_SHA256: &'static str = BUNDLE_SHA256; fn client(&self) -> &Client { &self.0 } fn from_client(client: Client) -> Self { Self(client) } }
#[allow(non_snake_case, unused_variables)] impl InterfaceConnectionClient { pub async fn query(&self, params: InterfaceConnectionMethod0Params) -> Result<Vec<BTreeMap<String, serde_json::Value>>> { let caller = self.0.caller(); let value = self.0.call("query", Outbound::json(params)?).await?; json::<Vec<BTreeMap<String, serde_json::Value>>>(value) }
pub fn session(&self) -> Result<InterfaceSessionClient> { let caller = self.0.caller(); let value = self.0.property("session")?; bind::<InterfaceSessionClient>(value, caller.clone()) }
 }
struct InterfaceConnectionExport(Arc<dyn InterfaceConnectionService>);
struct InterfaceConnectionDispatch { exports: Exports, identity: rutis_protocol::identity::ObjectIdentity }
#[allow(unused_variables)] impl Dispatcher for InterfaceConnectionDispatch { fn dispatch(&self, key: PinKey, context: CallContext, method: String, params: DecodedValue) -> RpcFuture<Outbound> { let exports = self.exports.clone(); let identity = self.identity.clone(); Box::pin(async move { let object = exports.execution_trait::<dyn InterfaceConnectionService>(&key)?; if !exports.is_native(&identity, &object) { return Err(denied_method()); } let caller = context.caller(); match method.as_str() { "query" => { let params = json::<InterfaceConnectionMethod0Params>(params)?; let value = object.query(context, params).await?; Outbound::json(value) },
 _ => Err(denied_method()) } }) } }
impl NativeExport for InterfaceConnectionExport { fn register(&self, exports: &Exports) -> Result<RegisteredExport> { let identity = exports.register_trait(&self.0)?; Ok(RegisteredExport { identity: identity.clone(), interface: "Connection", bundle_sha256: BUNDLE_SHA256, dispatcher: Arc::new(InterfaceConnectionDispatch { exports: exports.clone(), identity }) }) } fn snapshot(&self) -> Result<BTreeMap<String, Outbound>> { Ok(BTreeMap::from([("session".into(), exportInterfaceSession(self.0.session()?))])) } }
#[allow(non_snake_case)] pub fn exportInterfaceConnection(native: Arc<dyn InterfaceConnectionService>) -> Outbound { Outbound::Own(Arc::new(InterfaceConnectionExport(native))) }

#[allow(non_snake_case)] pub trait InterfaceDatabaseService: Send + Sync + 'static { fn connect(&self, context: CallContext, params: InterfaceDatabaseMethod0Params) -> RpcFuture<Arc<dyn InterfaceConnectionService>>;
fn inspect(&self, context: CallContext, params: InterfaceConnectionClient) -> RpcFuture<bool>;
fn withCallback(&self, context: CallContext, params: BorrowCallback0Client) -> RpcFuture<()>;
 }
#[derive(Clone)] pub struct InterfaceDatabaseClient(Client);
impl ClientHandle for InterfaceDatabaseClient { const INTERFACE: &'static str = "Database"; const BUNDLE_SHA256: &'static str = BUNDLE_SHA256; fn client(&self) -> &Client { &self.0 } fn from_client(client: Client) -> Self { Self(client) } }
#[allow(non_snake_case, unused_variables)] impl InterfaceDatabaseClient { pub async fn connect(&self, params: InterfaceDatabaseMethod0Params) -> Result<InterfaceConnectionClient> { let caller = self.0.caller(); let value = self.0.call("connect", Outbound::json(params)?).await?; bind::<InterfaceConnectionClient>(value, caller.clone()) }
pub async fn inspect(&self, params: InterfaceConnectionClient) -> Result<bool> { let caller = self.0.caller(); let value = self.0.call("inspect", Outbound::Foreign(ClientHandle::client(&(params)).proxy().clone())).await?; json::<bool>(value) }
pub async fn withCallback(&self, params: Arc<dyn BorrowCallback0Service>) -> Result<()> { let caller = self.0.caller(); let value = self.0.call("withCallback", exportBorrowCallback0(params)).await?; json::<()>(value) }
 }
struct InterfaceDatabaseExport(Arc<dyn InterfaceDatabaseService>);
struct InterfaceDatabaseDispatch { exports: Exports, identity: rutis_protocol::identity::ObjectIdentity }
#[allow(unused_variables)] impl Dispatcher for InterfaceDatabaseDispatch { fn dispatch(&self, key: PinKey, context: CallContext, method: String, params: DecodedValue) -> RpcFuture<Outbound> { let exports = self.exports.clone(); let identity = self.identity.clone(); Box::pin(async move { let object = exports.execution_trait::<dyn InterfaceDatabaseService>(&key)?; if !exports.is_native(&identity, &object) { return Err(denied_method()); } let caller = context.caller(); match method.as_str() { "connect" => { let params = json::<InterfaceDatabaseMethod0Params>(params)?; let value = object.connect(context, params).await?; Ok::<_, rutis_protocol::error::ProtocolError>(exportInterfaceConnection(value)) },
"inspect" => { let params = bind::<InterfaceConnectionClient>(params, caller.clone())?; let value = object.inspect(context, params).await?; Outbound::json(value) },
"withCallback" => { let params = bind::<BorrowCallback0Client>(params, caller.clone())?; object.withCallback(context, params).await?; Outbound::json(()) },
 _ => Err(denied_method()) } }) } }
impl NativeExport for InterfaceDatabaseExport { fn register(&self, exports: &Exports) -> Result<RegisteredExport> { let identity = exports.register_trait(&self.0)?; Ok(RegisteredExport { identity: identity.clone(), interface: "Database", bundle_sha256: BUNDLE_SHA256, dispatcher: Arc::new(InterfaceDatabaseDispatch { exports: exports.clone(), identity }) }) } fn snapshot(&self) -> Result<BTreeMap<String, Outbound>> { Ok(BTreeMap::from([])) } }
#[allow(non_snake_case)] pub fn exportInterfaceDatabase(native: Arc<dyn InterfaceDatabaseService>) -> Outbound { Outbound::Own(Arc::new(InterfaceDatabaseExport(native))) }

#[allow(non_snake_case)] pub trait InterfaceSessionService: Send + Sync + 'static { fn agent(&self) -> Result<Arc<dyn InterfaceAgentService>>;
 }
#[derive(Clone)] pub struct InterfaceSessionClient(Client);
impl ClientHandle for InterfaceSessionClient { const INTERFACE: &'static str = "Session"; const BUNDLE_SHA256: &'static str = BUNDLE_SHA256; fn client(&self) -> &Client { &self.0 } fn from_client(client: Client) -> Self { Self(client) } }
#[allow(non_snake_case, unused_variables)] impl InterfaceSessionClient { pub fn agent(&self) -> Result<InterfaceAgentClient> { let caller = self.0.caller(); let value = self.0.property("agent")?; bind::<InterfaceAgentClient>(value, caller.clone()) }
 }
struct InterfaceSessionExport(Arc<dyn InterfaceSessionService>);
struct InterfaceSessionDispatch { exports: Exports, identity: rutis_protocol::identity::ObjectIdentity }
#[allow(unused_variables)] impl Dispatcher for InterfaceSessionDispatch { fn dispatch(&self, key: PinKey, context: CallContext, method: String, params: DecodedValue) -> RpcFuture<Outbound> { let exports = self.exports.clone(); let identity = self.identity.clone(); Box::pin(async move { let object = exports.execution_trait::<dyn InterfaceSessionService>(&key)?; if !exports.is_native(&identity, &object) { return Err(denied_method()); } let caller = context.caller(); Err(denied_method()) }) } }
impl NativeExport for InterfaceSessionExport { fn register(&self, exports: &Exports) -> Result<RegisteredExport> { let identity = exports.register_trait(&self.0)?; Ok(RegisteredExport { identity: identity.clone(), interface: "Session", bundle_sha256: BUNDLE_SHA256, dispatcher: Arc::new(InterfaceSessionDispatch { exports: exports.clone(), identity }) }) } fn snapshot(&self) -> Result<BTreeMap<String, Outbound>> { Ok(BTreeMap::from([("agent".into(), exportInterfaceAgent(self.0.agent()?))])) } }
#[allow(non_snake_case)] pub fn exportInterfaceSession(native: Arc<dyn InterfaceSessionService>) -> Outbound { Outbound::Own(Arc::new(InterfaceSessionExport(native))) }

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(crate = "rutis_protocol::sdk::serde", deny_unknown_fields )]
pub struct InterfaceConnectionMethod0Params {
#[serde(rename = "sql")] pub sql: String,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(crate = "rutis_protocol::sdk::serde", deny_unknown_fields )]
pub struct InterfaceDatabaseMethod0Params {
#[serde(rename = "name")] pub name: String,
}
