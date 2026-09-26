//! Descriptor admission and data validation. Object references have their own
//! tagged encoding; JSON Schema never turns an ordinary JSON id into a grant.
use crate::error::{ErrorCode, ProtocolError, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const FAMILY: &str = "rutis-cordis-objects";
pub const VERSION: &str = "0.experimental";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bundle {
    pub id: String,
    pub version: String,
    pub interfaces: BTreeMap<String, Interface>,
    #[serde(default)]
    pub events: BTreeMap<String, EventContract>,
    #[serde(default)]
    pub required_capabilities: BTreeSet<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Interface {
    pub methods: BTreeMap<String, Method>,
    #[serde(default)]
    pub properties: BTreeMap<String, TypeExpr>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Method {
    pub params: TypeExpr,
    pub result: TypeExpr,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Ownership {
    Scope,
    Borrow,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TypeExpr {
    Value {
        schema: Value,
    },
    Object {
        interface: String,
        ownership: Ownership,
    },
    Record {
        fields: BTreeMap<String, TypeExpr>,
    },
    List {
        item: Box<TypeExpr>,
    },
    Optional {
        item: Box<TypeExpr>,
    },
    Callback {
        params: Box<TypeExpr>,
        result: Box<TypeExpr>,
        ownership: Ownership,
    },
    Stream {
        item: Box<TypeExpr>,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventMode {
    Parallel,
    Serial,
    Emit,
    Bail,
    Waterfall,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventContract {
    pub params: TypeExpr,
    pub result: TypeExpr,
    pub modes: Vec<EventMode>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WireValue {
    Value { value: Value },
    Ref { index: usize },
    Record { fields: BTreeMap<String, WireValue> },
    List { items: Vec<WireValue> },
    Optional { value: Option<Box<WireValue>> },
}

/// Callback signature includes its ownership and complete value/object types.
/// The canonical signature encoding is independent of JSON serializer order.
pub fn callback_key(expr: &TypeExpr) -> String {
    let value = serde_json::to_value(expr).expect("type expression is serializable");
    format!(
        "$callback:{:x}",
        Sha256::digest(crate::json::canonical(&value))
    )
}

#[derive(Debug)]
pub struct AdmittedBundle {
    pub(crate) bundle: Bundle,
    pub(crate) sha256: String,
}

fn invalid(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::InvalidParams, "contract", message)
}
fn unsupported(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::UnsupportedCapability, "prepare", message)
}

pub fn identifier(value: &str) -> bool {
    !value.is_empty()
        && !matches!(value, "__proto__" | "prototype" | "constructor")
        && value.bytes().enumerate().all(|(i, c)| {
            c.is_ascii_alphabetic()
                || c == b'_'
                || (i > 0 && (c.is_ascii_digit() || c == b'.' || c == b'-' || c == b'/'))
        })
}

impl AdmittedBundle {
    pub fn bundle(&self) -> &Bundle {
        &self.bundle
    }

    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let value = crate::json::decode(bytes)?;
        if let Some(capabilities) = value.get("required_capabilities").and_then(Value::as_array) {
            if capabilities
                .iter()
                .map(crate::json::canonical)
                .collect::<BTreeSet<_>>()
                .len()
                != capabilities.len()
            {
                return Err(invalid("duplicate required capability"));
            }
        }
        let bundle: Bundle = serde_json::from_value(value).map_err(|e| invalid(e.to_string()))?;
        if !identifier(&bundle.id)
            || !valid_version(&bundle.version)
            || bundle.interfaces.is_empty()
        {
            return Err(invalid("bundle identity or interfaces invalid"));
        }
        for capability in &bundle.required_capabilities {
            if !matches!(
                capability.as_str(),
                "object.scope" | "callback.borrow" | "event.parallel" | "event.serial"
            ) {
                return Err(unsupported(format!(
                    "capability {capability} is not implemented"
                )));
            }
        }
        for (name, iface) in &bundle.interfaces {
            if !identifier(name) {
                return Err(invalid("invalid interface name"));
            }
            for (method, contract) in &iface.methods {
                if !identifier(method) || iface.properties.contains_key(method) {
                    return Err(invalid("invalid or duplicate member"));
                }
                check_type(&contract.params, &bundle, false)?;
                check_type(&contract.result, &bundle, true)?;
            }
            for (property, contract) in &iface.properties {
                if !identifier(property) {
                    return Err(invalid("invalid property name"));
                }
                check_type(contract, &bundle, true)?;
                if contains_callback(contract) {
                    return Err(unsupported("callbacks cannot be snapshot properties"));
                }
            }
        }
        for (name, event) in &bundle.events {
            if !identifier(name) || event.modes.is_empty() {
                return Err(invalid("invalid event"));
            }
            if event
                .modes
                .iter()
                .enumerate()
                .any(|(i, mode)| event.modes[..i].contains(mode))
            {
                return Err(invalid("duplicate event mode"));
            }
            for mode in &event.modes {
                if !matches!(mode, EventMode::Parallel | EventMode::Serial) {
                    return Err(unsupported(
                        "event mode requires an extension or an explicit asynchronous migration",
                    ));
                }
            }
            check_type(&event.params, &bundle, true)?;
            check_type(&event.result, &bundle, true)?;
        }
        Ok(Self {
            bundle,
            sha256: format!("{:x}", Sha256::digest(bytes)),
        })
    }

    /// Exact bundle identity, including every reachable interface, is bound at
    /// prepare. A compatible-looking semver is not a substitute for this hash.
    pub fn require_identity(&self, version: &str, sha256: &str) -> Result<()> {
        if self.bundle.version != version || self.sha256 != sha256 {
            return Err(ProtocolError::new(
                ErrorCode::InterfaceMismatch,
                "prepare",
                "exact interface bundle mismatch",
            ));
        }
        Ok(())
    }
}

fn valid_version(value: &str) -> bool {
    let parts: Vec<_> = value.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.bytes().all(|c| c.is_ascii_digit())
                && (p.len() == 1 || !p.starts_with('0'))
        })
}

fn contains_callback(expr: &TypeExpr) -> bool {
    match expr {
        TypeExpr::Callback { .. } => true,
        TypeExpr::Record { fields } => fields.values().any(contains_callback),
        TypeExpr::List { item } | TypeExpr::Optional { item } => contains_callback(item),
        _ => false,
    }
}

fn check_type(expr: &TypeExpr, bundle: &Bundle, result: bool) -> Result<()> {
    match expr {
        TypeExpr::Value { schema } => check_schema(schema, schema, &mut BTreeSet::new()),
        TypeExpr::Object {
            interface,
            ownership,
        } => {
            if !bundle.interfaces.contains_key(interface) {
                return Err(invalid(format!("unknown object interface {interface}")));
            }
            if result && *ownership != Ownership::Scope {
                return Err(invalid("returned objects must belong to a scope"));
            }
            Ok(())
        }
        TypeExpr::Callback {
            params,
            result: callback_result,
            ownership,
        } => {
            if result || *ownership != Ownership::Borrow {
                return Err(unsupported("persistent callbacks are not implemented"));
            }
            check_type(params, bundle, false)?;
            check_type(callback_result, bundle, true)
        }
        TypeExpr::Record { fields } => {
            for (name, expr) in fields {
                if !identifier(name) {
                    return Err(invalid("invalid record field"));
                }
                check_type(expr, bundle, result)?;
            }
            Ok(())
        }
        TypeExpr::List { item } | TypeExpr::Optional { item } => check_type(item, bundle, result),
        TypeExpr::Stream { .. } => Err(unsupported("streams are not implemented")),
    }
}

const KEYWORDS: &[&str] = &[
    "$schema",
    "$defs",
    "$ref",
    "type",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "enum",
    "const",
    "oneOf",
    "minimum",
    "maximum",
    "minItems",
    "maxItems",
    "minLength",
    "maxLength",
    "title",
    "description",
];

fn check_schema(schema: &Value, root: &Value, stack: &mut BTreeSet<String>) -> Result<()> {
    let obj = schema
        .as_object()
        .ok_or_else(|| invalid("schema must be an object"))?;
    for key in obj.keys() {
        if !KEYWORDS.contains(&key.as_str()) {
            return Err(unsupported(format!("schema keyword {key} is unsupported")));
        }
    }
    if obj
        .get("$schema")
        .is_some_and(|v| v.as_str() != Some("https://json-schema.org/draft/2020-12/schema"))
    {
        return Err(unsupported("only JSON Schema 2020-12 is supported"));
    }
    for key in ["title", "description"] {
        if obj.get(key).is_some_and(|v| !v.is_string()) {
            return Err(invalid("schema annotation must be a string"));
        }
    }
    if let Some(reference) = obj.get("$ref") {
        let path = reference
            .as_str()
            .filter(|p| p.starts_with("#/$defs/"))
            .ok_or_else(|| unsupported("only local value $ref is supported"))?;
        if !stack.insert(path.into()) {
            return Err(invalid(
                "recursive value schemas are unsupported; use object references",
            ));
        }
        let target = root
            .pointer(&path[1..])
            .ok_or_else(|| invalid("missing value $ref"))?;
        check_schema(target, root, stack)?;
        stack.remove(path);
    }
    if let Some(kind) = obj.get("type") {
        if !matches!(
            kind.as_str(),
            Some("object" | "array" | "boolean" | "null" | "number" | "integer" | "string")
        ) {
            return Err(unsupported("schema type is unsupported"));
        }
    }
    for key in ["properties", "$defs"] {
        if let Some(entries) = obj.get(key) {
            for (name, entry) in entries
                .as_object()
                .ok_or_else(|| invalid("schema field map required"))?
            {
                if !identifier(name) {
                    return Err(invalid("invalid schema field"));
                }
                check_schema(entry, root, stack)?;
            }
        }
    }
    if let Some(items) = obj.get("items") {
        check_schema(items, root, stack)?;
    }
    if let Some(branches) = obj.get("oneOf") {
        let branches = branches
            .as_array()
            .filter(|b| !b.is_empty())
            .ok_or_else(|| invalid("oneOf requires nonempty branches"))?;
        for branch in branches {
            check_schema(branch, root, stack)?;
        }
        // The first release supports discriminated object unions, not general
        // overlapping alternatives. One required string tag selects a branch.
        let first = &branches[0];
        let tagged = first
            .get("properties")
            .and_then(Value::as_object)
            .is_some_and(|fields| {
                fields.keys().any(|tag| {
                    let mut constants = BTreeSet::new();
                    branches.iter().all(|branch| {
                        if branch.get("type").and_then(Value::as_str) != Some("object")
                            || !branch
                                .get("required")
                                .and_then(Value::as_array)
                                .is_some_and(|r| r.contains(&Value::String(tag.clone())))
                        {
                            return false;
                        }
                        branch
                            .get("properties")
                            .and_then(|p| p.get(tag))
                            .filter(|s| s.get("type").and_then(Value::as_str) == Some("string"))
                            .and_then(|s| s.get("const"))
                            .and_then(Value::as_str)
                            .is_some_and(|constant| constants.insert(constant))
                    })
                })
            });
        if !tagged {
            return Err(unsupported(
                "oneOf requires a distinct required string discriminant",
            ));
        }
    }
    if let Some(required) = obj.get("required") {
        let required = required
            .as_array()
            .ok_or_else(|| invalid("required must be an array"))?;
        let mut names = BTreeSet::new();
        for field in required {
            let field = field
                .as_str()
                .ok_or_else(|| invalid("required names must be strings"))?;
            if !names.insert(field) {
                return Err(invalid("duplicate required field"));
            }
            if obj.get("properties").and_then(|p| p.get(field)).is_none() {
                return Err(invalid("required field is not declared"));
            }
        }
    }
    if obj
        .get("additionalProperties")
        .is_some_and(|v| !v.is_boolean())
    {
        return Err(unsupported("additionalProperties schemas are unsupported"));
    }
    if obj
        .get("enum")
        .is_some_and(|v| !v.is_array() || v.as_array().unwrap().is_empty())
    {
        return Err(invalid("enum must be a nonempty array"));
    }
    if let Some(values) = obj.get("enum").and_then(Value::as_array) {
        let mut seen = BTreeSet::new();
        if values
            .iter()
            .any(|v| !safe_json(v) || !seen.insert(crate::json::canonical(v)))
        {
            return Err(invalid("unsafe or duplicate enum value"));
        }
    }
    if obj.get("const").is_some_and(|v| !safe_json(v)) {
        return Err(invalid("unsafe const value"));
    }
    for key in [
        "minimum",
        "maximum",
        "minItems",
        "maxItems",
        "minLength",
        "maxLength",
    ] {
        if let Some(bound) = obj.get(key) {
            let bound = bound
                .as_f64()
                .filter(|v| v.is_finite())
                .ok_or_else(|| invalid("schema bounds must be finite numbers"))?;
            if !safe_json(obj.get(key).unwrap()) {
                return Err(invalid("unsafe schema numeric bound"));
            }
            if key != "minimum" && key != "maximum" && (bound < 0.0 || bound.fract() != 0.0) {
                return Err(invalid("size bounds must be nonnegative integers"));
            }
        }
    }
    for (min, max) in [
        ("minimum", "maximum"),
        ("minItems", "maxItems"),
        ("minLength", "maxLength"),
    ] {
        if obj
            .get(min)
            .and_then(Value::as_f64)
            .zip(obj.get(max).and_then(Value::as_f64))
            .is_some_and(|(min, max)| min > max)
        {
            return Err(invalid("inverted schema bounds"));
        }
    }
    Ok(())
}

pub fn validate_json(schema: &Value, value: &Value) -> Result<()> {
    check_schema(schema, schema, &mut BTreeSet::new())?;
    validate_json_inner(schema, schema, value)
}
pub(crate) fn check_schema_for_prepare(schema: &Value) -> Result<()> {
    check_schema(schema, schema, &mut BTreeSet::new())
}

fn safe_json(value: &Value) -> bool {
    match value {
        Value::Number(n) => n.as_f64().is_some_and(|n| {
            n.is_finite() && (n.fract() != 0.0 || n.abs() <= 9_007_199_254_740_991.0)
        }),
        Value::Array(values) => values.iter().all(safe_json),
        Value::Object(fields) => fields.iter().all(|(name, v)| {
            !matches!(name.as_str(), "__proto__" | "prototype" | "constructor") && safe_json(v)
        }),
        _ => true,
    }
}

fn validate_json_inner(schema: &Value, root: &Value, value: &Value) -> Result<()> {
    if !safe_json(value) {
        return Err(invalid("unsafe numeric or reflection value"));
    }
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let target = root
            .pointer(&reference[1..])
            .ok_or_else(|| invalid("missing value $ref"))?;
        validate_json_inner(target, root, value)?;
    }
    if let Some(kind) = schema.get("type").and_then(Value::as_str) {
        let valid = match kind {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "null" => value.is_null(),
            "boolean" => value.is_boolean(),
            "number" => value.is_number(),
            "integer" => value.as_f64().is_some_and(|v| v.fract() == 0.0),
            _ => false,
        };
        if !valid {
            return Err(invalid(format!("expected {kind}")));
        }
    }
    if schema
        .get("const")
        .is_some_and(|expected| crate::json::canonical(value) != crate::json::canonical(expected))
    {
        return Err(invalid("const mismatch"));
    }
    if schema
        .get("enum")
        .and_then(Value::as_array)
        .is_some_and(|values| {
            !values
                .iter()
                .any(|v| crate::json::canonical(v) == crate::json::canonical(value))
        })
    {
        return Err(invalid("enum mismatch"));
    }
    if let Some(branches) = schema.get("oneOf").and_then(Value::as_array) {
        if branches
            .iter()
            .filter(|b| validate_json_inner(b, root, value).is_ok())
            .count()
            != 1
        {
            return Err(invalid("oneOf requires exactly one matching branch"));
        }
    }
    if let Some(fields) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for name in required {
                if !fields.contains_key(name.as_str().unwrap()) {
                    return Err(invalid("missing required field"));
                }
            }
        }
        for (name, value) in fields {
            if let Some(field_schema) = schema.get("properties").and_then(|p| p.get(name)) {
                validate_json_inner(field_schema, root, value)?;
            } else if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
                return Err(invalid("unexpected field"));
            }
        }
    }
    if let Some(items) = value.as_array() {
        bounds(schema, "minItems", "maxItems", items.len() as f64)?;
        if let Some(item_schema) = schema.get("items") {
            for item in items {
                validate_json_inner(item_schema, root, item)?;
            }
        }
    }
    if let Some(text) = value.as_str() {
        bounds(
            schema,
            "minLength",
            "maxLength",
            text.chars().count() as f64,
        )?;
    }
    if let Some(number) = value.as_f64() {
        bounds(schema, "minimum", "maximum", number)?;
    }
    Ok(())
}

fn bounds(schema: &Value, min: &str, max: &str, value: f64) -> Result<()> {
    if schema
        .get(min)
        .and_then(Value::as_f64)
        .is_some_and(|min| value < min)
        || schema
            .get(max)
            .and_then(Value::as_f64)
            .is_some_and(|max| value > max)
    {
        return Err(invalid("value outside bounds"));
    }
    Ok(())
}

/// Reference interface lookup is supplied by the authenticated delivery table,
/// not by data in an arbitrary JSON object. Callback entries use "$callback".
pub fn validate_wire(expr: &TypeExpr, value: &WireValue, references: &[String]) -> Result<()> {
    match (expr, value) {
        (TypeExpr::Value { schema }, WireValue::Value { value }) => validate_json(schema, value),
        (TypeExpr::Object { interface, .. }, WireValue::Ref { index })
            if references.get(*index) == Some(interface) =>
        {
            Ok(())
        }
        (TypeExpr::Callback { .. }, WireValue::Ref { index })
            if references.get(*index) == Some(&callback_key(expr)) =>
        {
            Ok(())
        }
        (TypeExpr::Record { fields: types }, WireValue::Record { fields })
            if types.len() == fields.len() =>
        {
            for (name, expr) in types {
                validate_wire(
                    expr,
                    fields
                        .get(name)
                        .ok_or_else(|| invalid("missing record field"))?,
                    references,
                )?;
            }
            Ok(())
        }
        (TypeExpr::List { item }, WireValue::List { items }) => {
            for value in items {
                validate_wire(item, value, references)?;
            }
            Ok(())
        }
        (TypeExpr::Optional { item }, WireValue::Optional { value }) => {
            if let Some(value) = value {
                validate_wire(item, value, references)?;
            }
            Ok(())
        }
        _ => Err(invalid(
            "wire value tag, reference interface, or structure mismatch",
        )),
    }
}
