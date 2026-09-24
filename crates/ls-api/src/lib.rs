//! ls-api: native API surface vocabulary.
//!
//! T00 scope: the stable error-code mapping shared by the future HTTP façade
//! and the CLI/SDK. No server, no mounts, no host-path access.

pub mod errors {
    /// Stable machine-facing error codes for the native v1 API.
    /// The string form is part of the cross-language contract and must not
    /// drift; see contracts/v1.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum NativeErrorCode {
        InvalidArgument,
        NotFound,
        Conflict,
        PreconditionFailed,
        Unavailable,
        Internal,
    }

    impl NativeErrorCode {
        pub const ALL: [NativeErrorCode; 6] = [
            NativeErrorCode::InvalidArgument,
            NativeErrorCode::NotFound,
            NativeErrorCode::Conflict,
            NativeErrorCode::PreconditionFailed,
            NativeErrorCode::Unavailable,
            NativeErrorCode::Internal,
        ];

        /// Stable wire string; client tests pin these exact values.
        pub fn as_str(self) -> &'static str {
            match self {
                NativeErrorCode::InvalidArgument => "INVALID_ARGUMENT",
                NativeErrorCode::NotFound => "NOT_FOUND",
                NativeErrorCode::Conflict => "CONFLICT",
                NativeErrorCode::PreconditionFailed => "PRECONDITION_FAILED",
                NativeErrorCode::Unavailable => "UNAVAILABLE",
                NativeErrorCode::Internal => "INTERNAL",
            }
        }

        pub fn http_status(self) -> u16 {
            match self {
                NativeErrorCode::InvalidArgument => 400,
                NativeErrorCode::NotFound => 404,
                NativeErrorCode::Conflict => 409,
                NativeErrorCode::PreconditionFailed => 412,
                NativeErrorCode::Unavailable => 503,
                NativeErrorCode::Internal => 500,
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn wire_strings_are_stable_and_unique() {
            let mut seen = std::collections::HashSet::new();
            for code in NativeErrorCode::ALL {
                assert!(seen.insert(code.as_str()), "duplicate wire string {}", code.as_str());
            }
            assert_eq!(NativeErrorCode::Conflict.as_str(), "CONFLICT");
            assert_eq!(NativeErrorCode::PreconditionFailed.as_str(), "PRECONDITION_FAILED");
        }

        #[test]
        fn http_status_mapping_is_exact() {
            let expected = [
                (NativeErrorCode::InvalidArgument, 400),
                (NativeErrorCode::NotFound, 404),
                (NativeErrorCode::Conflict, 409),
                (NativeErrorCode::PreconditionFailed, 412),
                (NativeErrorCode::Unavailable, 503),
                (NativeErrorCode::Internal, 500),
            ];
            for (code, status) in expected {
                assert_eq!(code.http_status(), status, "{code:?}");
            }
        }

        #[test]
        fn every_variant_has_both_mappings() {
            for code in NativeErrorCode::ALL {
                assert!(!code.as_str().is_empty());
                assert!(code.http_status() >= 400);
            }
        }
    }
}
