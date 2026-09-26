//! Static runner metadata is inspected before executing the image. This has
//! no rustc, libstd or dylib SDK ABI requirement: the process speaks JSON IPC.
use crate::{
    contract::{FAMILY, VERSION},
    error::{ErrorCode, ProtocolError, Result},
    json,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const CATALOG_SECTION: &[u8] = b".rutis.protocol.catalog";
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CatalogService {
    pub interface: String,
    pub version: String,
    pub bundle_sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FactoryCatalog {
    pub config_sha256: String,
    pub provides: BTreeMap<String, CatalogService>,
    pub requires: BTreeMap<String, CatalogService>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerCatalog {
    pub protocol_family: String,
    pub protocol_version: String,
    pub framework_version: String,
    pub environment_sha256: String,
    pub capabilities: BTreeSet<String>,
    pub factories: BTreeMap<String, FactoryCatalog>,
}
impl RunnerCatalog {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let value = json::decode(bytes)?;
        // Set deserialization must not silently erase duplicate capabilities.
        let values = value
            .get("capabilities")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| invalid("capabilities must be a list"))?;
        if values
            .iter()
            .map(json::canonical)
            .collect::<BTreeSet<_>>()
            .len()
            != values.len()
        {
            return Err(invalid("duplicate runner capability"));
        }
        let catalog: Self = serde_json::from_value(value).map_err(|e| invalid(e.to_string()))?;
        if catalog.protocol_family != FAMILY || catalog.protocol_version != VERSION {
            return Err(ProtocolError::new(
                ErrorCode::InterfaceMismatch,
                "prepare",
                "runner protocol family/version differs",
            ));
        }
        if !crate::prepare::version(&catalog.framework_version)
            || !crate::prepare::sha256_field(&catalog.environment_sha256)
        {
            return Err(invalid("invalid runner identity"));
        }
        for capability in &catalog.capabilities {
            if !matches!(
                capability.as_str(),
                "object.scope" | "callback.borrow" | "event.parallel" | "event.serial"
            ) {
                return Err(ProtocolError::new(
                    ErrorCode::UnsupportedCapability,
                    "prepare",
                    format!("unsupported runner capability {capability}"),
                ));
            }
        }
        for (name, factory) in &catalog.factories {
            if !crate::contract::identifier(name)
                || !crate::prepare::sha256_field(&factory.config_sha256)
            {
                return Err(invalid("invalid static factory identity"));
            }
            for (name, service) in factory.provides.iter().chain(&factory.requires) {
                if !crate::contract::identifier(name)
                    || !crate::contract::identifier(&service.interface)
                    || !crate::prepare::version(&service.version)
                    || !crate::prepare::sha256_field(&service.bundle_sha256)
                {
                    return Err(invalid("invalid static factory service contract"));
                }
            }
        }
        Ok(catalog)
    }
    pub fn from_elf(image: &[u8]) -> Result<Self> {
        Self::parse(section(image, CATALOG_SECTION)?)
    }
}
fn invalid(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::InvalidParams, "prepare", message)
}

/// Emit once per statically linked Rust runner. The runtime must use the same
/// bytes for hello/registry validation, keeping the section live in release.
#[macro_export]
macro_rules! embed_runner_catalog {
    ($bytes:expr) => {
        #[used]
        #[link_section = ".rutis.protocol.catalog"]
        static RUTIS_PROTOCOL_CATALOG: [u8; $bytes.len()] = *$bytes;
        pub fn runner_catalog_bytes() -> &'static [u8] {
            std::hint::black_box(&RUTIS_PROTOCOL_CATALOG)
        }
    };
}

/// Linux ELF64 little-endian section reader. Every offset is checked, and
/// missing/ambiguous metadata is rejected rather than searching arbitrary data.
pub fn section<'a>(image: &'a [u8], wanted: &[u8]) -> Result<&'a [u8]> {
    if image.get(..7) != Some(b"\x7fELF\x02\x01\x01") {
        return Err(invalid("expected ELF64 little-endian runner"));
    }
    let read = |at: usize, width: usize| -> Result<u64> {
        let end = at
            .checked_add(width)
            .ok_or_else(|| invalid("ELF offset overflow"))?;
        let bytes = image
            .get(at..end)
            .ok_or_else(|| invalid("truncated ELF image"))?;
        let mut number = [0u8; 8];
        number[..width].copy_from_slice(bytes);
        Ok(u64::from_le_bytes(number))
    };
    let number = |at, width| -> Result<usize> {
        usize::try_from(read(at, width)?).map_err(|_| invalid("ELF offset too large"))
    };
    let table = number(40, 8)?;
    let size = number(58, 2)?;
    let count = number(60, 2)?;
    let names_index = number(62, 2)?;
    if size < 64 || count == 0 || names_index >= count {
        return Err(invalid("unsupported ELF section table"));
    }
    let end = table
        .checked_add(
            size.checked_mul(count)
                .ok_or_else(|| invalid("ELF section overflow"))?,
        )
        .ok_or_else(|| invalid("ELF section overflow"))?;
    if end > image.len() {
        return Err(invalid("truncated ELF section table"));
    }
    let entry = |index: usize| -> Result<(usize, usize, usize)> {
        let at = table + index * size;
        Ok((number(at, 4)?, number(at + 24, 8)?, number(at + 32, 8)?))
    };
    let range = |at: usize, size: usize| -> Result<&'a [u8]> {
        image
            .get(
                at..at
                    .checked_add(size)
                    .ok_or_else(|| invalid("ELF data overflow"))?,
            )
            .ok_or_else(|| invalid("truncated ELF section"))
    };
    let (_, at, length) = entry(names_index)?;
    let names = range(at, length)?;
    let mut found = None;
    for index in 0..count {
        let (name, at, length) = entry(index)?;
        let bytes = names
            .get(name..)
            .ok_or_else(|| invalid("invalid ELF section name"))?;
        let end = bytes
            .iter()
            .position(|b| *b == 0)
            .ok_or_else(|| invalid("unterminated ELF section name"))?;
        if &bytes[..end] == wanted {
            if found.is_some() {
                return Err(invalid("ambiguous runner catalog"));
            }
            found = Some(range(at, length)?);
        }
    }
    found.ok_or_else(|| invalid("runner catalog section is missing"))
}
