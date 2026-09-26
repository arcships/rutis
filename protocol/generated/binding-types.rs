// Generated from raw bundle SHA-256 85761ee1bd6171c18bb18219fd31912097241bda151f82f565d05af47b8d0b60. Regenerate; do not edit.
use rutis_protocol::sdk::*;
use rutis_protocol::{error::Result, exports::{Exports, PinKey}, graph::DecodedValue};
use std::{sync::Arc, collections::BTreeMap};
pub const BUNDLE_SHA256: &str = "85761ee1bd6171c18bb18219fd31912097241bda151f82f565d05af47b8d0b60";
#[allow(non_snake_case)] pub trait BorrowCallback0Service: Send + Sync + 'static { fn call(&self, context: CallContext, params: Option<Vec<String>>) -> RpcFuture<()>;
 }
#[derive(Clone)] pub struct BorrowCallback0Client(Client);
impl ClientHandle for BorrowCallback0Client { const INTERFACE: &'static str = "$callback:9df9212785473f83e272628f613c6fb6b5a5ab42625e45001e6566cd89ee272f"; const BUNDLE_SHA256: &'static str = BUNDLE_SHA256; fn client(&self) -> &Client { &self.0 } fn from_client(client: Client) -> Self { Self(client) } }
#[allow(non_snake_case, unused_variables)] impl BorrowCallback0Client { pub async fn call(&self, params: Option<Vec<String>>) -> Result<()> { let caller = self.0.caller(); let value = self.0.call("call", Outbound::Optional((params).map(|item| Ok::<_, rutis_protocol::error::ProtocolError>(Box::new(Outbound::List((item).into_iter().map(Outbound::json).collect::<Result<Vec<_>>>()?)))).transpose()?)).await?; json::<()>(value) }
 }
struct BorrowCallback0Export(Arc<dyn BorrowCallback0Service>);
struct BorrowCallback0Dispatch { exports: Exports, identity: rutis_protocol::identity::ObjectIdentity }
#[allow(unused_variables)] impl Dispatcher for BorrowCallback0Dispatch { fn dispatch(&self, key: PinKey, context: CallContext, method: String, params: DecodedValue) -> RpcFuture<Outbound> { let exports = self.exports.clone(); let identity = self.identity.clone(); Box::pin(async move { let object = exports.execution_trait::<dyn BorrowCallback0Service>(&key)?; if !exports.is_native(&identity, &object) { return Err(denied_method()); } let caller = context.caller(); match method.as_str() { "call" => { let params = optional(params)?.map(|item| list(item)?.into_iter().map(json::<String>).collect::<Result<Vec<_>>>()).transpose()?; object.call(context, params).await?; Outbound::json(()) },
 _ => Err(denied_method()) } }) } }
impl NativeExport for BorrowCallback0Export { fn register(&self, exports: &Exports) -> Result<RegisteredExport> { let identity = exports.register_trait(&self.0)?; Ok(RegisteredExport { identity: identity.clone(), interface: "$callback:9df9212785473f83e272628f613c6fb6b5a5ab42625e45001e6566cd89ee272f", bundle_sha256: BUNDLE_SHA256, dispatcher: Arc::new(BorrowCallback0Dispatch { exports: exports.clone(), identity }) }) } fn snapshot(&self) -> Result<BTreeMap<String, Outbound>> { Ok(BTreeMap::from([])) } }
#[allow(non_snake_case)] pub fn exportBorrowCallback0(native: Arc<dyn BorrowCallback0Service>) -> Outbound { Outbound::Own(Arc::new(BorrowCallback0Export(native))) }

#[allow(non_snake_case)] pub trait InterfaceResourceService: Send + Sync + 'static { fn client(&self, context: CallContext, params: InterfaceResourceClient) -> RpcFuture<bool>;
 }
#[derive(Clone)] pub struct InterfaceResourceClient(Client);
impl ClientHandle for InterfaceResourceClient { const INTERFACE: &'static str = "Resource"; const BUNDLE_SHA256: &'static str = BUNDLE_SHA256; fn client(&self) -> &Client { &self.0 } fn from_client(client: Client) -> Self { Self(client) } }
#[allow(non_snake_case, unused_variables)] impl InterfaceResourceClient { pub async fn client(&self, params: InterfaceResourceClient) -> Result<bool> { let caller = self.0.caller(); let value = self.0.call("client", Outbound::Foreign(ClientHandle::client(&(params)).proxy().clone())).await?; json::<bool>(value) }
 }
struct InterfaceResourceExport(Arc<dyn InterfaceResourceService>);
struct InterfaceResourceDispatch { exports: Exports, identity: rutis_protocol::identity::ObjectIdentity }
#[allow(unused_variables)] impl Dispatcher for InterfaceResourceDispatch { fn dispatch(&self, key: PinKey, context: CallContext, method: String, params: DecodedValue) -> RpcFuture<Outbound> { let exports = self.exports.clone(); let identity = self.identity.clone(); Box::pin(async move { let object = exports.execution_trait::<dyn InterfaceResourceService>(&key)?; if !exports.is_native(&identity, &object) { return Err(denied_method()); } let caller = context.caller(); match method.as_str() { "client" => { let params = bind::<InterfaceResourceClient>(params, caller.clone())?; let value = object.client(context, params).await?; Outbound::json(value) },
 _ => Err(denied_method()) } }) } }
impl NativeExport for InterfaceResourceExport { fn register(&self, exports: &Exports) -> Result<RegisteredExport> { let identity = exports.register_trait(&self.0)?; Ok(RegisteredExport { identity: identity.clone(), interface: "Resource", bundle_sha256: BUNDLE_SHA256, dispatcher: Arc::new(InterfaceResourceDispatch { exports: exports.clone(), identity }) }) } fn snapshot(&self) -> Result<BTreeMap<String, Outbound>> { Ok(BTreeMap::from([])) } }
#[allow(non_snake_case)] pub fn exportInterfaceResource(native: Arc<dyn InterfaceResourceService>) -> Outbound { Outbound::Own(Arc::new(InterfaceResourceExport(native))) }

#[allow(non_snake_case)] pub trait InterfaceValuesService: Send + Sync + 'static { fn compose(&self, context: CallContext, params: InterfaceValuesMethod0ParamsParamRecord) -> RpcFuture<Option<Vec<Arc<dyn InterfaceResourceService>>>>;
fn dto(&self, context: CallContext, params: InterfaceValuesMethod1Params) -> RpcFuture<InterfaceValuesMethod1Result>;
fn then(&self, context: CallContext, params: String) -> RpcFuture<String>;
 }
#[derive(Clone)] pub struct InterfaceValuesClient(Client);
impl ClientHandle for InterfaceValuesClient { const INTERFACE: &'static str = "Values"; const BUNDLE_SHA256: &'static str = BUNDLE_SHA256; fn client(&self) -> &Client { &self.0 } fn from_client(client: Client) -> Self { Self(client) } }
#[allow(non_snake_case, unused_variables)] impl InterfaceValuesClient { pub async fn compose(&self, params: InterfaceValuesMethod0ParamsClientRecord) -> Result<Option<Vec<InterfaceResourceClient>>> { let caller = self.0.caller(); let value = self.0.call("compose", Outbound::Record(BTreeMap::from([("callback".into(), Outbound::Optional(((params).callback).map(|item| Ok::<_, rutis_protocol::error::ProtocolError>(Box::new(exportBorrowCallback0(item)))).transpose()?)),("items".into(), Outbound::List(((params).items).into_iter().map(|item| Ok::<_, rutis_protocol::error::ProtocolError>(Outbound::Foreign(ClientHandle::client(&(item)).proxy().clone()))).collect::<Result<Vec<_>>>()?)),("type".into(), Outbound::json((params).field_74797065)?)]))).await?; optional(value)?.map(|item| list(item)?.into_iter().map(|item| bind::<InterfaceResourceClient>(item, caller.clone())).collect::<Result<Vec<_>>>()).transpose() }
pub async fn dto(&self, params: InterfaceValuesMethod1Params) -> Result<InterfaceValuesMethod1Result> { let caller = self.0.caller(); let value = self.0.call("dto", Outbound::json(params)?).await?; json::<InterfaceValuesMethod1Result>(value) }
pub async fn then(&self, params: String) -> Result<String> { let caller = self.0.caller(); let value = self.0.call("then", Outbound::json(params)?).await?; json::<String>(value) }
 }
struct InterfaceValuesExport(Arc<dyn InterfaceValuesService>);
struct InterfaceValuesDispatch { exports: Exports, identity: rutis_protocol::identity::ObjectIdentity }
#[allow(unused_variables)] impl Dispatcher for InterfaceValuesDispatch { fn dispatch(&self, key: PinKey, context: CallContext, method: String, params: DecodedValue) -> RpcFuture<Outbound> { let exports = self.exports.clone(); let identity = self.identity.clone(); Box::pin(async move { let object = exports.execution_trait::<dyn InterfaceValuesService>(&key)?; if !exports.is_native(&identity, &object) { return Err(denied_method()); } let caller = context.caller(); match method.as_str() { "compose" => { let params = { let mut fields = record(params)?; InterfaceValuesMethod0ParamsParamRecord { callback: optional(field(&mut fields, "callback")?)?.map(|item| bind::<BorrowCallback0Client>(item, caller.clone())).transpose()?,items: list(field(&mut fields, "items")?)?.into_iter().map(|item| bind::<InterfaceResourceClient>(item, caller.clone())).collect::<Result<Vec<_>>>()?,field_74797065: json::<serde_json::Number>(field(&mut fields, "type")?)? } }; let value = object.compose(context, params).await?; Ok::<_, rutis_protocol::error::ProtocolError>(Outbound::Optional((value).map(|item| Ok::<_, rutis_protocol::error::ProtocolError>(Box::new(Outbound::List((item).into_iter().map(|item| Ok::<_, rutis_protocol::error::ProtocolError>(exportInterfaceResource(item))).collect::<Result<Vec<_>>>()?)))).transpose()?)) },
"dto" => { let params = json::<InterfaceValuesMethod1Params>(params)?; let value = object.dto(context, params).await?; Outbound::json(value) },
"then" => { let params = json::<String>(params)?; let value = object.then(context, params).await?; Outbound::json(value) },
 _ => Err(denied_method()) } }) } }
impl NativeExport for InterfaceValuesExport { fn register(&self, exports: &Exports) -> Result<RegisteredExport> { let identity = exports.register_trait(&self.0)?; Ok(RegisteredExport { identity: identity.clone(), interface: "Values", bundle_sha256: BUNDLE_SHA256, dispatcher: Arc::new(InterfaceValuesDispatch { exports: exports.clone(), identity }) }) } fn snapshot(&self) -> Result<BTreeMap<String, Outbound>> { Ok(BTreeMap::from([])) } }
#[allow(non_snake_case)] pub fn exportInterfaceValues(native: Arc<dyn InterfaceValuesService>) -> Outbound { Outbound::Own(Arc::new(InterfaceValuesExport(native))) }

#[derive(Clone)] pub struct InterfaceValuesMethod0ParamsClientRecord { pub callback: Option<Arc<dyn BorrowCallback0Service>>,
pub items: Vec<InterfaceResourceClient>,
pub field_74797065: serde_json::Number,
 }
#[derive(Clone)] pub struct InterfaceValuesMethod0ParamsParamRecord { pub callback: Option<BorrowCallback0Client>,
pub items: Vec<InterfaceResourceClient>,
pub field_74797065: serde_json::Number,
 }
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(crate = "rutis_protocol::sdk::serde", deny_unknown_fields )]
pub struct InterfaceValuesMethod1Params {
#[serde(rename = "choice")] pub choice: InterfaceValuesMethod1ParamsField0,
#[serde(rename = "labels")] pub labels: Vec<InterfaceValuesMethod1ParamsField1Item>,
#[serde(default, skip_serializing_if = "OptionalField::is_missing")]
#[serde(rename = "nullable")] pub nullable: OptionalField<()>,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)] #[serde(crate = "rutis_protocol::sdk::serde", untagged)] pub enum InterfaceValuesMethod1ParamsField0 { V0(InterfaceValuesMethod1ParamsField0Branch0),V1(InterfaceValuesMethod1ParamsField0Branch1) }
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(crate = "rutis_protocol::sdk::serde", deny_unknown_fields )]
pub struct InterfaceValuesMethod1ParamsField0Branch0 {
#[serde(rename = "tag")] pub tag: InterfaceValuesMethod1ParamsField0Branch0Field0,
#[serde(rename = "text")] pub text: String,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)] #[serde(crate = "rutis_protocol::sdk::serde")] pub enum InterfaceValuesMethod1ParamsField0Branch0Field0 { #[serde(rename = "first")] V0 }
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(crate = "rutis_protocol::sdk::serde", deny_unknown_fields )]
pub struct InterfaceValuesMethod1ParamsField0Branch1 {
#[serde(rename = "count")] pub count: i64,
#[serde(rename = "tag")] pub tag: InterfaceValuesMethod1ParamsField0Branch1Field1,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)] #[serde(crate = "rutis_protocol::sdk::serde")] pub enum InterfaceValuesMethod1ParamsField0Branch1Field1 { #[serde(rename = "second")] V0 }
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)] #[serde(crate = "rutis_protocol::sdk::serde")] pub enum InterfaceValuesMethod1ParamsField1Item { #[serde(rename = "red")] V0,#[serde(rename = "green")] V1 }
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(crate = "rutis_protocol::sdk::serde", deny_unknown_fields )]
pub struct InterfaceValuesMethod1Result {
#[serde(rename = "choice")] pub choice: InterfaceValuesMethod1ResultField0,
#[serde(rename = "labels")] pub labels: Vec<InterfaceValuesMethod1ResultField1Item>,
#[serde(default, skip_serializing_if = "OptionalField::is_missing")]
#[serde(rename = "nullable")] pub nullable: OptionalField<()>,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)] #[serde(crate = "rutis_protocol::sdk::serde", untagged)] pub enum InterfaceValuesMethod1ResultField0 { V0(InterfaceValuesMethod1ResultField0Branch0),V1(InterfaceValuesMethod1ResultField0Branch1) }
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(crate = "rutis_protocol::sdk::serde", deny_unknown_fields )]
pub struct InterfaceValuesMethod1ResultField0Branch0 {
#[serde(rename = "tag")] pub tag: InterfaceValuesMethod1ResultField0Branch0Field0,
#[serde(rename = "text")] pub text: String,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)] #[serde(crate = "rutis_protocol::sdk::serde")] pub enum InterfaceValuesMethod1ResultField0Branch0Field0 { #[serde(rename = "first")] V0 }
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(crate = "rutis_protocol::sdk::serde", deny_unknown_fields )]
pub struct InterfaceValuesMethod1ResultField0Branch1 {
#[serde(rename = "count")] pub count: i64,
#[serde(rename = "tag")] pub tag: InterfaceValuesMethod1ResultField0Branch1Field1,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)] #[serde(crate = "rutis_protocol::sdk::serde")] pub enum InterfaceValuesMethod1ResultField0Branch1Field1 { #[serde(rename = "second")] V0 }
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)] #[serde(crate = "rutis_protocol::sdk::serde")] pub enum InterfaceValuesMethod1ResultField1Item { #[serde(rename = "red")] V0,#[serde(rename = "green")] V1 }
