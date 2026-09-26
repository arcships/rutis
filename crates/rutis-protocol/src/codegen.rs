//! Deterministic Rust/TS bindings from an already admitted bundle. Generated
//! native adapters select declared members explicitly, never by reflection.
use crate::contract::{callback_key, AdmittedBundle, Interface, Method, TypeExpr};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt::Write;

#[derive(Clone, Copy)]
enum Mode {
    Client,
    Params,
    Result,
}
struct Generator {
    names: BTreeMap<String, String>,
    rust_defs: BTreeMap<String, String>,
    ts_defs: BTreeMap<String, String>,
}
fn quote(value: &str) -> String {
    serde_json::to_string(value).unwrap()
}
fn result(expression: String) -> String {
    match expression.strip_suffix('?') {
        Some(result) => result.into(),
        None => format!("Ok::<_, rutis_protocol::error::ProtocolError>({expression})"),
    }
}
fn mapper(expression: String) -> String {
    if let Some(function) = expression.strip_suffix("(item)") {
        if !function.contains('(') {
            return function.into();
        }
    }
    format!("|item| {expression}")
}
fn schema_fields(schema: &Value) -> Vec<(&String, &Value)> {
    let mut fields: Vec<_> = schema
        .get("properties")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .collect();
    fields.sort_by(|a, b| a.0.cmp(b.0));
    fields
}
fn symbol(value: &str) -> String {
    let mut result = String::from("field_");
    for byte in value.bytes() {
        write!(result, "{byte:02x}").unwrap();
    }
    result
}
fn member(value: &str) -> String {
    // Exact ASCII names avoid casing collisions (getURL/getUrl) and keywords.
    // Encoded identifiers are reserved for all names containing underscores.
    if value.bytes().all(|c| c.is_ascii_alphanumeric())
        && ![
            "self", "Self", "super", "crate", "type", "match", "loop", "move", "async", "await",
            "fn", "pub", "impl", "trait", "struct", "enum", "use", "where", "in", "ref", "const",
            "static", "mod", "let", "if", "else", "for", "while", "return", "break", "continue",
            "as", "dyn", "extern", "unsafe", "true", "false", "box", "do", "final", "macro",
            "override", "priv", "typeof", "unsized", "virtual", "yield", "try", "abstract",
            "become", "gen",
        ]
        .contains(&value)
    {
        value.into()
    } else {
        symbol(value)
    }
}
fn ts_member(value: &str) -> String {
    if value == "then" {
        "then$".into()
    } else {
        value.into()
    }
}
fn scan(expr: &TypeExpr, callbacks: &mut BTreeMap<String, Interface>) {
    match expr {
        TypeExpr::Callback { params, result, .. } => {
            callbacks
                .entry(callback_key(expr))
                .or_insert_with(|| Interface {
                    methods: BTreeMap::from([(
                        "call".into(),
                        Method {
                            params: *params.clone(),
                            result: *result.clone(),
                        },
                    )]),
                    properties: BTreeMap::new(),
                });
            scan(params, callbacks);
            scan(result, callbacks);
        }
        TypeExpr::Record { fields } => {
            for child in fields.values() {
                scan(child, callbacks);
            }
        }
        TypeExpr::List { item } | TypeExpr::Optional { item } => scan(item, callbacks),
        _ => {}
    }
}

impl Generator {
    fn schema_rust(&mut self, schema: &Value, root: &Value, hint: &str) -> String {
        if let Some(path) = schema.get("$ref").and_then(Value::as_str) {
            return self.schema_rust(root.pointer(&path[1..]).unwrap(), root, hint);
        }
        if let Some(branches) = schema.get("oneOf").and_then(Value::as_array) {
            if !self.rust_defs.contains_key(hint) {
                let variants = branches
                    .iter()
                    .enumerate()
                    .map(|(i, branch)| {
                        format!(
                            "V{i}({})",
                            self.schema_rust(branch, root, &format!("{hint}Branch{i}"))
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                self.rust_defs.insert(hint.into(), format!("#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)] #[serde(crate = \"rutis_protocol::sdk::serde\", untagged)] pub enum {hint} {{ {variants} }}\n"));
            }
            return hint.into();
        }
        let constants = schema
            .get("enum")
            .and_then(Value::as_array)
            .cloned()
            .or_else(|| schema.get("const").map(|v| vec![v.clone()]));
        if let Some(constants) = constants {
            if constants.iter().all(Value::is_string) {
                self.rust_defs.entry(hint.into()).or_insert_with(|| format!("#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)] #[serde(crate = \"rutis_protocol::sdk::serde\")] pub enum {hint} {{ {} }}\n", constants.iter().enumerate().map(|(i, v)| format!("#[serde(rename = {})] V{i}", quote(v.as_str().unwrap()))).collect::<Vec<_>>().join(",")));
                return hint.into();
            }
        }
        match schema.get("type").and_then(Value::as_str) {
            Some("string") => "String".into(),
            Some("boolean") => "bool".into(),
            Some("null") => "()".into(),
            Some("integer") => "i64".into(),
            Some("number") => "serde_json::Number".into(),
            Some("array") => format!(
                "Vec<{}>",
                self.schema_rust(
                    schema.get("items").unwrap_or(&Value::Null),
                    root,
                    &format!("{hint}Item")
                )
            ),
            Some("object")
                if schema
                    .get("properties")
                    .and_then(Value::as_object)
                    .is_some_and(|p| !p.is_empty()) =>
            {
                if self.rust_defs.contains_key(hint) {
                    return hint.into();
                }
                let mut fields = String::new();
                for (i, (name, child)) in schema_fields(schema).into_iter().enumerate() {
                    let ty = self.schema_rust(child, root, &format!("{hint}Field{i}"));
                    let required = schema["required"]
                        .as_array()
                        .is_some_and(|r| r.contains(&Value::String(name.clone())));
                    if !required {
                        fields.push_str("#[serde(default, skip_serializing_if = \"OptionalField::is_missing\")]\n");
                    }
                    writeln!(
                        fields,
                        "#[serde(rename = {})] pub {}: {},",
                        quote(name),
                        member(name),
                        if required {
                            ty
                        } else {
                            format!("OptionalField<{ty}>")
                        }
                    )
                    .unwrap();
                }
                let closed = schema.get("additionalProperties") == Some(&Value::Bool(false));
                if !closed {
                    fields.push_str("#[serde(default, flatten)] pub additional_fields: BTreeMap<String, serde_json::Value>,\n");
                }
                self.rust_defs.insert(hint.into(), format!("#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]\n#[serde(crate = \"rutis_protocol::sdk::serde\"{} )]\npub struct {hint} {{\n{fields}}}\n", if closed { ", deny_unknown_fields" } else { "" }));
                hint.into()
            }
            Some("object") => "BTreeMap<String, serde_json::Value>".into(),
            _ => "serde_json::Value".into(),
        }
    }
    fn schema_ts(schema: &Value, root: &Value) -> String {
        if let Some(path) = schema.get("$ref").and_then(Value::as_str) {
            return Self::schema_ts(root.pointer(&path[1..]).unwrap(), root);
        }
        if let Some(value) = schema.get("const") {
            return quote_value_type(value);
        }
        if let Some(values) = schema.get("enum").and_then(Value::as_array) {
            return values
                .iter()
                .map(quote_value_type)
                .collect::<Vec<_>>()
                .join(" | ");
        }
        if let Some(branches) = schema.get("oneOf").and_then(Value::as_array) {
            return branches
                .iter()
                .map(|s| Self::schema_ts(s, root))
                .collect::<Vec<_>>()
                .join(" | ");
        }
        match schema.get("type").and_then(Value::as_str) {
            Some("string") => "string".into(),
            Some("boolean") => "boolean".into(),
            Some("null") => "null".into(),
            Some("integer" | "number") => "number".into(),
            Some("array") => format!(
                "Array<{}>",
                Self::schema_ts(schema.get("items").unwrap_or(&Value::Null), root)
            ),
            Some("object") => {
                let mut fields = Vec::new();
                if schema
                    .get("properties")
                    .and_then(Value::as_object)
                    .is_some()
                {
                    for (name, child) in schema_fields(schema) {
                        let required = schema["required"]
                            .as_array()
                            .is_some_and(|r| r.contains(&Value::String(name.clone())));
                        fields.push(format!(
                            "{}{}: {}",
                            quote(name),
                            if required { "" } else { "?" },
                            Self::schema_ts(child, root)
                        ));
                    }
                }
                if schema.get("additionalProperties") != Some(&Value::Bool(false)) {
                    fields.push("[key: string]: unknown".into());
                }
                format!("{{ {} }}", fields.join("; "))
            }
            _ => "unknown".into(),
        }
    }
    fn rust_type(&mut self, expr: &TypeExpr, hint: &str, mode: Mode) -> String {
        match expr {
            TypeExpr::Value { schema } => self.schema_rust(schema, schema, hint),
            TypeExpr::Object { interface, .. } => match mode {
                Mode::Result => format!("Arc<dyn {}Service>", self.names[interface]),
                _ => format!("{}Client", self.names[interface]),
            },
            TypeExpr::Callback { .. } => match mode {
                Mode::Client => format!("Arc<dyn {}Service>", self.names[&callback_key(expr)]),
                _ => format!("{}Client", self.names[&callback_key(expr)]),
            },
            TypeExpr::List { item } => format!(
                "Vec<{}>",
                self.rust_type(item, &format!("{hint}Item"), mode)
            ),
            TypeExpr::Optional { item } => format!(
                "Option<{}>",
                self.rust_type(item, &format!("{hint}Item"), mode)
            ),
            TypeExpr::Record { fields } => {
                let suffix = match mode {
                    Mode::Client => "ClientRecord",
                    Mode::Params => "ParamRecord",
                    Mode::Result => "ResultRecord",
                };
                let name = format!("{hint}{suffix}");
                if !self.rust_defs.contains_key(&name) {
                    let mut content = String::new();
                    for (i, (field, expr)) in fields.iter().enumerate() {
                        writeln!(
                            content,
                            "pub {}: {},",
                            member(field),
                            self.rust_type(expr, &format!("{hint}Field{i}"), mode)
                        )
                        .unwrap();
                    }
                    self.rust_defs.insert(
                        name.clone(),
                        format!("#[derive(Clone)] pub struct {name} {{ {content} }}\n"),
                    );
                }
                name
            }
            TypeExpr::Stream { .. } => unreachable!("admission rejects streams"),
        }
    }
    fn ts_type(&mut self, expr: &TypeExpr, hint: &str, mode: Mode) -> String {
        match expr {
            TypeExpr::Value { schema } => {
                self.ts_defs.entry(hint.into()).or_insert_with(|| {
                    format!("export type {hint} = {}\n", Self::schema_ts(schema, schema))
                });
                hint.into()
            }
            TypeExpr::Object { interface, .. } => format!(
                "{}{}",
                self.names[interface],
                if matches!(mode, Mode::Result) {
                    "Service"
                } else {
                    ""
                }
            ),
            TypeExpr::Callback { .. } => format!(
                "{}{}",
                self.names[&callback_key(expr)],
                if matches!(mode, Mode::Client) {
                    "Service"
                } else {
                    ""
                }
            ),
            TypeExpr::List { item } => format!(
                "Array<{}>",
                self.ts_type(item, &format!("{hint}Item"), mode)
            ),
            TypeExpr::Optional { item } => format!(
                "({} | null)",
                self.ts_type(item, &format!("{hint}Item"), mode)
            ),
            TypeExpr::Record { fields } => format!(
                "{{ {} }}",
                fields
                    .iter()
                    .enumerate()
                    .map(|(i, (name, expr))| format!(
                        "{}: {}",
                        quote(name),
                        self.ts_type(expr, &format!("{hint}Field{i}"), mode)
                    ))
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
            TypeExpr::Stream { .. } => unreachable!(),
        }
    }
    fn encode_rust(&self, expr: &TypeExpr, mode: Mode, value: &str) -> String {
        match expr {
            TypeExpr::Value { .. } => format!("Outbound::json({value})?"),
            TypeExpr::Object { interface, .. } if matches!(mode, Mode::Result) => format!("export{}({value})", self.names[interface]),
            TypeExpr::Object { .. } => format!("Outbound::Foreign(ClientHandle::client(&({value})).proxy().clone())"),
            TypeExpr::Callback { .. } => format!("export{}({value})", self.names[&callback_key(expr)]),
            TypeExpr::List { item } => format!("Outbound::List(({value}).into_iter().map({}).collect::<Result<Vec<_>>>()?)", mapper(result(self.encode_rust(item, mode, "item")))),
            TypeExpr::Optional { item } => format!("Outbound::Optional(({value}).map(|item| Ok::<_, rutis_protocol::error::ProtocolError>(Box::new({}))).transpose()?)", self.encode_rust(item, mode, "item")),
            TypeExpr::Record { fields } => format!("Outbound::Record(BTreeMap::from([{}]))", fields.iter().map(|(name, expr)| format!("({}.into(), {})", quote(name), self.encode_rust(expr, mode, &format!("({value}).{}", member(name))))).collect::<Vec<_>>().join(",")),
            _ => unreachable!(),
        }
    }
    fn decode_rust(&mut self, expr: &TypeExpr, hint: &str, mode: Mode, value: &str) -> String {
        let ty = self.rust_type(expr, hint, mode);
        match expr {
            TypeExpr::Value { .. } => format!("json::<{ty}>({value})?"),
            TypeExpr::Object { .. } | TypeExpr::Callback { .. } => {
                format!("bind::<{ty}>({value}, caller.clone())?")
            }
            TypeExpr::List { item } => format!(
                "list({value})?.into_iter().map({}).collect::<Result<Vec<_>>>()?",
                mapper(result(self.decode_rust(
                    item,
                    &format!("{hint}Item"),
                    mode,
                    "item"
                )))
            ),
            TypeExpr::Optional { item } => format!(
                "optional({value})?.map(|item| {}).transpose()?",
                result(self.decode_rust(item, &format!("{hint}Item"), mode, "item"))
            ),
            TypeExpr::Record { fields } => format!(
                "{{ let mut fields = record({value})?; {ty} {{ {} }} }}",
                fields
                    .iter()
                    .enumerate()
                    .map(|(i, (name, expr))| format!(
                        "{}: {}",
                        member(name),
                        self.decode_rust(
                            expr,
                            &format!("{hint}Field{i}"),
                            mode,
                            &format!("field(&mut fields, {})?", quote(name))
                        )
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            _ => unreachable!(),
        }
    }
    fn encode_ts(&self, expr: &TypeExpr, mode: Mode, value: &str) -> String {
        match expr {
            TypeExpr::Value { .. } => format!("json({value})"),
            TypeExpr::Object { interface, .. } if matches!(mode, Mode::Result) => {
                format!("export{}({value})", self.names[interface])
            }
            TypeExpr::Object { .. } => format!("foreign({value})"),
            TypeExpr::Callback { .. } => {
                format!("export{}({value})", self.names[&callback_key(expr)])
            }
            TypeExpr::List { item } => format!(
                "{{ kind: 'list' as const, items: ({value}).map(item => ({})) }}",
                self.encode_ts(item, mode, "item")
            ),
            TypeExpr::Optional { item } => format!(
                "{{ kind: 'optional' as const, value: ({value}) === null ? null : ({}) }}",
                self.encode_ts(item, mode, value)
            ),
            TypeExpr::Record { fields } => format!(
                "{{ kind: 'record' as const, fields: {{ {} }} }}",
                fields
                    .iter()
                    .map(|(name, expr)| format!(
                        "{}: ({})",
                        quote(name),
                        self.encode_ts(expr, mode, &format!("({value})[{}]", quote(name)))
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            _ => unreachable!(),
        }
    }
    fn decode_ts(&mut self, expr: &TypeExpr, hint: &str, mode: Mode, value: &str) -> String {
        let ty = self.ts_type(expr, hint, mode);
        match expr {
            TypeExpr::Value { .. } => format!("({value}) as {ty}"),
            TypeExpr::Object { interface, .. } => {
                format!("bind{}(object({value}), caller)", self.names[interface])
            }
            TypeExpr::Callback { .. } => format!(
                "bind{}(object({value}), caller)",
                self.names[&callback_key(expr)]
            ),
            TypeExpr::List { item } => format!(
                "({value} as unknown[]).map(item => ({}))",
                self.decode_ts(item, &format!("{hint}Item"), mode, "item")
            ),
            TypeExpr::Optional { item } => format!(
                "({value}) === null ? null : ({})",
                self.decode_ts(item, &format!("{hint}Item"), mode, value)
            ),
            TypeExpr::Record { fields } => format!(
                "{{ {} }}",
                fields
                    .iter()
                    .enumerate()
                    .map(|(i, (name, expr))| format!(
                        "{}: ({})",
                        quote(name),
                        self.decode_ts(
                            expr,
                            &format!("{hint}Field{i}"),
                            mode,
                            &format!("({value} as Record<string, unknown>)[{}]", quote(name))
                        )
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            _ => unreachable!(),
        }
    }
}
fn quote_value_type(value: &Value) -> String {
    match value {
        Value::String(_) | Value::Bool(_) | Value::Number(_) | Value::Null => value.to_string(),
        _ => "unknown".into(),
    }
}

pub struct Bindings {
    pub rust: String,
    pub typescript: String,
}
pub fn generate(bundle: &AdmittedBundle) -> Bindings {
    let mut interfaces = bundle.bundle().interfaces.clone();
    let mut callbacks = BTreeMap::new();
    for iface in interfaces.values() {
        for method in iface.methods.values() {
            scan(&method.params, &mut callbacks);
            scan(&method.result, &mut callbacks);
        }
    }
    let mut names: BTreeMap<String, String> = interfaces
        .keys()
        .map(|name| {
            (
                name.clone(),
                if name.bytes().all(|c| c.is_ascii_alphanumeric()) {
                    format!("Interface{name}")
                } else {
                    format!("Interface{}", symbol(name))
                },
            )
        })
        .collect();
    for (i, name) in callbacks.keys().enumerate() {
        names.insert(name.clone(), format!("BorrowCallback{i}"));
    }
    interfaces.extend(callbacks);
    let mut g = Generator {
        names,
        rust_defs: BTreeMap::new(),
        ts_defs: BTreeMap::new(),
    };
    let mut rust = format!("// Generated from raw bundle SHA-256 {}. Regenerate; do not edit.\nuse rutis_protocol::sdk::*;\nuse rutis_protocol::{{error::Result, exports::{{Exports, PinKey}}, graph::DecodedValue}};\nuse std::{{sync::Arc, collections::BTreeMap}};\npub const BUNDLE_SHA256: &str = {};\n", bundle.sha256(), quote(bundle.sha256()));
    let mut ts = format!("// Generated from raw bundle SHA-256 {}. Regenerate; do not edit.\nimport {{ Client, CallContext, facade, object, json, foreign, deniedMethod, type Caller, type Outbound }} from '../src/sdk.ts'\nimport {{ ObjectProxy }} from '../src/imports.ts'\nimport {{ Exports, type PinKey }} from '../src/exports.ts'\nexport const BUNDLE_SHA256 = {}\n", bundle.sha256(), quote(bundle.sha256()));
    for (interface, iface) in interfaces {
        let name = g.names[&interface].clone();
        let mut trait_methods = String::new();
        let mut client_methods = String::new();
        let mut dispatch = String::new();
        let mut snapshot = Vec::new();
        let mut ts_service = String::new();
        let mut ts_interface = String::new();
        let mut ts_methods = Vec::new();
        let mut ts_properties = Vec::new();
        let mut ts_dispatch = String::new();
        let mut ts_snapshot = Vec::new();
        for (i, (method, contract)) in iface.methods.iter().enumerate() {
            let mh = format!("{name}Method{i}");
            let ph = format!("{mh}Params");
            let rh = format!("{mh}Result");
            let selector = member(method);
            let tm = quote(&ts_member(method));
            let raw = quote(method);
            let input = g.rust_type(&contract.params, &ph, Mode::Client);
            let native_input = g.rust_type(&contract.params, &ph, Mode::Params);
            let output = g.rust_type(&contract.result, &rh, Mode::Params);
            let native_output = g.rust_type(&contract.result, &rh, Mode::Result);
            writeln!(trait_methods, "fn {selector}(&self, context: CallContext, params: {native_input}) -> RpcFuture<{native_output}>;").unwrap();
            writeln!(client_methods, "pub async fn {selector}(&self, params: {input}) -> Result<{output}> {{ let caller = self.0.caller(); let value = self.0.call({raw}, {}).await?; {} }}", g.encode_rust(&contract.params, Mode::Client, "params"), result(g.decode_rust(&contract.result, &rh, Mode::Params, "value"))).unwrap();
            let decoded = g.decode_rust(&contract.params, &ph, Mode::Params, "params");
            let (param_binding, param_value) = if native_input == "()" {
                (format!("{decoded};"), "()")
            } else {
                (format!("let params = {decoded};"), "params")
            };
            let (value_binding, value) = if native_output == "()" {
                ("", "()")
            } else {
                ("let value = ", "value")
            };
            writeln!(dispatch, "{raw} => {{ {param_binding} {value_binding}object.{selector}(context, {param_value}).await?; {} }},", result(g.encode_rust(&contract.result, Mode::Result, value))).unwrap();
            let ti = g.ts_type(&contract.params, &ph, Mode::Client);
            let tni = g.ts_type(&contract.params, &ph, Mode::Params);
            let to = g.ts_type(&contract.result, &rh, Mode::Params);
            let tno = g.ts_type(&contract.result, &rh, Mode::Result);
            writeln!(ts_interface, "{tm}(params: {ti}): Promise<{to}>").unwrap();
            writeln!(
                ts_service,
                "{raw}(context: CallContext, params: {tni}): Promise<{tno}>"
            )
            .unwrap();
            ts_methods.push(format!("{tm}: async (params: {ti}) => {{ const value = await client.call({raw}, {}); return ({}) }}", g.encode_ts(&contract.params, Mode::Client, "params"), g.decode_ts(&contract.result, &rh, Mode::Params, "value")));
            writeln!(ts_dispatch, "case {raw}: {{ const input = {}; const value = await native[{raw}](context, input); return ({}) }}", g.decode_ts(&contract.params, &ph, Mode::Params, "params"), g.encode_ts(&contract.result, Mode::Result, "value")).unwrap();
        }
        for (i, (property, expr)) in iface.properties.iter().enumerate() {
            let hint = format!("{name}Property{i}");
            let selector = member(property);
            let raw = quote(property);
            let tm = quote(&ts_member(property));
            let output = g.rust_type(expr, &hint, Mode::Params);
            let native_output = g.rust_type(expr, &hint, Mode::Result);
            writeln!(
                trait_methods,
                "fn {selector}(&self) -> Result<{native_output}>;"
            )
            .unwrap();
            writeln!(client_methods, "pub fn {selector}(&self) -> Result<{output}> {{ let caller = self.0.caller(); let value = self.0.property({raw})?; {} }}", result(g.decode_rust(expr, &hint, Mode::Params, "value"))).unwrap();
            snapshot.push(format!(
                "({raw}.into(), {})",
                g.encode_rust(expr, Mode::Result, &format!("self.0.{selector}()?"))
            ));
            writeln!(
                ts_interface,
                "readonly {tm}: {}",
                g.ts_type(expr, &hint, Mode::Params)
            )
            .unwrap();
            writeln!(
                ts_service,
                "readonly {raw}: {}",
                g.ts_type(expr, &hint, Mode::Result)
            )
            .unwrap();
            ts_properties.push(format!(
                "{tm}: () => ({})",
                g.decode_ts(
                    expr,
                    &hint,
                    Mode::Params,
                    &format!("client.property({raw})")
                )
            ));
            ts_snapshot.push(format!(
                "{raw}: ({})",
                g.encode_ts(expr, Mode::Result, &format!("native[{raw}]"))
            ));
        }
        let dispatch_body = if dispatch.is_empty() {
            "Err(denied_method())".into()
        } else {
            format!("match method.as_str() {{ {dispatch} _ => Err(denied_method()) }}")
        };
        writeln!(rust, "#[allow(non_snake_case)] pub trait {name}Service: Send + Sync + 'static {{ {trait_methods} }}\n#[derive(Clone)] pub struct {name}Client(Client);\nimpl ClientHandle for {name}Client {{ const INTERFACE: &'static str = {}; const BUNDLE_SHA256: &'static str = BUNDLE_SHA256; fn client(&self) -> &Client {{ &self.0 }} fn from_client(client: Client) -> Self {{ Self(client) }} }}\n#[allow(non_snake_case, unused_variables)] impl {name}Client {{ {client_methods} }}\nstruct {name}Export(Arc<dyn {name}Service>);\nstruct {name}Dispatch {{ exports: Exports, identity: rutis_protocol::identity::ObjectIdentity }}\n#[allow(unused_variables)] impl Dispatcher for {name}Dispatch {{ fn dispatch(&self, key: PinKey, context: CallContext, method: String, params: DecodedValue) -> RpcFuture<Outbound> {{ let exports = self.exports.clone(); let identity = self.identity.clone(); Box::pin(async move {{ let object = exports.execution_trait::<dyn {name}Service>(&key)?; if !exports.is_native(&identity, &object) {{ return Err(denied_method()); }} let caller = context.caller(); {dispatch_body} }}) }} }}\nimpl NativeExport for {name}Export {{ fn register(&self, exports: &Exports) -> Result<RegisteredExport> {{ let identity = exports.register_trait(&self.0)?; Ok(RegisteredExport {{ identity: identity.clone(), interface: {}, bundle_sha256: BUNDLE_SHA256, dispatcher: Arc::new({name}Dispatch {{ exports: exports.clone(), identity }}) }}) }} fn snapshot(&self) -> Result<BTreeMap<String, Outbound>> {{ Ok(BTreeMap::from([{}])) }} }}\n#[allow(non_snake_case)] pub fn export{name}(native: Arc<dyn {name}Service>) -> Outbound {{ Outbound::Own(Arc::new({name}Export(native))) }}\n", quote(&interface), quote(&interface), snapshot.join(",")).unwrap();
        writeln!(ts, "export interface {name} {{ {ts_interface} }}\nexport interface {name}Service {{ {ts_service} }}\nexport function bind{name}(proxy: ObjectProxy, caller: Caller): {name} {{ const client = new Client(proxy, caller, BUNDLE_SHA256, {}); return facade<{name}>(client, {{ {} }}, {{ {} }}) }}\nclass {name}Dispatch {{ constructor(private exports: Exports, private source: WeakRef<{name}Service>) {{}} async dispatch(key: PinKey, context: CallContext, method: string, params: unknown): Promise<Outbound> {{ const native = this.exports.executionObject(key) as {name}Service; if (native !== this.source.deref()) throw deniedMethod(); const caller = context.caller; switch (method) {{ {ts_dispatch} default: throw deniedMethod() }} }} }}\nexport function export{name}(native: {name}Service): Outbound {{ return {{ kind: 'own', value: {{ register(exports) {{ const identity = exports.register(native); return {{ identity, interface: {}, bundleSha256: BUNDLE_SHA256, dispatcher: new {name}Dispatch(exports, new WeakRef(native)) }} }}, snapshot() {{ return {{ {} }} }} }} }} }}\n", quote(&interface), ts_methods.join(","), ts_properties.join(","), quote(&interface), ts_snapshot.join(",")).unwrap();
    }
    for definition in g.rust_defs.values() {
        rust.push_str(definition);
    }
    for definition in g.ts_defs.values() {
        ts.push_str(definition);
    }
    Bindings {
        rust,
        typescript: ts,
    }
}
