//! Idempotency scope, canonical request digest, and replay/conflict decision
//! (T04-C02; docs/v5/03_DOMAIN.md §幂等与超时).
//!
//! Scope uniqueness = principal + API version + method + canonical route +
//! resource identity + key. Same key + same canonical body → replay the
//! original result. Same key + different body → CONFLICT; there is NO code
//! path that re-keys or re-executes. The digest covers the canonical request
//! body only — never a log line, never secret plaintext.
//!
//! SHA-256 is implemented locally (FIPS 180-4) to keep ls-core dependency-free
//! (adding a crate would touch Cargo.toml/lockfile, outside this task's
//! ownership); known-answer vectors in tests/unit/contracts/idempotency.json
//! pin correctness.

use super::error::DomainError;
use super::ids::OwnerId;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt::Write as _;

// ---------------------------------------------------------------- SHA-256

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

pub fn sha256_hex(data: &[u8]) -> String {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for block in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = String::with_capacity(64);
    for word in h {
        write!(out, "{word:08x}").expect("write to string");
    }
    out
}

// ------------------------------------------------------ canonical JSON

/// Deterministic canonical encoding: object keys sorted, no whitespace,
/// standard JSON string escaping, serde_json number formatting. Non-finite
/// floats cannot appear (serde_json refuses them); we re-guard anyway.
pub fn canonical_json(value: &Value) -> Result<String, DomainError> {
    fn write_value(v: &Value, out: &mut String) -> Result<(), DomainError> {
        match v {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Number(n) => {
                if let Some(f) = n.as_f64() {
                    if !f.is_finite() {
                        return Err(DomainError::invalid(
                            "non-finite number cannot enter a request digest",
                        ));
                    }
                }
                out.push_str(&n.to_string());
            }
            Value::String(s) => {
                out.push_str(&serde_json::to_string(s).expect("string escape is infallible"));
            }
            Value::Array(xs) => {
                out.push('[');
                for (i, x) in xs.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_value(x, out)?;
                }
                out.push(']');
            }
            Value::Object(map) => {
                let sorted: BTreeMap<&String, &Value> = map.iter().collect();
                out.push('{');
                for (i, (k, x)) in sorted.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(k).expect("string escape is infallible"));
                    out.push(':');
                    write_value(x, out)?;
                }
                out.push('}');
            }
        }
        Ok(())
    }
    let mut out = String::new();
    write_value(value, &mut out)?;
    Ok(out)
}

pub fn request_digest(body: &Value) -> Result<String, DomainError> {
    Ok(sha256_hex(canonical_json(body)?.as_bytes()))
}

// ------------------------------------------------------ scope & decision

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Method {
    Get,
    Post,
    Delete,
}

/// The uniqueness tuple of an idempotent write. All fields boundary-checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyScope {
    pub principal: OwnerId,
    pub api_version: String,
    pub method: Method,
    pub canonical_route: String,
    pub resource_identity: String,
    pub key: String,
}

impl IdempotencyScope {
    pub fn new(
        principal: OwnerId,
        api_version: &str,
        method: Method,
        canonical_route: &str,
        resource_identity: &str,
        key: &str,
    ) -> Result<Self, DomainError> {
        if api_version != "v1" {
            return Err(DomainError::invalid(format!(
                "unsupported api_version {api_version:?} (native contract is v1)"
            )));
        }
        if canonical_route.is_empty()
            || !canonical_route.starts_with("/v1/")
            || canonical_route.contains("..")
            || canonical_route.chars().any(|c| c == '\0')
        {
            return Err(DomainError::invalid(
                "canonical_route must be a rooted /v1/ route without traversal",
            ));
        }
        if key.is_empty() || key.len() > 255 || key.chars().any(|c| c == '\0') {
            return Err(DomainError::invalid(
                "idempotency key must be 1..=255 chars without NUL",
            ));
        }
        if resource_identity.len() > 256 || resource_identity.chars().any(|c| c == '\0') {
            return Err(DomainError::invalid("resource_identity too long or contains NUL"));
        }
        Ok(IdempotencyScope {
            principal,
            api_version: api_version.to_string(),
            method,
            canonical_route: canonical_route.to_string(),
            resource_identity: resource_identity.to_string(),
            key: key.to_string(),
        })
    }
}

/// A stored first-writer record for one scope tuple.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyRecord {
    pub scope: IdempotencyScope,
    pub request_digest: String,
    /// Opaque pointer to the object the accepted request minted (e.g. an
    /// operation id). Replayed verbatim on digest match.
    pub result_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayDecision {
    /// No stored record for this exact scope tuple: proceed and persist.
    Fresh,
    /// Identical scope AND digest: return the recorded result, re-execute
    /// nothing.
    Replay { result_ref: String },
}

/// The only verdicts a duplicate submission may receive. Conflict is an
/// error; the caller must NOT retry with a rewritten key.
pub fn decide(
    existing: Option<&IdempotencyRecord>,
    scope: &IdempotencyScope,
    digest: &str,
) -> Result<ReplayDecision, DomainError> {
    let Some(record) = existing else {
        return Ok(ReplayDecision::Fresh);
    };
    if record.scope != *scope {
        // Same key under a different scope tuple is a different row entirely.
        return Ok(ReplayDecision::Fresh);
    }
    if record.request_digest == digest {
        return Ok(ReplayDecision::Replay {
            result_ref: record.result_ref.clone(),
        });
    }
    Err(DomainError::new(
        super::error::ErrorCode::Conflict,
        "idempotency key reused with a different request body",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::repo_file_json;

    #[test]
    fn sha256_known_answer_vectors() {
        // KAT digests were produced by an independent python3 hashlib run;
        // the multiblock vector (200 bytes) exercises the >64B chunk loop.
        let fx = repo_file_json("tests/unit/contracts/idempotency.json");
        for kat in fx["hash_kat"].as_array().unwrap() {
            let input = kat["input"].as_str().unwrap();
            let expect = kat["sha256"].as_str().unwrap();
            assert_eq!(sha256_hex(input.as_bytes()), expect, "KAT {}", kat["name"]);
        }
    }

    #[test]
    fn canonicalisation_pairs_match_fixture() {
        let fx = repo_file_json("tests/unit/contracts/idempotency.json");
        for pair in fx["canonical_pairs"].as_array().unwrap() {
            let l = canonical_json(&pair["left"]).unwrap();
            let r = canonical_json(&pair["right"]).unwrap();
            let equal = pair["equal"].as_bool().unwrap();
            assert_eq!(l == r, equal, "canonical_pair {}", pair["name"]);
        }
    }

    fn scope_from(doc: &Value) -> IdempotencyScope {
        let method = match doc["method"].as_str().unwrap() {
            "GET" => Method::Get,
            "POST" => Method::Post,
            "DELETE" => Method::Delete,
            other => panic!("bad method {other}"),
        };
        IdempotencyScope::new(
            OwnerId::new(doc["principal"].as_str().unwrap()).unwrap(),
            doc["api_version"].as_str().unwrap(),
            method,
            doc["canonical_route"].as_str().unwrap(),
            doc["resource_identity"].as_str().unwrap(),
            doc["key"].as_str().unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn decisions_match_fixture() {
        let fx = repo_file_json("tests/unit/contracts/idempotency.json");
        for case in fx["decisions"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let existing_scope = scope_from(&case["existing"]["scope"]);
            let existing = IdempotencyRecord {
                request_digest: request_digest(&case["existing"]["body"]).unwrap(),
                result_ref: case["existing"]["result_ref"].as_str().unwrap().to_string(),
                scope: existing_scope,
            };
            let incoming_scope = scope_from(&case["incoming"]["scope"]);
            let incoming_digest = request_digest(&case["incoming"]["body"]).unwrap();
            let verdict = decide(Some(&existing), &incoming_scope, &incoming_digest);
            match case["expect"].as_str().unwrap() {
                "replay" => {
                    let d = verdict.unwrap_or_else(|e| panic!("{name}: {e}"));
                    assert_eq!(
                        d,
                        ReplayDecision::Replay {
                            result_ref: case["expect_ref"].as_str().unwrap().to_string()
                        },
                        "{name}"
                    );
                }
                "fresh" => assert_eq!(verdict.unwrap(), ReplayDecision::Fresh, "{name}"),
                "conflict" => {
                    let err = verdict.unwrap_err();
                    assert_eq!(err.code, super::super::error::ErrorCode::Conflict, "{name}");
                }
                other => panic!("bad expectation {other}"),
            }
        }
    }

    #[test]
    fn no_record_is_fresh() {
        let scope = IdempotencyScope::new(
            OwnerId::new("alice").unwrap(),
            "v1",
            Method::Post,
            "/v1/sandboxes",
            "",
            "K1",
        )
        .unwrap();
        assert_eq!(decide(None, &scope, "any"), Ok(ReplayDecision::Fresh));
    }

    #[test]
    fn scope_isolation_covers_method_version_and_route() {
        let base = IdempotencyScope::new(
            OwnerId::new("alice").unwrap(),
            "v1",
            Method::Post,
            "/v1/sandboxes",
            "",
            "K1",
        );
        assert!(base.is_ok());
        assert!(IdempotencyScope::new(
            OwnerId::new("alice").unwrap(),
            "v2",
            Method::Post,
            "/v1/sandboxes",
            "",
            "K1"
        )
        .is_err());
        assert!(IdempotencyScope::new(
            OwnerId::new("alice").unwrap(),
            "v1",
            Method::Post,
            "v1/sandboxes",
            "",
            "K1"
        )
        .is_err());
        assert!(IdempotencyScope::new(
            OwnerId::new("alice").unwrap(),
            "v1",
            Method::Post,
            "/v1/../v0/x",
            "",
            "K1"
        )
        .is_err());
        assert!(IdempotencyScope::new(
            OwnerId::new("alice").unwrap(),
            "v1",
            Method::Post,
            "/v1/sandboxes",
            "",
            ""
        )
        .is_err());
        assert!(IdempotencyScope::new(
            OwnerId::new("alice").unwrap(),
            "v1",
            Method::Post,
            "/v1/sandboxes",
            "",
            &"k".repeat(256)
        )
        .is_err());
    }

    #[test]
    fn digest_never_contains_secret_plaintext_of_body() {
        // The digest is a one-way hash of the canonical body; a marker secret
        // inside the body must not be recoverable from the digest string.
        let body = serde_json::json!({"token": "LSBX_SECRET_marker_001", "image": "sha256:cc"});
        let digest = request_digest(&body).unwrap();
        assert!(!digest.contains("LSBX_SECRET"));
        assert_eq!(digest.len(), 64);
    }
}
