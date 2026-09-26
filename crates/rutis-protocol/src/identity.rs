//! All sequence numbers are decimal strings on the wire, including Rust's
//! u64 range. JavaScript must never round an epoch, activation or delivery id.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Sequence(pub u64);
impl TryFrom<String> for Sequence {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() || value.starts_with('0') || !value.bytes().all(|b| b.is_ascii_digit())
        {
            return Err("sequence must be a canonical positive decimal string".into());
        }
        value
            .parse()
            .map(Self)
            .map_err(|_| "sequence exceeds u64".into())
    }
}
impl From<Sequence> for String {
    fn from(value: Sequence) -> Self {
        value.0.to_string()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Activation {
    pub runtime: String,
    pub epoch: Sequence,
    pub activation: Sequence,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub activation: Activation,
    pub scope: Sequence,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectIdentity {
    pub owner: Activation,
    pub object: Sequence,
}

/// The source is the broker's authorization route. Equal object/interface ids
/// with different sources must not share revocation state or cached proxies.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterfaceView {
    pub interface: String,
    pub bundle_sha256: String,
    pub source: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delivery {
    pub id: Sequence,
    pub token: String,
    pub object: ObjectIdentity,
    pub recipient: Scope,
    pub view: InterfaceView,
}
impl std::fmt::Debug for Delivery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Delivery")
            .field("object", &self.object)
            .field("recipient", &self.recipient)
            .field("view", &self.view)
            .field("token", &"<redacted>")
            .finish_non_exhaustive()
    }
}
