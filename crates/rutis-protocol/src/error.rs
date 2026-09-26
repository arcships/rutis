use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub enum ErrorCode {
    InvalidParams,
    InterfaceMismatch,
    UnsupportedCapability,
    CapabilityDenied,
    StaleObject,
    ScopeClosed,
    Cancelled,
    DeadlineExceeded,
    Unavailable,
    Business,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Execution {
    NotStarted,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, thiserror::Error)]
#[serde(deny_unknown_fields)]
#[error("{code:?} during {stage}: {message}")]
pub struct ProtocolError {
    pub code: ErrorCode,
    pub stage: String,
    pub execution: Execution,
    pub message: String,
}

impl ProtocolError {
    pub fn new(code: ErrorCode, stage: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code,
            stage: stage.into(),
            execution: Execution::NotStarted,
            message: message.into(),
        }
    }
}

pub type Result<T> = std::result::Result<T, ProtocolError>;
