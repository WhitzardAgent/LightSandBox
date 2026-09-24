//! Closed set of domain error codes locked to `contracts/v1/error.schema.json`.
//!
//! Every domain failure carries one of these codes; the HTTP façade maps them
//! via `http_status()`. A code outside the frozen wire enum can never be
//! constructed here — the lock test below proves the two sets stay identical.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    InvalidArgument,
    AuthenticationRequired,
    Forbidden,
    NotFound,
    Conflict,
    StaleGeneration,
    CapabilityUnavailable,
    HostCapabilityUnavailable,
    PolicyUnsupported,
    ResourceExhausted,
    InternalFailure,
    Unavailable,
}

impl ErrorCode {
    pub const ALL: [ErrorCode; 12] = [
        ErrorCode::InvalidArgument,
        ErrorCode::AuthenticationRequired,
        ErrorCode::Forbidden,
        ErrorCode::NotFound,
        ErrorCode::Conflict,
        ErrorCode::StaleGeneration,
        ErrorCode::CapabilityUnavailable,
        ErrorCode::HostCapabilityUnavailable,
        ErrorCode::PolicyUnsupported,
        ErrorCode::ResourceExhausted,
        ErrorCode::InternalFailure,
        ErrorCode::Unavailable,
    ];

    /// Exact wire string from the frozen error contract.
    pub fn wire(self) -> &'static str {
        match self {
            ErrorCode::InvalidArgument => "INVALID_ARGUMENT",
            ErrorCode::AuthenticationRequired => "AUTHENTICATION_REQUIRED",
            ErrorCode::Forbidden => "FORBIDDEN",
            ErrorCode::NotFound => "NOT_FOUND",
            ErrorCode::Conflict => "CONFLICT",
            ErrorCode::StaleGeneration => "STALE_GENERATION",
            ErrorCode::CapabilityUnavailable => "CAPABILITY_UNAVAILABLE",
            ErrorCode::HostCapabilityUnavailable => "HOST_CAPABILITY_UNAVAILABLE",
            ErrorCode::PolicyUnsupported => "POLICY_UNSUPPORTED",
            ErrorCode::ResourceExhausted => "RESOURCE_EXHAUSTED",
            ErrorCode::InternalFailure => "INTERNAL_FAILURE",
            ErrorCode::Unavailable => "UNAVAILABLE",
        }
    }

    /// Frozen mapping from docs/v5/06_HTTP.md §统一错误.
    pub fn http_status(self) -> u16 {
        match self {
            ErrorCode::InvalidArgument => 400,
            ErrorCode::AuthenticationRequired => 401,
            ErrorCode::Forbidden => 403,
            ErrorCode::NotFound => 404,
            ErrorCode::Conflict | ErrorCode::StaleGeneration => 409,
            ErrorCode::CapabilityUnavailable
            | ErrorCode::HostCapabilityUnavailable
            | ErrorCode::PolicyUnsupported => 412,
            ErrorCode::ResourceExhausted => 429,
            ErrorCode::InternalFailure => 500,
            ErrorCode::Unavailable => 503,
        }
    }
}

/// Domain failure. Never carries secret material; messages are operator-facing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainError {
    pub code: ErrorCode,
    pub message: String,
}

impl DomainError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        DomainError {
            code,
            message: message.into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidArgument, message)
    }
}

impl fmt::Display for DomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.wire(), self.message)
    }
}

impl std::error::Error for DomainError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::fixture_json;
    use std::collections::BTreeSet;

    #[test]
    fn wire_codes_match_frozen_schema_exactly() {
        let schema = fixture_json("error.schema.json");
        let frozen: BTreeSet<String> = schema["properties"]["error"]["properties"]["code"]["enum"]
            .as_array()
            .expect("schema enum")
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        let ours: BTreeSet<String> = ErrorCode::ALL.iter().map(|c| c.wire().to_string()).collect();
        assert_eq!(ours, frozen, "domain codes drifted from error.schema.json");
    }

    #[test]
    fn http_mapping_pins_documented_statuses() {
        assert_eq!(ErrorCode::InvalidArgument.http_status(), 400);
        assert_eq!(ErrorCode::AuthenticationRequired.http_status(), 401);
        assert_eq!(ErrorCode::Forbidden.http_status(), 403);
        assert_eq!(ErrorCode::NotFound.http_status(), 404);
        assert_eq!(ErrorCode::Conflict.http_status(), 409);
        assert_eq!(ErrorCode::StaleGeneration.http_status(), 409);
        assert_eq!(ErrorCode::HostCapabilityUnavailable.http_status(), 412);
        assert_eq!(ErrorCode::PolicyUnsupported.http_status(), 412);
        assert_eq!(ErrorCode::ResourceExhausted.http_status(), 429);
        assert_eq!(ErrorCode::InternalFailure.http_status(), 500);
        assert_eq!(ErrorCode::Unavailable.http_status(), 503);
    }

    #[test]
    fn serde_uses_wire_names() {
        let json = serde_json::to_string(&ErrorCode::StaleGeneration).unwrap();
        assert_eq!(json, "\"STALE_GENERATION\"");
        let back: ErrorCode = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ErrorCode::StaleGeneration);
    }
}
