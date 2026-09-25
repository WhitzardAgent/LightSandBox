//! Structured image errors. Codes are stable strings; platform mismatch maps
//! to the T06-C03 contract spelling HOST_UNSUPPORTED and is raised at import
//! time — never as a faked failure after exec starts.

use std::fmt;

pub type Result<T> = std::result::Result<T, ImageError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageError {
    /// oci-layout/index/manifest/config shape violation.
    LayoutInvalid { detail: String },
    /// blob bytes do not match their descriptor digest (T06-C01).
    DigestMismatch {
        kind: &'static str,
        expected: String,
        actual: String,
    },
    /// config platform cannot execute on this host (T06-C03).
    PlatformUnsupported { image: String, host: String },
    /// requested tag/ref not present in the index.
    RefNotFound { reference: String },
    /// pinned digest and resolved digest disagree (tag moved).
    PinnedDigestMismatch { pinned: String, resolved: String },
    /// pulled/incoming data failed verification during staging.
    Transport { detail: String },
    /// transfer tool not available on this host (e.g. no pinned skopeo).
    TransportUnavailable { tool: String },
    Io { detail: String },
}

impl ImageError {
    /// Machine-readable contract code.
    pub fn code(&self) -> &'static str {
        match self {
            ImageError::LayoutInvalid { .. } => "IMAGE_LAYOUT_INVALID",
            ImageError::DigestMismatch { .. } => "IMAGE_DIGEST_MISMATCH",
            ImageError::PlatformUnsupported { .. } => "HOST_UNSUPPORTED",
            ImageError::RefNotFound { .. } => "IMAGE_REF_NOT_FOUND",
            ImageError::PinnedDigestMismatch { .. } => "IMAGE_PINNED_DIGEST_MISMATCH",
            ImageError::Transport { .. } => "IMAGE_TRANSPORT_FAILED",
            ImageError::TransportUnavailable { .. } => "IMAGE_TRANSPORT_UNAVAILABLE",
            ImageError::Io { .. } => "IMAGE_IO",
        }
    }
}

impl fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImageError::LayoutInvalid { detail } => {
                write!(f, "{} {detail}", self.code())
            }
            ImageError::DigestMismatch {
                kind,
                expected,
                actual,
            } => write!(
                f,
                "{} {kind} digest mismatch: expected {expected}, actual {actual}",
                self.code()
            ),
            ImageError::PlatformUnsupported { image, host } => {
                write!(f, "{} image platform {image} cannot execute on host {host}", self.code())
            }
            ImageError::RefNotFound { reference } => {
                write!(f, "{} reference {reference:?} not in index", self.code())
            }
            ImageError::PinnedDigestMismatch { pinned, resolved } => write!(
                f,
                "{} pinned {pinned} but registry resolved {resolved}",
                self.code()
            ),
            ImageError::Transport { detail } => write!(f, "{} {detail}", self.code()),
            ImageError::TransportUnavailable { tool } => {
                write!(f, "{} transfer tool {tool} not available", self.code())
            }
            ImageError::Io { detail } => write!(f, "{} {detail}", self.code()),
        }
    }
}

impl std::error::Error for ImageError {}

impl From<std::io::Error> for ImageError {
    fn from(e: std::io::Error) -> Self {
        ImageError::Io {
            detail: format!("{e}"),
        }
    }
}

impl From<serde_json::Error> for ImageError {
    fn from(e: serde_json::Error) -> Self {
        ImageError::Io {
            detail: format!("json encode of self-generated store record failed: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_mismatch_code_is_contract_spelling() {
        let e = ImageError::PlatformUnsupported {
            image: "linux/riscv64".into(),
            host: "linux/amd64".into(),
        };
        assert_eq!(e.code(), "HOST_UNSUPPORTED");
    }

    #[test]
    fn digest_mismatch_code_is_distinct_from_platform() {
        let e = ImageError::DigestMismatch {
            kind: "layer",
            expected: "a".into(),
            actual: "b".into(),
        };
        assert_eq!(e.code(), "IMAGE_DIGEST_MISMATCH");
        assert_ne!(e.code(), "HOST_UNSUPPORTED");
    }
}
