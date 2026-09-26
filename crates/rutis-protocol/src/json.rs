//! Bounded scalar-Unicode JSON for descriptors and frames, without overwriting
//! duplicate keys (including differently escaped spellings of the same key).
use crate::error::{ErrorCode, ProtocolError, Result};
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};
use std::fmt;

pub const MAX_JSON_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_JSON_DEPTH: usize = 64;
const SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

pub fn decode(bytes: &[u8]) -> Result<Value> {
    let invalid = |message| ProtocolError::new(ErrorCode::InvalidParams, "decode", message);
    if bytes.len() > MAX_JSON_BYTES {
        return Err(invalid("JSON exceeds decoder byte bound".into()));
    }
    let mut parser = serde_json::Deserializer::from_slice(bytes);
    let value = Json { depth: 0 }
        .deserialize(&mut parser)
        .map_err(|e| invalid(e.to_string()))?;
    parser.end().map_err(|e| invalid(e.to_string()))?;
    Ok(value)
}

struct Json {
    depth: usize,
}
impl Json {
    fn child(&self) -> Self {
        Self {
            depth: self.depth + 1,
        }
    }
}
impl<'de> DeserializeSeed<'de> for Json {
    type Value = Value;
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> std::result::Result<Value, D::Error> {
        if self.depth > MAX_JSON_DEPTH {
            return Err(de::Error::custom("JSON exceeds decoder depth bound"));
        }
        d.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Json {
    type Value = Value;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("finite JSON with unique keys and scalar Unicode")
    }
    fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<Value, E> {
        Ok(Value::Bool(v))
    }
    fn visit_unit<E: de::Error>(self) -> std::result::Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Value, E> {
        Ok(Value::String(v.into()))
    }
    fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<Value, E> {
        Ok(Value::String(v))
    }
    fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Value, E> {
        if v.unsigned_abs() > SAFE_INTEGER as u64 {
            return Err(E::custom("large integers require decimal strings"));
        }
        Ok(Value::Number(v.into()))
    }
    fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Value, E> {
        if v > SAFE_INTEGER as u64 {
            return Err(E::custom("large integers require decimal strings"));
        }
        Ok(Value::Number(v.into()))
    }
    fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<Value, E> {
        if !v.is_finite() || (v.fract() == 0.0 && v.abs() > SAFE_INTEGER) {
            return Err(E::custom("unsafe JSON number"));
        }
        // 1, 1.0, 1e0 and -0 agree with JS and remain integral indexes.
        let number = if v.fract() == 0.0 {
            Number::from(v as i64)
        } else {
            Number::from_f64(v).ok_or_else(|| E::custom("non-finite number"))?
        };
        Ok(Value::Number(number))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = seq.next_element_seed(self.child())? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<Value, A::Error> {
        let mut values = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(de::Error::custom("duplicate JSON key"));
            }
            values.insert(key, map.next_value_seed(self.child())?);
        }
        Ok(Value::Object(values))
    }
}

/// Signature encoding independent of JSON printers and UTF-16 sorting.
/// Lengths count UTF-8 bytes; numbers are big-endian IEEE-754, with zero unified.
pub(crate) fn canonical(value: &Value) -> String {
    match value {
        Value::Null => "z".into(),
        Value::Bool(true) => "t".into(),
        Value::Bool(false) => "f".into(),
        Value::Number(n) => {
            let n = n.as_f64().expect("JSON number is finite");
            format!("n{:016x};", if n == 0.0 { 0 } else { n.to_bits() })
        }
        Value::String(s) => format!("s{}:{s}", s.len()),
        Value::Array(items) => {
            let mut encoded = format!("a{}:", items.len());
            for item in items {
                encoded.push_str(&canonical(item));
            }
            encoded
        }
        Value::Object(fields) => {
            let sorted: std::collections::BTreeMap<_, _> = fields.iter().collect();
            let mut encoded = format!("o{}:", sorted.len());
            for (key, item) in sorted {
                encoded.push_str(&canonical(&Value::String(key.clone())));
                encoded.push_str(&canonical(item));
            }
            encoded
        }
    }
}
